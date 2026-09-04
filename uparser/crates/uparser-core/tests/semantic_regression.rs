//! Semantic regression net (plan O0.3).
//!
//! Every other test in this crate asserts *one* behaviour. This one asserts
//! the shape of the whole user-visible surface at once: for a matrix of real
//! inputs crossed with the parameter combinations that are supposed to be
//! semantically neutral, it records `(exit code, stable digest of stdout)`
//! and compares the whole table against a snapshot.
//!
//! It exists because the refactor it was written for (deleting the CLI-level
//! native fast path, moving the cache ahead of the `native` branch, applying
//! postprocess to `native`) touches code paths that no single assertion
//! covers, and because the specific bug it is meant to prevent —
//! `--no-cache` changing *whether* a parse succeeds — is invisible unless
//! you compare two runs of the same file.
//!
//! Volatile fields are stripped before digesting: `timing` is wall-clock,
//! and `source_path`/`source_sha256` depend on the temp directory. What
//! remains (exit code, block counts, categories, text) is the contract.

#![cfg(feature = "native")]

mod common;

use assert_cmd::Command;
use common::{csv, image_only_pdf, rtf, text_pdf, write_fixture};
use sha2::{Digest, Sha256};
use std::io::Write;

/// Parameter sets that must not change *whether* a parse succeeds, only how
/// fast it is or where its side effects land.
const NEUTRAL_PARAMS: &[(&str, &[&str])] = &[
    ("plain", &[]),
    ("no-cache", &["--no-cache"]),
    ("no-assets", &["--no-assets"]),
    ("no-cache+no-assets", &["--no-cache", "--no-assets"]),
];

fn digest_stdout(format: &str, stdout: &[u8]) -> String {
    if format != "json" {
        return short_hash(stdout);
    }
    let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(stdout) else {
        return short_hash(stdout);
    };
    if let Some(object) = value.as_object_mut() {
        // Wall-clock and absolute-path fields are not part of the contract.
        object.remove("timing");
        object.remove("source_path");
        object.remove("source_sha256");
        object.remove("document_profile");
        object.remove("route_decision");
        object.remove("preprocess_plan");
    }
    short_hash(
        serde_json::to_string(&value)
            .expect("re-serializing a parsed value cannot fail")
            .as_bytes(),
    )
}

fn short_hash(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())[..12].to_owned()
}

/// One row per (input x format x neutral parameter set).
fn run_matrix() -> Vec<String> {
    let dir = tempfile::tempdir().unwrap();
    let inputs: Vec<(&str, std::path::PathBuf)> = vec![
        (
            "text.pdf",
            write_fixture(dir.path(), "text.pdf", &text_pdf()),
        ),
        (
            "image_only.pdf",
            write_fixture(dir.path(), "image_only.pdf", &image_only_pdf()),
        ),
        ("doc.rtf", write_fixture(dir.path(), "doc.rtf", &rtf())),
        ("rows.csv", write_fixture(dir.path(), "rows.csv", &csv())),
    ];

    let mut rows = Vec::new();
    for (label, path) in &inputs {
        for format in ["json", "markdown"] {
            for (params_label, params) in NEUTRAL_PARAMS {
                // A fresh cache per parameter set, so "plain" never reads an
                // entry another row wrote — each row must stand alone.
                let cache = tempfile::tempdir().unwrap();
                let mut args = vec![
                    "parse",
                    path.to_str().unwrap(),
                    "--protocol",
                    "native",
                    "--format",
                    format,
                ];
                args.extend_from_slice(params);
                let output = Command::cargo_bin("uparser")
                    .unwrap()
                    .env("UPARSER_CACHE_DIR", cache.path())
                    .args(&args)
                    .output()
                    .unwrap();
                rows.push(format!(
                    "{label:16} {format:9} {params_label:19} exit={} out={}",
                    output.status.code().unwrap_or(-1),
                    digest_stdout(format, &output.stdout),
                ));
            }
        }
    }
    rows
}

/// The neutral parameters must not change the exit code *or* the output for
/// any input. This is the assertion that would have failed before O2.2:
/// `image_only.pdf` exited 0 with a placeholder under `--no-cache` and 1
/// without it.
#[test]
fn performance_flags_never_change_the_result() {
    let rows = run_matrix();
    let mut by_case: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    for row in &rows {
        let mut parts = row.split_whitespace();
        let input = parts.next().unwrap().to_owned();
        let format = parts.next().unwrap().to_owned();
        let _params = parts.next();
        let rest: Vec<&str> = parts.collect();
        by_case
            .entry(format!("{input} {format}"))
            .or_default()
            .push(rest.join(" "));
    }
    for (case, results) in by_case {
        let first = &results[0];
        assert!(
            results.iter().all(|r| r == first),
            "`{case}` differs across semantically neutral flags:\n{results:#?}"
        );
    }
}

/// Full matrix snapshot: any change to exit codes or output content for any
/// (input, format) pair shows up as a reviewable diff rather than silently.
#[test]
fn semantic_matrix_snapshot() {
    let rows = run_matrix();
    insta::assert_snapshot!(rows.join("\n"));
}

/// `native` has no OCR, so an image-only PDF is a capability boundary, not a
/// parse failure to paper over. Before O2.2 this depended on `--no-cache`.
#[test]
fn image_only_pdf_is_refused_not_stubbed() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_fixture(dir.path(), "image_only.pdf", &image_only_pdf());
    for params in [vec![], vec!["--no-cache"]] {
        let cache = tempfile::tempdir().unwrap();
        let mut args = vec![
            "parse",
            path.to_str().unwrap(),
            "--protocol",
            "native",
            "--format",
            "markdown",
        ];
        args.extend_from_slice(&params);
        let output = Command::cargo_bin("uparser")
            .unwrap()
            .env("UPARSER_CACHE_DIR", cache.path())
            .args(&args)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1), "params={params:?}");
        assert!(
            !String::from_utf8_lossy(&output.stdout).contains("Image-only PDF"),
            "the placeholder must not come back"
        );
    }
}

/// O3: a second identical run is served from the cache and produces the
/// same bytes. `native` was excluded from the cache entirely before O3.2.
#[test]
fn native_second_run_hits_the_cache_with_identical_output() {
    let dir = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let path = write_fixture(dir.path(), "rows.csv", &csv());

    let run = || {
        Command::cargo_bin("uparser")
            .unwrap()
            .env("UPARSER_CACHE_DIR", cache.path())
            .args([
                "parse",
                path.to_str().unwrap(),
                "--protocol",
                "native",
                "--format",
                "json",
                "--no-assets",
            ])
            .output()
            .unwrap()
    };

    let first = run();
    let second = run();
    assert!(first.status.success() && second.status.success());
    assert_eq!(
        digest_stdout("json", &first.stdout),
        digest_stdout("json", &second.stdout)
    );
    assert!(
        !String::from_utf8_lossy(&first.stderr).contains("cache: hit"),
        "the first run populates the cache"
    );
    assert!(
        String::from_utf8_lossy(&second.stderr).contains("cache: hit"),
        "the second run must be served from it, got: {}",
        String::from_utf8_lossy(&second.stderr)
    );
}

/// O3.1: a cache hit must replay *every* artifact, not just the IR — the
/// `CanonicalDocument` that `--format document-json` needs is only carried
/// by the outcome envelope introduced in that step.
#[test]
fn document_json_survives_a_cache_hit() {
    let dir = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let path = write_fixture(dir.path(), "rows.csv", &csv());

    let run = || {
        Command::cargo_bin("uparser")
            .unwrap()
            .env("UPARSER_CACHE_DIR", cache.path())
            .args([
                "parse",
                path.to_str().unwrap(),
                "--protocol",
                "native",
                "--format",
                "document-json",
                "--no-assets",
            ])
            .output()
            .unwrap()
    };

    let first = run();
    let second = run();
    assert!(second.status.success(), "{:?}", second.status);
    assert!(String::from_utf8_lossy(&second.stderr).contains("cache: hit"));
    assert_eq!(
        digest_stdout("json", &first.stdout),
        digest_stdout("json", &second.stdout),
        "a cache hit must not degrade document-json output"
    );
}

/// A `markdown_only` run stores no `Page`/`Block` IR, so its cache entry
/// must never be handed to a `--format json` request for the same file.
#[test]
fn markdown_only_cache_entry_is_not_served_to_a_json_request() {
    let dir = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let path = write_fixture(dir.path(), "rows.csv", &csv());

    let run = |format: &str| {
        Command::cargo_bin("uparser")
            .unwrap()
            .env("UPARSER_CACHE_DIR", cache.path())
            .args([
                "parse",
                path.to_str().unwrap(),
                "--protocol",
                "native",
                "--format",
                format,
                "--no-assets",
            ])
            .output()
            .unwrap()
    };

    assert!(run("markdown").status.success());
    let json = run("json");
    assert!(json.status.success());
    let value: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    let blocks: usize = value["pages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|page| page["blocks"].as_array().unwrap().len())
        .sum();
    assert!(blocks > 0, "the JSON request must get real IR: {value}");
}

/// O3.3: `native` now goes through the same paragraph-merge and CJK
/// punctuation normalization every other mode has had since P1, and
/// `--no-postprocess` is the documented way back to raw blocks.
#[test]
fn native_ir_is_postprocessed_unless_opted_out() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_fixture(dir.path(), "text.pdf", &text_pdf());

    let blocks = |extra: &[&str]| -> usize {
        let cache = tempfile::tempdir().unwrap();
        let mut args = vec![
            "parse",
            path.to_str().unwrap(),
            "--protocol",
            "native",
            "--format",
            "json",
            "--no-assets",
            "--no-cache",
        ];
        args.extend_from_slice(extra);
        let output = Command::cargo_bin("uparser")
            .unwrap()
            .env("UPARSER_CACHE_DIR", cache.path())
            .args(&args)
            .output()
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        value["pages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|page| page["blocks"].as_array().unwrap().len())
            .sum()
    };

    // This fixture is a single line, so merging cannot reduce it — the
    // assertion that matters is that both paths still produce real blocks
    // and that `--no-postprocess` never produces *fewer* than the default.
    let merged = blocks(&[]);
    let raw = blocks(&["--no-postprocess"]);
    assert!(merged > 0 && raw > 0);
    assert!(
        raw >= merged,
        "postprocess may only merge blocks, never add: raw={raw} merged={merged}"
    );
}

/// Structured sources have no rasterization step at all, so they must work
/// identically with and without PDFium compiled in. Kept in this file
/// because it is the one input class that crosses every mode boundary.
#[test]
fn structured_sources_produce_content_without_any_model_or_raster_step() {
    let dir = tempfile::tempdir().unwrap();
    for (name, bytes, expected) in [
        ("rows.csv", csv(), "| alpha | 42 |"),
        ("doc.rtf", rtf(), "Body paragraph text."),
    ] {
        let mut file = std::fs::File::create(dir.path().join(name)).unwrap();
        file.write_all(&bytes).unwrap();
        let cache = tempfile::tempdir().unwrap();
        Command::cargo_bin("uparser")
            .unwrap()
            .env("UPARSER_CACHE_DIR", cache.path())
            .args([
                "parse",
                dir.path().join(name).to_str().unwrap(),
                "--protocol",
                "native",
                "--format",
                "markdown",
                "--no-assets",
            ])
            .assert()
            .success()
            .stdout(predicates::str::contains(expected));
    }
}
