//! Golden comparison of the Markdown rendering paths (plan O5.1).
//!
//! Three renderers coexist in this repo:
//!
//! 1. `uparser_native_engine`'s own Markdown (PDF, `--markdown-source engine`)
//! 2. `uparser_document_engine::render::markdown` (structured sources,
//!    `--markdown-source engine`)
//! 3. `uparser_core::render::to_markdown` over the `Page`/`Block` IR
//!    (`--markdown-source canonical`, and every model protocol)
//!
//! O5.3 asks whether (3) can subsume (1) and (2). That question is only
//! answerable if the current difference is written down: these snapshots are
//! the before-picture. A diff here is the reviewable evidence for or against
//! switching the default, and a guard against changing one renderer while
//! believing you changed all of them.

#![cfg(feature = "native")]

mod common;

use assert_cmd::Command;
use common::{csv, rtf, text_pdf, write_fixture};

fn markdown(path: &std::path::Path, source: &str) -> String {
    let cache = tempfile::tempdir().unwrap();
    let output = Command::cargo_bin("uparser")
        .unwrap()
        .env("UPARSER_CACHE_DIR", cache.path())
        .args([
            "parse",
            path.to_str().unwrap(),
            "--protocol",
            "native",
            "--format",
            "markdown",
            "--markdown-source",
            source,
            "--no-assets",
            "--no-cache",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{source} render failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn side_by_side(label: &str, path: &std::path::Path) -> String {
    format!(
        "=== {label} · engine ===\n{}\n=== {label} · canonical ===\n{}",
        markdown(path, "engine").trim_end(),
        markdown(path, "canonical").trim_end()
    )
}

/// PDF: `engine` is the vendored native engine's Markdown — the output the
/// `0.8754` opendataloader-bench score was measured on. `canonical` is the
/// shared `Page`/`Block` renderer over the same parse.
#[test]
fn pdf_render_paths_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_fixture(dir.path(), "text.pdf", &text_pdf());
    insta::assert_snapshot!(side_by_side("text.pdf", &path));
}

/// RTF: `engine` is `document_engine::render::markdown` straight off the
/// `CanonicalDocument`; `canonical` goes through the lossy descending map
/// into `Page`/`Block` first. The heading and list markers surviving in both
/// columns is what O5.2 fixed.
#[test]
fn rtf_render_paths_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_fixture(dir.path(), "doc.rtf", &rtf());
    insta::assert_snapshot!(side_by_side("doc.rtf", &path));
}

/// CSV: the table path. `engine` produces a Markdown table from the
/// canonical `Table`; `canonical` carries the same table as embedded HTML on
/// the compatibility block.
#[test]
fn csv_render_paths_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_fixture(dir.path(), "rows.csv", &csv());
    insta::assert_snapshot!(side_by_side("rows.csv", &path));
}
