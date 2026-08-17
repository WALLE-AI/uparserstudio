//! Pictures embedded in a legacy PowerPoint deck (MS-ODRAW).
//!
//! A `.ppt` splits one picture across two OLE streams and three record
//! layers, and all three are needed to put an image back where it belongs:
//!
//! 1. the shape that displays it carries an `OfficeArtFOPT` property `pib`,
//!    which is a **1-based index** into the deck's blip store — not an
//!    offset, not an id;
//! 2. the blip store (`OfficeArtBStoreContainer`, in the document's drawing
//!    group) holds one `OfficeArtFBSE` per picture, whose `foDelay` field is
//!    the byte offset of the picture's data;
//! 3. that offset points into the separate `Pictures` OLE stream, where the
//!    actual JPEG/PNG bytes sit behind a blip header of a size that depends
//!    on the record's own `recInstance`.
//!
//! Skipping step 1 and 2 — dumping every blip in the `Pictures` stream as an
//! unplaced document asset — is easier, but then nothing knows which slide an
//! image belongs to, and Markdown has no `![]()` to emit.
//!
//! Only bitmap blips are extracted. Metafile blips (EMF/WMF) are usually
//! deflate-compressed and would need both an inflater and a renderer to be
//! worth anything in Markdown, so they degrade to a warning.

/// `OfficeArtFOPT` property id for `pib`, the shape's blip reference.
const PROPERTY_PIB: u16 = 0x0104;

/// An extracted picture.
pub(crate) struct Picture<'a> {
    pub(crate) media_type: &'static str,
    pub(crate) bytes: &'a [u8],
}

/// The `pib` value of a shape's `OfficeArtFOPT` property table, if it has one.
///
/// The table is `count` fixed 6-byte entries followed by the complex
/// properties' variable-length data; `count` lives in the record header's
/// `recInstance`, not in the body, so it has to be passed in.
pub(crate) fn fopt_picture_index(body: &[u8], count: u16) -> Option<u32> {
    let entries = (count as usize).min(body.len() / 6);
    for index in 0..entries {
        let at = index * 6;
        let opid = u16::from_le_bytes([body[at], body[at + 1]]);
        let value = u32::from_le_bytes([body[at + 2], body[at + 3], body[at + 4], body[at + 5]]);
        // Bit 15 marks a complex property, whose `value` is a length into the
        // trailing data rather than the value itself.
        let complex = opid & 0x8000 != 0;
        if opid & 0x3FFF == PROPERTY_PIB && !complex && value != 0 {
            return Some(value);
        }
    }
    None
}

/// `foDelay` of an `OfficeArtFBSE` (MS-ODRAW §2.2.32): the offset of this
/// entry's picture inside the `Pictures` stream.
///
/// Layout up to that field: `btWin32`(1) `btMacOS`(1) `rgbUid`(16) `tag`(2)
/// `size`(4) `cRef`(4) `foDelay`(4).
pub(crate) fn fbse_picture_offset(body: &[u8]) -> Option<u32> {
    let slice = body.get(28..32)?;
    Some(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

/// Offset of the blip record embedded directly in an `OfficeArtFBSE` body:
/// the 36-byte header plus the entry's name. A deck can store its pictures
/// inline in the store instead of in the `Pictures` stream, and then
/// `foDelay` is meaningless.
fn fbse_embedded_blip_at(body: &[u8]) -> Option<usize> {
    let name_length = *body.get(33)? as usize;
    36usize.checked_add(name_length)
}

/// Decode the picture at `offset` in the `Pictures` stream.
///
/// The offset may point either at a blip record directly or at an `FBSE`
/// wrapping one, depending on the producer.
pub(crate) fn picture_at(pictures: &[u8], offset: usize) -> Option<Picture<'_>> {
    let (ver_instance, kind, body) = record_at(pictures, offset)?;
    if kind == 0xF007 {
        let inner = fbse_embedded_blip_at(body)?;
        let (ver_instance, kind, body) = record_at(body, inner)?;
        return decode_blip(ver_instance, kind, body);
    }
    decode_blip(ver_instance, kind, body)
}

fn record_at(data: &[u8], at: usize) -> Option<(u16, u16, &[u8])> {
    let header = data.get(at..at.checked_add(8)?)?;
    let ver_instance = u16::from_le_bytes([header[0], header[1]]);
    let kind = u16::from_le_bytes([header[2], header[3]]);
    let length = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;
    let start = at + 8;
    let body = data.get(start..start.checked_add(length)?)?;
    Some((ver_instance, kind, body))
}

/// Decode one blip record (MS-ODRAW §2.2.23).
///
/// The picture bytes start after one or two 16-byte UIDs — which of the two
/// is in play is encoded in `recInstance`, not in a length field — plus a
/// one-byte tag.
fn decode_blip(ver_instance: u16, kind: u16, body: &[u8]) -> Option<Picture<'_>> {
    let instance = ver_instance >> 4;
    let media_type = match kind {
        0xF01D | 0xF02A => "image/jpeg",
        0xF01E => "image/png",
        0xF029 => "image/tiff",
        _ => return None,
    };
    // The "doubled" instances carry a second UID before the data.
    let doubled = matches!(instance, 0x46B | 0x6E1 | 0x6E3 | 0x6E5 | 0x6E7);
    let start = if doubled { 32 } else { 16 } + 1;
    Some(Picture {
        media_type,
        bytes: body.get(start..)?,
    })
}

/// Whether a record type is a blip this reader cannot turn into a usable
/// image — worth a warning rather than silence, since the shape referencing
/// it will otherwise just be missing from the output.
pub(crate) fn is_unsupported_blip(kind: u16) -> bool {
    matches!(kind, 0xF01A | 0xF01B | 0xF01C | 0xF01F)
}

/// The record type at `offset`, for reporting what could not be decoded.
pub(crate) fn record_kind_at(pictures: &[u8], offset: usize) -> Option<u16> {
    record_at(pictures, offset).map(|(_, kind, _)| kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blip(kind: u16, instance: u16, uids: usize, payload: &[u8]) -> Vec<u8> {
        let mut body = vec![0u8; uids * 16];
        body.push(0); // tag
        body.extend_from_slice(payload);
        let mut out = (instance << 4).to_le_bytes().to_vec();
        out.extend_from_slice(&kind.to_le_bytes());
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&body);
        out
    }

    #[test]
    fn a_png_blip_decodes_to_its_payload() {
        let stream = blip(0xF01E, 0x6E0, 1, b"PNGDATA");
        let picture = picture_at(&stream, 0).expect("a png blip");
        assert_eq!(picture.media_type, "image/png");
        assert_eq!(picture.bytes, b"PNGDATA");
    }

    #[test]
    fn a_doubled_uid_instance_skips_both_uids() {
        // The second UID is only present for particular instances, and
        // nothing in the record's length says so: reading 16 bytes here
        // would prepend 16 bytes of UID to the image data and produce a file
        // no viewer opens.
        let stream = blip(0xF01E, 0x6E1, 2, b"PNGDATA");
        let picture = picture_at(&stream, 0).expect("a png blip");
        assert_eq!(picture.bytes, b"PNGDATA");
    }

    #[test]
    fn a_jpeg_blip_is_recognised() {
        let stream = blip(0xF01D, 0x46A, 1, b"JPEGDATA");
        assert_eq!(picture_at(&stream, 0).unwrap().media_type, "image/jpeg");
    }

    #[test]
    fn a_metafile_blip_is_reported_rather_than_decoded() {
        let stream = blip(0xF01B, 0x216, 1, b"WMFDATA");
        assert!(picture_at(&stream, 0).is_none());
        assert!(is_unsupported_blip(record_kind_at(&stream, 0).unwrap()));
    }

    #[test]
    fn a_truncated_blip_does_not_panic() {
        let mut stream = blip(0xF01E, 0x6E0, 1, b"PNGDATA");
        stream.truncate(12);
        assert!(picture_at(&stream, 0).is_none());
    }

    #[test]
    fn the_property_table_finds_pib_among_other_properties() {
        let mut body = Vec::new();
        body.extend_from_slice(&0x0180u16.to_le_bytes()); // some other property
        body.extend_from_slice(&7u32.to_le_bytes());
        body.extend_from_slice(&PROPERTY_PIB.to_le_bytes());
        body.extend_from_slice(&3u32.to_le_bytes());
        assert_eq!(fopt_picture_index(&body, 2), Some(3));
    }

    #[test]
    fn a_complex_pib_property_is_not_a_blip_index() {
        // With the complex bit set the value is a byte count into the
        // trailing data, and using it as a store index picks an unrelated
        // picture.
        let mut body = (PROPERTY_PIB | 0x8000).to_le_bytes().to_vec();
        body.extend_from_slice(&3u32.to_le_bytes());
        assert_eq!(fopt_picture_index(&body, 1), None);
    }

    #[test]
    fn a_property_count_larger_than_the_body_is_clamped() {
        let mut body = PROPERTY_PIB.to_le_bytes().to_vec();
        body.extend_from_slice(&1u32.to_le_bytes());
        assert_eq!(fopt_picture_index(&body, u16::MAX), Some(1));
    }

    #[test]
    fn fbse_reports_its_pictures_stream_offset() {
        let mut body = vec![0u8; 28];
        body.extend_from_slice(&4096u32.to_le_bytes());
        body.extend_from_slice(&[0, 0, 0, 0]);
        assert_eq!(fbse_picture_offset(&body), Some(4096));
    }
}
