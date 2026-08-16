//! Writes block-level image assets (crops populated by protocol adapters
//! into `Block.asset_bytes`) to a content-addressed `images/` directory
//! next to the source document, mirroring MinerU's own `image_writer`/
//! `img_buket_path` output convention — see `image_link_gap_report.md`
//! for the full source-level analysis of why uparser previously produced
//! no image links at all in its Markdown output.

use crate::types::{Page, ParseResult};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Default images directory for a given source document, when the caller
/// didn't override it with `--assets-dir`: `<source_dir>/<source_stem>_images/`.
/// One folder per document (not a single shared `images/` directory)
/// deliberately avoids collisions when multiple documents are parsed out
/// of the same source directory — a real scenario this session's own
/// test fixtures live in.
pub fn default_assets_dir(source_path: &str) -> PathBuf {
    let path = Path::new(source_path);
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty());
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("output");
    match dir {
        Some(dir) => dir.join(format!("{stem}_images")),
        None => PathBuf::from(format!("{stem}_images")),
    }
}

/// Write every block's pending `asset_bytes` (across every page) to
/// `assets_dir`, content-addressed by sha256 (so identical crops within
/// the same document naturally dedupe — a repeated logo/watermark image
/// only gets written once), set `asset_path` to the path relative to
/// `assets_dir`'s parent, and clear `asset_bytes`.
///
/// The directory is only created when there's actually something to
/// write — a document with no image-category blocks (the common case)
/// produces zero filesystem side effects, not an empty folder.
///
/// Returns the number of files actually written (a block whose bytes
/// hash to an already-written file doesn't count again).
pub fn write_page_assets(pages: &mut [Page], assets_dir: &Path) -> std::io::Result<usize> {
    let mut written = 0;
    let mut dir_created = false;
    let dir_name = assets_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("images")
        .to_string();

    for page in pages {
        for block in &mut page.blocks {
            let Some(bytes) = block.asset_bytes.take() else {
                continue;
            };
            if !dir_created {
                std::fs::create_dir_all(assets_dir)?;
                dir_created = true;
            }
            let mut hasher = Sha256::new();
            hasher.update(&bytes);
            let hash = format!("{:x}", hasher.finalize());
            let filename = format!("{hash}.png");
            let file_path = assets_dir.join(&filename);
            if !file_path.exists() {
                std::fs::write(&file_path, &bytes)?;
                written += 1;
            }
            block.asset_path = Some(format!("{dir_name}/{filename}"));
        }
    }
    Ok(written)
}

/// Convenience wrapper over [`write_page_assets`] for a full `ParseResult`.
pub fn write_block_assets(result: &mut ParseResult, assets_dir: &Path) -> std::io::Result<usize> {
    write_page_assets(&mut result.pages, assets_dir)
}

/// Write the embedded images of a structured document (DOCX/PPTX/ODF/EPUB/RTF)
/// and record where each one landed on its `Asset::path`.
///
/// Same content-addressing and lazy-directory-creation rules as
/// [`write_page_assets`]; the difference is that a structured document owns
/// its assets centrally (one entry per distinct image, referenced by id from
/// any number of blocks) rather than per block, so the path is recorded once
/// and the Markdown renderer resolves ids through it.
///
/// The original file extension is preserved — unlike PDF crops, which this
/// crate produces itself and therefore knows are PNG, an embedded asset can
/// be a JPEG, GIF, SVG or EMF, and renaming it `.png` would leave a file no
/// viewer can open.
#[cfg(feature = "native")]
pub fn write_document_assets(
    document: &mut uparser_document_engine::CanonicalDocument,
    assets_dir: &Path,
) -> std::io::Result<usize> {
    let mut written = 0;
    let mut dir_created = false;
    let dir_name = assets_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("images")
        .to_string();

    for asset in &mut document.assets {
        let Some(bytes) = asset.bytes.take() else {
            continue;
        };
        if !dir_created {
            std::fs::create_dir_all(assets_dir)?;
            dir_created = true;
        }
        let filename = format!("{}.{}", asset.sha256, asset_extension(&asset.media_type));
        let file_path = assets_dir.join(&filename);
        if !file_path.exists() {
            std::fs::write(&file_path, &bytes)?;
            written += 1;
        }
        asset.path = Some(format!("{dir_name}/{filename}"));
    }
    Ok(written)
}

#[cfg(feature = "native")]
fn asset_extension(media_type: &str) -> &'static str {
    match media_type {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/svg+xml" => "svg",
        "image/webp" => "webp",
        "image/tiff" => "tiff",
        "image/bmp" => "bmp",
        "image/emf" => "emf",
        "image/wmf" => "wmf",
        _ => "bin",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Block, BlockSource, CoordFrame, Geometry};

    fn image_block(bytes: Vec<u8>) -> Block {
        Block {
            geom: Geometry::Rect([0.0, 0.0, 10.0, 10.0]),
            geom_frame: CoordFrame::Page,
            bbox_px: Some([0, 0, 10, 10]),
            category_raw: "image".into(),
            category: Some("image".into()),
            reading_order: None,
            text: None,
            html: None,
            latex: None,
            spans: vec![],
            merge_hint: None,
            confidence: None,
            source: BlockSource::LayoutThenRecognize,
            error: None,
            asset_bytes: Some(bytes),
            asset_path: None,
        }
    }

    fn text_block() -> Block {
        Block {
            geom: Geometry::Rect([0.0, 0.0, 10.0, 10.0]),
            geom_frame: CoordFrame::Page,
            bbox_px: Some([0, 0, 10, 10]),
            category_raw: "text".into(),
            category: Some("text".into()),
            reading_order: None,
            text: Some("hello".into()),
            html: None,
            latex: None,
            spans: vec![],
            merge_hint: None,
            confidence: None,
            source: BlockSource::LayoutThenRecognize,
            error: None,
            asset_bytes: None,
            asset_path: None,
        }
    }

    #[test]
    fn default_assets_dir_places_folder_next_to_source_named_after_stem() {
        assert_eq!(
            default_assets_dir("/a/b/doc.pdf"),
            PathBuf::from("/a/b/doc_images")
        );
    }

    #[test]
    fn default_assets_dir_handles_a_bare_filename_with_no_directory() {
        assert_eq!(default_assets_dir("doc.pdf"), PathBuf::from("doc_images"));
    }

    #[test]
    fn write_page_assets_writes_real_bytes_and_sets_asset_path() {
        let dir = tempfile::tempdir().unwrap();
        let assets_dir = dir.path().join("doc_images");
        let mut pages = vec![Page {
            page_num: 1,
            width_px: 100,
            height_px: 100,
            blocks: vec![image_block(vec![1, 2, 3, 4])],
        }];

        let written = write_page_assets(&mut pages, &assets_dir).unwrap();
        assert_eq!(written, 1);

        let block = &pages[0].blocks[0];
        assert!(block.asset_bytes.is_none(), "bytes must be cleared");
        let path = block.asset_path.as_deref().unwrap();
        assert!(path.starts_with("doc_images/"));
        assert!(path.ends_with(".png"));

        let filename = path.strip_prefix("doc_images/").unwrap();
        let on_disk = std::fs::read(assets_dir.join(filename)).unwrap();
        assert_eq!(on_disk, vec![1, 2, 3, 4]);
    }

    #[test]
    fn write_page_assets_dedupes_identical_bytes_by_content_hash() {
        let dir = tempfile::tempdir().unwrap();
        let assets_dir = dir.path().join("doc_images");
        let mut pages = vec![Page {
            page_num: 1,
            width_px: 100,
            height_px: 100,
            blocks: vec![image_block(vec![9, 9, 9]), image_block(vec![9, 9, 9])],
        }];

        let written = write_page_assets(&mut pages, &assets_dir).unwrap();
        assert_eq!(written, 1, "identical bytes should only be written once");
        assert_eq!(pages[0].blocks[0].asset_path, pages[0].blocks[1].asset_path);
    }

    #[test]
    fn write_page_assets_does_not_create_a_directory_when_there_is_nothing_to_write() {
        let dir = tempfile::tempdir().unwrap();
        let assets_dir = dir.path().join("doc_images");
        let mut pages = vec![Page {
            page_num: 1,
            width_px: 100,
            height_px: 100,
            blocks: vec![text_block()],
        }];

        let written = write_page_assets(&mut pages, &assets_dir).unwrap();
        assert_eq!(written, 0);
        assert!(!assets_dir.exists());
        assert!(pages[0].blocks[0].asset_path.is_none());
    }

    #[test]
    fn write_block_assets_wraps_write_page_assets_for_a_full_parse_result() {
        let dir = tempfile::tempdir().unwrap();
        let assets_dir = dir.path().join("doc_images");
        let mut result = ParseResult {
            source_path: "doc.pdf".into(),
            source_sha256: "abc".into(),
            protocol: "mock".into(),
            routed_by: crate::types::RoutedBy::Explicit,
            document_profile: None,
            model_endpoint: None,
            model_name: None,
            pages: vec![Page {
                page_num: 1,
                width_px: 100,
                height_px: 100,
                blocks: vec![image_block(vec![5, 6, 7])],
            }],
            page_errors: vec![],
            capability_notes: vec![],
            warnings: vec![],
            timing: Default::default(),
        };

        let written = write_block_assets(&mut result, &assets_dir).unwrap();
        assert_eq!(written, 1);
        assert!(result.pages[0].blocks[0].asset_path.is_some());
    }
}
