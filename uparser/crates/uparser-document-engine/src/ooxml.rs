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

pub(crate) fn relationships_part(part: &str) -> Option<String> {
    let (directory, filename) = part.rsplit_once('/')?;
    Some(format!("{directory}/_rels/{filename}.rels"))
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

pub(crate) fn resolve_internal_target(source_part: &str, target: &str) -> Option<String> {
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
