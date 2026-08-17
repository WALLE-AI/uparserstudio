//! `StyleTextPropAtom` and `TxMasterStyleAtom`: the two records that carry
//! PowerPoint 97-2003 text formatting.
//!
//! Both are built from the same two variable-length structures, which is why
//! they share this module:
//!
//! * `TextPFException` (MS-PPT §2.9.44) — paragraph properties, of which only
//!   the bullet flag survives into a Markdown-shaped IR.
//! * `TextCFException` (MS-PPT §2.9.19) — character properties; bold and
//!   italic survive.
//!
//! Neither structure is fixed-size or self-describing: a leading mask says
//! which fields are present, and the fields then follow **in the order the
//! spec declares them**, not in mask-bit order. Getting that order wrong does
//! not fail loudly — it silently shifts every later field, so the walk below
//! follows the declared order explicitly and each step is commented with the
//! field it skips.
//!
//! Every property is tri-state (`Option<bool>`): "absent" is not "off". A
//! run that does not specify bold inherits the master's per-level default,
//! and collapsing that to `false` early is exactly how master inheritance
//! gets lost.
//!
//! A malformed exception stops styling at that point and keeps whatever was
//! parsed before it; text extraction never depends on this module succeeding.
//!
//! The record layouts were cross-checked against `opensource/anydoc`'s own
//! reader (MIT, Sideguide Technologies Inc.), which resolves the same
//! structures.

/// One paragraph run: how many characters it covers, its outline depth, and
/// whether it carries a bullet.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ParagraphRun {
    pub(crate) count: usize,
    pub(crate) depth: u16,
    pub(crate) bullet: Option<bool>,
}

/// One character run: how many characters it covers, and its tri-state
/// emphasis.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct CharacterRun {
    pub(crate) count: usize,
    pub(crate) bold: Option<bool>,
    pub(crate) italic: Option<bool>,
}

/// One indent level's defaults from a master's `TxMasterStyleAtom`.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct MasterLevel {
    pub(crate) bullet: Option<bool>,
    pub(crate) bold: Option<bool>,
    pub(crate) italic: Option<bool>,
}

/// The styling of one text shape, as parallel run lists.
#[derive(Debug, Default)]
pub(crate) struct StyleRuns {
    pub(crate) paragraphs: Vec<ParagraphRun>,
    pub(crate) characters: Vec<CharacterRun>,
}

/// A master's per-level defaults for one text type.
pub(crate) type MasterLevels = Vec<MasterLevel>;

/// Highest indent level PowerPoint itself allows; a `cLevels` beyond it is a
/// corrupt or hostile atom rather than a deeper outline.
const MAX_INDENT_LEVELS: usize = 10;

/// Parse a `StyleTextPropAtom` (MS-PPT §2.9.85) for a shape whose text is
/// `text_len` UTF-16 code units long.
///
/// Both run arrays are self-terminating by coverage: they end once they have
/// accounted for the whole text. The paragraph array covers `text_len + 1`
/// because PowerPoint counts the paragraph mark that terminates the last
/// paragraph even though the text atom does not store it.
pub(crate) fn parse_style_text(body: &[u8], text_len: usize) -> StyleRuns {
    let mut runs = StyleRuns::default();
    let mut at = 0usize;

    let mut covered = 0usize;
    while covered <= text_len {
        let (Some(count), Some(depth)) = (get_u32(body, at), get_u16(body, at + 4)) else {
            break;
        };
        at += 6;
        let Some((bullet, next)) = skip_paragraph_exception(body, at) else {
            break;
        };
        at = next;
        let count = count as usize;
        runs.paragraphs.push(ParagraphRun {
            count,
            depth,
            bullet,
        });
        // A zero-length run would never advance `covered`.
        if count == 0 {
            break;
        }
        covered += count;
    }

    let mut covered = 0usize;
    while covered <= text_len {
        let Some(count) = get_u32(body, at) else {
            break;
        };
        at += 4;
        let Some((emphasis, next)) = skip_character_exception(body, at) else {
            break;
        };
        at = next;
        let count = count as usize;
        runs.characters.push(CharacterRun {
            count,
            bold: emphasis.bold,
            italic: emphasis.italic,
        });
        if count == 0 {
            break;
        }
        covered += count;
    }

    runs
}

/// Parse a `TxMasterStyleAtom` (MS-PPT §2.9.35) into per-indent-level
/// defaults, indexed by depth.
///
/// `instance` is the record header's `recInstance`, which is both the text
/// type the atom applies to and — for instances 5 and up — the marker that
/// each level is prefixed with its own indent level field.
pub(crate) fn parse_master_style(body: &[u8], instance: u16) -> MasterLevels {
    let Some(levels) = get_u16(body, 0) else {
        return Vec::new();
    };
    let mut at = 2usize;
    let mut out = Vec::new();
    for _ in 0..(levels as usize).min(MAX_INDENT_LEVELS) {
        if instance >= 5 {
            at += 2; // indentLevel
        }
        let Some((bullet, next)) = skip_paragraph_exception(body, at) else {
            break;
        };
        at = next;
        let Some((emphasis, next)) = skip_character_exception(body, at) else {
            break;
        };
        at = next;
        out.push(MasterLevel {
            bullet,
            bold: emphasis.bold,
            italic: emphasis.italic,
        });
    }
    out
}

/// Walk a `TextPFException`, returning its bullet state and the offset just
/// past it.
fn skip_paragraph_exception(body: &[u8], mut at: usize) -> Option<(Option<bool>, usize)> {
    let mask = get_u32(body, at)?;
    at += 4;
    let mut bullet = None;
    // The four bullet sub-flags share one 16-bit field, present when any of
    // them is set — but only `hasBullet` (bit 0) says whether the paragraph
    // *has* a bullet; the others only say the bullet has its own font, colour
    // or size.
    if mask & 0x0000_000F != 0 {
        let flags = get_u16(body, at)?;
        if mask & 0x0000_0001 != 0 {
            bullet = Some(flags & 0x0001 != 0);
        }
        at += 2;
    }
    if mask & 0x0000_0080 != 0 {
        at += 2; // bulletChar
    }
    if mask & 0x0000_0010 != 0 {
        at += 2; // bulletFontRef
    }
    if mask & 0x0000_0040 != 0 {
        at += 2; // bulletSize
    }
    if mask & 0x0000_0020 != 0 {
        at += 4; // bulletColor
    }
    if mask & 0x0000_0800 != 0 {
        at += 2; // textAlignment
    }
    if mask & 0x0000_1000 != 0 {
        at += 2; // lineSpacing
    }
    if mask & 0x0000_2000 != 0 {
        at += 2; // spaceBefore
    }
    if mask & 0x0000_4000 != 0 {
        at += 2; // spaceAfter
    }
    if mask & 0x0000_0100 != 0 {
        at += 2; // leftMargin
    }
    if mask & 0x0000_0400 != 0 {
        at += 2; // indent
    }
    if mask & 0x0000_8000 != 0 {
        at += 2; // defaultTabSize
    }
    if mask & 0x0010_0000 != 0 {
        // tabStops: a count followed by that many 4-byte stops.
        let count = get_u16(body, at)? as usize;
        at = at.checked_add(2 + count * 4)?;
    }
    if mask & 0x0001_0000 != 0 {
        at += 2; // fontAlign
    }
    // charWrap, wordWrap and overflow share one 16-bit field.
    if mask & 0x000E_0000 != 0 {
        at += 2;
    }
    if mask & 0x0020_0000 != 0 {
        at += 2; // textDirection
    }
    (at <= body.len()).then_some((bullet, at))
}

/// Tri-state emphasis from a `TextCFException`.
#[derive(Debug, Clone, Copy, Default)]
struct Emphasis {
    bold: Option<bool>,
    italic: Option<bool>,
}

/// Walk a `TextCFException`, returning its emphasis and the offset just past
/// it.
fn skip_character_exception(body: &[u8], mut at: usize) -> Option<(Emphasis, usize)> {
    let mask = get_u32(body, at)?;
    at += 4;
    let mut emphasis = Emphasis::default();
    // All the style toggles share one 16-bit field, present when any of them
    // is set. Each toggle's own mask bit says whether its value is specified,
    // which is what keeps the tri-state: an unset mask bit inherits.
    if mask & 0x0000_FFFF != 0 {
        let style = get_u16(body, at)?;
        if mask & 0x0000_0001 != 0 {
            emphasis.bold = Some(style & 0x0001 != 0);
        }
        if mask & 0x0000_0002 != 0 {
            emphasis.italic = Some(style & 0x0002 != 0);
        }
        at += 2;
    }
    if mask & 0x0001_0000 != 0 {
        at += 2; // fontRef
    }
    if mask & 0x0020_0000 != 0 {
        at += 2; // oldEAFontRef
    }
    if mask & 0x0040_0000 != 0 {
        at += 2; // ansiFontRef
    }
    if mask & 0x0080_0000 != 0 {
        at += 2; // symbolFontRef
    }
    if mask & 0x0002_0000 != 0 {
        at += 2; // fontSize
    }
    if mask & 0x0004_0000 != 0 {
        at += 4; // fontColor
    }
    if mask & 0x0008_0000 != 0 {
        at += 2; // position
    }
    (at <= body.len()).then_some((emphasis, at))
}

fn get_u16(bytes: &[u8], at: usize) -> Option<u16> {
    let slice = bytes.get(at..at.checked_add(2)?)?;
    Some(u16::from_le_bytes([slice[0], slice[1]]))
}

fn get_u32(bytes: &[u8], at: usize) -> Option<u32> {
    let slice = bytes.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `TextPFException` whose mask says "hasBullet is specified", followed
    /// by the 16-bit flags field holding it.
    fn bullet_exception(has_bullet: bool) -> Vec<u8> {
        let mut out = 0x0000_0001u32.to_le_bytes().to_vec();
        out.extend_from_slice(&(u16::from(has_bullet)).to_le_bytes());
        out
    }

    /// A `TextCFException` specifying bold and italic explicitly.
    fn emphasis_exception(bold: bool, italic: bool) -> Vec<u8> {
        let mut out = 0x0000_0003u32.to_le_bytes().to_vec();
        let style = u16::from(bold) | (u16::from(italic) << 1);
        out.extend_from_slice(&style.to_le_bytes());
        out
    }

    /// An exception that specifies nothing at all: mask zero, no fields.
    fn empty_exception() -> Vec<u8> {
        0u32.to_le_bytes().to_vec()
    }

    #[test]
    fn style_runs_cover_the_text_in_declared_order() {
        // Two paragraph runs (5 then 4 characters) and one character run.
        let mut body = Vec::new();
        body.extend_from_slice(&5u32.to_le_bytes());
        body.extend_from_slice(&0u16.to_le_bytes());
        body.extend_from_slice(&bullet_exception(true));
        body.extend_from_slice(&5u32.to_le_bytes());
        body.extend_from_slice(&1u16.to_le_bytes());
        body.extend_from_slice(&bullet_exception(false));
        body.extend_from_slice(&10u32.to_le_bytes());
        body.extend_from_slice(&emphasis_exception(true, false));

        let runs = parse_style_text(&body, 9);
        assert_eq!(runs.paragraphs.len(), 2);
        assert_eq!(runs.paragraphs[0].depth, 0);
        assert_eq!(runs.paragraphs[0].bullet, Some(true));
        assert_eq!(runs.paragraphs[1].depth, 1);
        assert_eq!(runs.paragraphs[1].bullet, Some(false));
        assert_eq!(runs.characters.len(), 1);
        assert_eq!(runs.characters[0].bold, Some(true));
        // Italic's mask bit was set with a zero value: specified as *off*,
        // which is not the same as unspecified.
        assert_eq!(runs.characters[0].italic, Some(false));
    }

    #[test]
    fn an_unspecified_property_stays_none_rather_than_false() {
        // Mask zero: the run says nothing, so every property must inherit.
        let mut body = Vec::new();
        body.extend_from_slice(&4u32.to_le_bytes());
        body.extend_from_slice(&0u16.to_le_bytes());
        body.extend_from_slice(&empty_exception());
        body.extend_from_slice(&4u32.to_le_bytes());
        body.extend_from_slice(&empty_exception());

        let runs = parse_style_text(&body, 3);
        assert_eq!(runs.paragraphs[0].bullet, None);
        assert_eq!(runs.characters[0].bold, None);
        assert_eq!(runs.characters[0].italic, None);
    }

    #[test]
    fn a_truncated_atom_keeps_the_runs_parsed_before_it() {
        let mut body = Vec::new();
        body.extend_from_slice(&3u32.to_le_bytes());
        body.extend_from_slice(&0u16.to_le_bytes());
        body.extend_from_slice(&bullet_exception(true));
        // A second run header that stops mid-mask.
        body.extend_from_slice(&3u32.to_le_bytes());
        body.extend_from_slice(&0u16.to_le_bytes());
        body.extend_from_slice(&[0x01, 0x00]);

        let runs = parse_style_text(&body, 5);
        assert_eq!(runs.paragraphs.len(), 1);
        assert_eq!(runs.paragraphs[0].bullet, Some(true));
    }

    #[test]
    fn a_zero_length_run_does_not_loop_forever() {
        let mut body = Vec::new();
        body.extend_from_slice(&0u32.to_le_bytes());
        body.extend_from_slice(&0u16.to_le_bytes());
        body.extend_from_slice(&empty_exception());
        // A run covering nothing can never satisfy the coverage loop, so the
        // parser has to stop on it explicitly.
        let runs = parse_style_text(&body, 100);
        assert_eq!(runs.paragraphs.len(), 1);
    }

    #[test]
    fn master_levels_are_indexed_by_depth() {
        let mut body = 2u16.to_le_bytes().to_vec();
        body.extend_from_slice(&bullet_exception(true));
        body.extend_from_slice(&emphasis_exception(true, false));
        body.extend_from_slice(&bullet_exception(true));
        body.extend_from_slice(&emphasis_exception(false, true));

        let levels = parse_master_style(&body, 1);
        assert_eq!(levels.len(), 2);
        assert_eq!(levels[0].bullet, Some(true));
        assert_eq!(levels[0].bold, Some(true));
        assert_eq!(levels[1].italic, Some(true));
    }

    #[test]
    fn master_levels_for_high_instances_carry_a_leading_indent_field() {
        // Instance >= 5 prefixes each level with its own indent level; a
        // reader that skips it reads the paragraph mask two bytes early and
        // silently mis-parses every level.
        let mut body = 1u16.to_le_bytes().to_vec();
        body.extend_from_slice(&0u16.to_le_bytes()); // indentLevel
        body.extend_from_slice(&bullet_exception(true));
        body.extend_from_slice(&emphasis_exception(true, true));

        let levels = parse_master_style(&body, 5);
        assert_eq!(levels.len(), 1);
        assert_eq!(levels[0].bullet, Some(true));
        assert_eq!(levels[0].bold, Some(true));
        assert_eq!(levels[0].italic, Some(true));
    }

    #[test]
    fn an_absurd_level_count_is_capped() {
        let body = u16::MAX.to_le_bytes().to_vec();
        // No level data follows, so parsing stops immediately regardless —
        // the cap is what keeps the loop itself bounded.
        assert!(parse_master_style(&body, 1).is_empty());
    }

    #[test]
    fn tab_stops_are_skipped_by_their_declared_count() {
        // masks.tabStops with three stops, then the bullet field that follows
        // it in declared order. Mis-skipping the array would read a stop as
        // the next exception's mask.
        let mut body = Vec::new();
        body.extend_from_slice(&1u32.to_le_bytes());
        body.extend_from_slice(&0u16.to_le_bytes());
        let mut exception = (0x0010_0000u32 | 0x0000_0001).to_le_bytes().to_vec();
        exception.extend_from_slice(&1u16.to_le_bytes()); // bulletFlags
        exception.extend_from_slice(&3u16.to_le_bytes()); // tabStop count
        exception.extend_from_slice(&[0u8; 12]);
        body.extend_from_slice(&exception);
        body.extend_from_slice(&1u32.to_le_bytes());
        body.extend_from_slice(&emphasis_exception(true, false));

        let runs = parse_style_text(&body, 0);
        assert_eq!(runs.paragraphs[0].bullet, Some(true));
        assert_eq!(runs.characters[0].bold, Some(true));
    }
}
