use crate::package::Package;
use crate::{Asset, AssetId, DocumentError, ParseOptions, ParseWarning, WarningCode};
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub(crate) struct Relationship {
    pub target: String,
    pub kind: String,
    pub external: bool,
}

pub(crate) type Relationships = HashMap<String, Relationship>;

/// The package-level relationship part. Its `officeDocument` relationship is
/// the only authoritative way to find a package's main part: OOXML does not
/// require the conventional `word/document.xml` / `ppt/presentation.xml`
/// paths, and a `[Content_Types].xml` can name parts that do not exist.
pub(crate) const ROOT_RELATIONSHIPS_PART: &str = "_rels/.rels";

/// Relationship type suffixes. Both the transitional
/// (`http://schemas.openxmlformats.org/officeDocument/2006/relationships/...`)
/// and strict (`http://purl.oclc.org/ooxml/officeDocument/relationships/...`)
/// namespaces end with the same segment, so suffix matching covers both.
pub(crate) const REL_OFFICE_DOCUMENT: &str = "/officeDocument";
pub(crate) const REL_STYLES: &str = "/styles";
pub(crate) const REL_NUMBERING: &str = "/numbering";
pub(crate) const REL_FOOTNOTES: &str = "/footnotes";
pub(crate) const REL_ENDNOTES: &str = "/endnotes";
pub(crate) const REL_HEADER: &str = "/header";
pub(crate) const REL_FOOTER: &str = "/footer";

pub(crate) fn relationships_part(part: &str) -> Option<String> {
    match part.rsplit_once('/') {
        Some((directory, filename)) => Some(format!("{directory}/_rels/{filename}.rels")),
        // A part at the package root still has a rels part: `_rels/<name>.rels`.
        None if !part.is_empty() => Some(format!("_rels/{part}.rels")),
        None => None,
    }
}

pub(crate) fn parse_relationships(
    xml: &[u8],
    part: &str,
    options: &ParseOptions,
) -> Result<Relationships, DocumentError> {
    let mut reader = Reader::from_reader(xml);
    reader.trim_text(true);
    let mut relationships = HashMap::new();
    let mut nodes = 0usize;
    loop {
        nodes += 1;
        if nodes > options.limits.max_xml_nodes {
            return Err(DocumentError::ResourceLimit {
                limit: "max_xml_nodes",
                detail: format!("{part} contains too many XML events"),
            });
        }
        match reader.read_event() {
            Ok(Event::Start(event) | Event::Empty(event))
                if event.local_name().as_ref() == b"Relationship" =>
            {
                let Some(id) = attribute(&event, b"Id") else {
                    continue;
                };
                let Some(target) = attribute(&event, b"Target") else {
                    continue;
                };
                relationships.insert(
                    id,
                    Relationship {
                        target,
                        kind: attribute(&event, b"Type").unwrap_or_default(),
                        external: attribute(&event, b"TargetMode")
                            .is_some_and(|value| value.eq_ignore_ascii_case("external")),
                    },
                );
            }
            Ok(Event::Eof) => break,
            Err(error) => {
                return Err(DocumentError::Malformed {
                    part: Some(part.to_owned()),
                    detail: error.to_string(),
                });
            }
            _ => {}
        }
    }
    Ok(relationships)
}

pub(crate) fn load_relationships(
    package: &mut Package<'_>,
    source_part: &str,
    options: &ParseOptions,
) -> Result<Relationships, DocumentError> {
    let Some(part) = relationships_part(source_part) else {
        return Ok(HashMap::new());
    };
    package
        .read(&part)?
        .map(|xml| parse_relationships(&xml, &part, options))
        .transpose()
        .map(Option::unwrap_or_default)
}

/// Package `[Content_Types].xml`: the authoritative part → media-type map.
#[derive(Debug, Default)]
pub(crate) struct ContentTypes {
    defaults: HashMap<String, String>,
    overrides: HashMap<String, String>,
}

impl ContentTypes {
    pub(crate) fn load(
        package: &mut Package<'_>,
        options: &ParseOptions,
    ) -> Result<Self, DocumentError> {
        let Some(xml) = package.read("[Content_Types].xml")? else {
            return Ok(Self::default());
        };
        let mut reader = Reader::from_reader(xml.as_slice());
        let mut types = Self::default();
        let mut nodes = 0usize;
        loop {
            nodes += 1;
            if nodes > options.limits.max_xml_nodes {
                return Err(DocumentError::ResourceLimit {
                    limit: "max_xml_nodes",
                    detail: "[Content_Types].xml contains too many XML events".to_owned(),
                });
            }
            match reader.read_event() {
                Ok(Event::Start(event) | Event::Empty(event)) => {
                    match event.local_name().as_ref() {
                        b"Default" => {
                            if let (Some(extension), Some(content_type)) = (
                                attribute(&event, b"Extension"),
                                attribute(&event, b"ContentType"),
                            ) {
                                types
                                    .defaults
                                    .insert(extension.to_ascii_lowercase(), content_type);
                            }
                        }
                        b"Override" => {
                            if let (Some(part), Some(content_type)) = (
                                attribute(&event, b"PartName"),
                                attribute(&event, b"ContentType"),
                            ) {
                                types
                                    .overrides
                                    .insert(part.trim_start_matches('/').to_owned(), content_type);
                            }
                        }
                        _ => {}
                    }
                }
                Ok(Event::Eof) => break,
                // A malformed content-types part is recoverable: relationship
                // resolution does not depend on it.
                Err(_) => break,
                _ => {}
            }
        }
        Ok(types)
    }

    /// Override wins over the extension default, per ECMA-376 Part 2.
    pub(crate) fn for_part(&self, part: &str) -> Option<&str> {
        let part = part.trim_start_matches('/');
        if let Some(content_type) = self.overrides.get(part) {
            return Some(content_type.as_str());
        }
        let extension = part.rsplit_once('.')?.1.to_ascii_lowercase();
        self.defaults.get(&extension).map(String::as_str)
    }
}

pub(crate) fn load_root_relationships(
    package: &mut Package<'_>,
    options: &ParseOptions,
) -> Result<Relationships, DocumentError> {
    match package.read(ROOT_RELATIONSHIPS_PART)? {
        Some(xml) => parse_relationships(&xml, ROOT_RELATIONSHIPS_PART, options),
        None => Ok(HashMap::new()),
    }
}

/// The package's main part, resolved through the root `officeDocument`
/// relationship. Returns `None` when the package declares no such
/// relationship, so callers can fall back to a conventional path and warn.
pub(crate) fn main_part(root_relationships: &Relationships) -> Option<String> {
    let relationship = root_relationships.values().find(|relationship| {
        !relationship.external && relationship.kind.ends_with(REL_OFFICE_DOCUMENT)
    })?;
    // Root-relationship targets are relative to the package root, not to the
    // `_rels/` folder the rels part itself lives in.
    resolve_internal_target("", &relationship.target)
}

/// The single part related to `source_part` by a relationship type suffix.
pub(crate) fn related_part(
    source_part: &str,
    relationships: &Relationships,
    type_suffix: &str,
) -> Option<String> {
    relationships
        .values()
        .find(|relationship| {
            !relationship.external && relationship_kind_is(&relationship.kind, type_suffix)
        })
        .and_then(|relationship| resolve_internal_target(source_part, &relationship.target))
}

/// Exact type-segment match. Guards against `/slide` also matching
/// `/slideLayout` and `/slideMaster`, which share the prefix.
pub(crate) fn relationship_kind_is(kind: &str, type_suffix: &str) -> bool {
    kind.ends_with(type_suffix)
}

/// Read a relationship id (`r:id`) attribute. OOXML always namespace-prefixes
/// it, and a bare `id` attribute usually sits right next to it holding an
/// unrelated numeric id (`<p:sldId id="256" r:id="rId4"/>`), so a plain
/// local-name lookup picks the wrong one.
pub(crate) fn relationship_id(event: &BytesStart<'_>) -> Option<String> {
    let mut fallback = None;
    for attribute in event.attributes().flatten() {
        if attribute.key.local_name().as_ref() != b"id" {
            continue;
        }
        let value = String::from_utf8_lossy(attribute.value.as_ref()).into_owned();
        if attribute.key.as_ref().contains(&b':') {
            return Some(value);
        }
        fallback.get_or_insert(value);
    }
    fallback
}

pub(crate) fn resolve_internal_target(source_part: &str, target: &str) -> Option<String> {
    if target.contains("\\") || target.contains('\0') {
        return None;
    }
    let target = &percent_decode(target)?;
    if target.contains("\\") || target.contains('\0') {
        return None;
    }
    let mut components = Vec::new();
    if !target.starts_with('/') {
        let directory = source_part
            .rsplit_once('/')
            .map(|value| value.0)
            .unwrap_or("");
        components.extend(directory.split('/').filter(|value| !value.is_empty()));
    }
    for component in target.trim_start_matches('/').split('/') {
        match component {
            "" | "." => {}
            ".." => {
                components.pop()?;
            }
            value => components.push(value),
        }
    }
    (!components.is_empty()).then(|| components.join("/"))
}

/// Percent-decode a package-relative target, dropping any fragment.
///
/// Returns `None` when a decoded byte would introduce a path separator or a
/// NUL: `%2F`/`%5C` are the standard way to smuggle traversal past a decoder
/// that splits on `/` *before* decoding, so they are rejected rather than
/// normalized.
fn percent_decode(target: &str) -> Option<String> {
    let target = target.split('#').next().unwrap_or(target);
    if !target.contains('%') {
        return Some(target.to_owned());
    }
    let bytes = target.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = bytes.get(index + 1..index + 3)?;
            let value = u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()?;
            if matches!(value, b'/' | b'\\' | 0) {
                return None;
            }
            decoded.push(value);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

pub(crate) fn load_image_relationships(
    package: &mut Package<'_>,
    source_part: &str,
    relationships: &Relationships,
    options: &ParseOptions,
    assets: &mut Vec<Asset>,
    warnings: &mut Vec<ParseWarning>,
) -> Result<HashMap<String, AssetId>, DocumentError> {
    let mut image_ids = HashMap::new();
    for (relationship_id, relationship) in relationships {
        if relationship.external || !relationship.kind.ends_with("/image") {
            continue;
        }
        let Some(part) = resolve_internal_target(source_part, &relationship.target) else {
            warnings.push(ParseWarning {
                code: WarningCode::BrokenRelationship,
                part: Some(source_part.to_owned()),
                message: format!("image relationship {relationship_id} escapes the package"),
            });
            continue;
        };
        let Some(bytes) = package.read(&part)? else {
            warnings.push(ParseWarning {
                code: WarningCode::BrokenRelationship,
                part: Some(part),
                message: format!("image relationship {relationship_id} target is missing"),
            });
            continue;
        };
        if bytes.len() > options.limits.max_asset_bytes {
            warnings.push(ParseWarning {
                code: WarningCode::AssetDropped,
                part: Some(part),
                message: format!("image exceeds {} bytes", options.limits.max_asset_bytes),
            });
            continue;
        }
        let sha256 = format!("{:x}", Sha256::digest(&bytes));
        let id = format!("asset-{}", &sha256[..16]);
        image_ids.insert(relationship_id.clone(), id.clone());
        if !assets.iter().any(|asset| asset.id == id) {
            assets.push(Asset {
                id,
                media_type: media_type(&part).to_owned(),
                filename: part.rsplit('/').next().map(str::to_owned),
                byte_length: bytes.len(),
                sha256,
                path: None,
                bytes: options.include_assets.then_some(bytes),
            });
        }
    }
    Ok(image_ids)
}

pub(crate) fn attribute(event: &BytesStart<'_>, name: &[u8]) -> Option<String> {
    event
        .attributes()
        .flatten()
        .find(|attribute| attribute.key.local_name().as_ref() == name)
        .map(|attribute| String::from_utf8_lossy(attribute.value.as_ref()).into_owned())
}

fn media_type(path: &str) -> &'static str {
    match path
        .rsplit_once('.')
        .map(|value| value.1.to_ascii_lowercase())
    {
        Some(extension) if extension == "png" => "image/png",
        Some(extension) if extension == "jpg" || extension == "jpeg" => "image/jpeg",
        Some(extension) if extension == "gif" => "image/gif",
        Some(extension) if extension == "svg" => "image/svg+xml",
        Some(extension) if extension == "webp" => "image/webp",
        Some(extension) if extension == "emf" => "image/emf",
        Some(extension) if extension == "wmf" => "image/wmf",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_relative_relationship_target() {
        assert_eq!(
            resolve_internal_target("word/document.xml", "media/image1.png").as_deref(),
            Some("word/media/image1.png")
        );
        assert_eq!(
            resolve_internal_target("ppt/slides/slide1.xml", "../media/image1.png").as_deref(),
            Some("ppt/media/image1.png")
        );
    }

    #[test]
    fn rejects_target_escaping_package_root() {
        assert_eq!(
            resolve_internal_target("word/document.xml", "../../outside"),
            None
        );
    }
}
