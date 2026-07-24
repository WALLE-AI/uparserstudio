//! Gate-G0 smoke tests: exercises the compiled `uparser` binary end to
//! end (ingest -> scheduler -> mock adapter -> render), asserting the
//! Agent-first exit-code/stdout-stderr contract from ARCHITECTURE.md §6.1.

use assert_cmd::Command;
use predicates::prelude::*;
use std::io::Write;

/// Every test spins up its own isolated cache directory via
/// `UPARSER_CACHE_DIR` (T-9.1's escape hatch) — without this, `parse`
/// would read/write the real `$HOME/.cache/uparser`, which is both a
/// side effect on the developer's real machine and a source of
/// cross-test flakiness (several tests below share the literal bytes
/// `b"fake pdf bytes"` + the same protocol, which would otherwise hash
/// to the same cache key and race/collide across parallel test threads).
fn isolated_cache_dir() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

#[test]
fn bad_args_exit_usage_error() {
    Command::cargo_bin("uparser")
        .unwrap()
        .arg("not-a-subcommand")
        .assert()
        .failure()
        .code(1);
}

#[test]
fn nonexistent_file_exits_dependency_unavailable() {
    Command::cargo_bin("uparser")
        .unwrap()
        .args(["parse", "/no/such/file.pdf", "--format", "json"])
        .assert()
        .failure()
        .code(2)
        .stdout(predicate::str::contains("\"error\""))
        .stderr(predicate::str::contains("no such file"));
}

#[test]
fn unknown_protocol_exits_usage_error() {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(b"fake pdf bytes").unwrap();

    Command::cargo_bin("uparser")
        .unwrap()
        .args([
            "parse",
            file.path().to_str().unwrap(),
            "--protocol",
            "nonexistent",
            "--format",
            "json",
        ])
        .assert()
        .failure()
        .code(1);
}

#[test]
fn mock_protocol_success_produces_valid_json_on_stdout() {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(b"fake pdf bytes").unwrap();
    let cache_dir = isolated_cache_dir();

    let output = Command::cargo_bin("uparser")
        .unwrap()
        .env("UPARSER_CACHE_DIR", cache_dir.path())
        .args([
            "parse",
            file.path().to_str().unwrap(),
            "--protocol",
            "mock",
            "--format",
            "json",
        ])
        .assert()
        .success()
        .code(0)
        .get_output()
        .stdout
        .clone();

    let parsed: serde_json::Value = serde_json::from_slice(&output).expect("stdout is valid JSON");
    assert_eq!(parsed["protocol"], "mock");
    assert_eq!(parsed["pages"].as_array().unwrap().len(), 1);
    assert!(parsed["page_errors"].as_array().unwrap().is_empty());
}

#[test]
fn classify_nonexistent_file_exits_dependency_unavailable() {
    Command::cargo_bin("uparser")
        .unwrap()
        .args(["classify", "/no/such/file.pdf"])
        .assert()
        .failure()
        .code(2)
        .stdout(predicate::str::contains("\"error\""));
}

#[test]
fn classify_produces_valid_document_profile_json() {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(b"not really a pdf, just bytes for L1 classification")
        .unwrap();

    let output = Command::cargo_bin("uparser")
        .unwrap()
        .args(["classify", file.path().to_str().unwrap()])
        .assert()
        .success()
        .code(0)
        .get_output()
        .stdout
        .clone();

    let parsed: serde_json::Value = serde_json::from_slice(&output).expect("stdout is valid JSON");
    assert!(parsed.get("kind").is_some());
    assert!(parsed.get("dominant_content").is_some());
    assert!(parsed.get("source_format").is_some());
}

/// Proves `--protocol mineru-vlm --endpoint ...` genuinely constructs and
/// dispatches through the real adapter (not silently ignored / falling
/// back to mock): pointing at a guaranteed-refused local port surfaces a
/// real connection failure as a `PageError`, isolated to that page by
/// `scheduler.rs` — exit 3 (partial failure), not a crash — and the
/// error text carries the overridden port, proving the override (not
/// the adapter's localhost:8000 default) was actually used.
#[test]
fn mineru_vlm_with_overridden_endpoint_surfaces_connection_failure_as_partial() {
    // Must be real, decodable image bytes: without the `pdfium` feature,
    // the CLI falls back to wrapping the raw file bytes as a single
    // page's PNG payload, and the adapter needs to get past
    // `image::load_from_memory` before it ever reaches the network —
    // arbitrary text bytes would fail at decode, never exercising the
    // dispatch/endpoint-override path this test is actually about.
    let png_bytes = {
        let img = image::RgbImage::from_pixel(2, 2, image::Rgb([255, 255, 255]));
        let mut out = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    };
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(&png_bytes).unwrap();
    let cache_dir = isolated_cache_dir();

    let output = Command::cargo_bin("uparser")
        .unwrap()
        .env("UPARSER_CACHE_DIR", cache_dir.path())
        .args([
            "parse",
            file.path().to_str().unwrap(),
            "--protocol",
            "mineru-vlm",
            "--endpoint",
            "http://127.0.0.1:1/v1/chat/completions",
            "--format",
            "json",
        ])
        .assert()
        .failure()
        .code(3)
        .get_output()
        .stdout
        .clone();

    let parsed: serde_json::Value = serde_json::from_slice(&output).expect("stdout is valid JSON");
    assert_eq!(parsed["protocol"], "mineru-vlm");
    let page_errors = parsed["page_errors"].as_array().unwrap();
    assert_eq!(page_errors.len(), 1);
    assert!(
        page_errors[0]["message"]
            .as_str()
            .unwrap()
            .contains("127.0.0.1:1")
    );
}

/// Without the `native` feature compiled in, `--protocol native` must
/// fail cleanly with a usage error, not panic.
#[cfg(not(feature = "native"))]
#[test]
fn native_without_feature_is_usage_error() {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(b"fake pdf bytes").unwrap();

    Command::cargo_bin("uparser")
        .unwrap()
        .args([
            "parse",
            file.path().to_str().unwrap(),
            "--protocol",
            "native",
        ])
        .assert()
        .failure()
        .code(1);
}

/// `--protocol auto` must run Profiler+Router and pick *something*
/// without crashing, logging its choice to stderr (Agent-first: stdout
/// stays result-only).
#[test]
fn auto_protocol_routes_without_crashing() {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(b"not a real pdf, just bytes for L1 routing")
        .unwrap();
    let cache_dir = isolated_cache_dir();

    let assert = Command::cargo_bin("uparser")
        .unwrap()
        .env("UPARSER_CACHE_DIR", cache_dir.path())
        .args([
            "parse",
            file.path().to_str().unwrap(),
            "--protocol",
            "auto",
            "--format",
            "json",
        ])
        .assert();

    let output = assert.get_output();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("auto: routed to"), "stderr was: {stderr}");
}

/// `--protocol pipeline --layout-endpoint ...` proves the stage-endpoint
/// override actually reaches `PipelineAdapter` (not silently ignored):
/// pointing at a guaranteed-refused port surfaces a connection failure
/// isolated to a `PageError` (exit 3), with the overridden endpoint in
/// the message — same shape as mineru-vlm's equivalent test above.
#[test]
fn pipeline_with_overridden_layout_endpoint_surfaces_connection_failure_as_partial() {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(b"fake pdf bytes").unwrap();
    let cache_dir = isolated_cache_dir();

    let output = Command::cargo_bin("uparser")
        .unwrap()
        .env("UPARSER_CACHE_DIR", cache_dir.path())
        .args([
            "parse",
            file.path().to_str().unwrap(),
            "--protocol",
            "pipeline",
            "--layout-endpoint",
            "http://127.0.0.1:1/layout",
            "--format",
            "json",
        ])
        .assert()
        .failure()
        .code(3)
        .get_output()
        .stdout
        .clone();

    let parsed: serde_json::Value = serde_json::from_slice(&output).expect("stdout is valid JSON");
    assert_eq!(parsed["protocol"], "pipeline");
    let page_errors = parsed["page_errors"].as_array().unwrap();
    assert_eq!(page_errors.len(), 1);
    assert!(
        page_errors[0]["message"]
            .as_str()
            .unwrap()
            .contains("127.0.0.1:1")
    );
}

/// `--protocol paddleocr --endpoint ...` proves the override reaches
/// `PaddleOcrAdapter` the same way it does for every other protocol.
#[test]
fn paddleocr_with_overridden_endpoint_surfaces_connection_failure_as_partial() {
    let png_bytes = {
        let img = image::RgbImage::from_pixel(2, 2, image::Rgb([255, 255, 255]));
        let mut out = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    };
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(&png_bytes).unwrap();
    let cache_dir = isolated_cache_dir();

    let output = Command::cargo_bin("uparser")
        .unwrap()
        .env("UPARSER_CACHE_DIR", cache_dir.path())
        .args([
            "parse",
            file.path().to_str().unwrap(),
            "--protocol",
            "paddleocr",
            "--endpoint",
            "http://127.0.0.1:1/ocr",
            "--format",
            "json",
        ])
        .assert()
        .failure()
        .code(3)
        .get_output()
        .stdout
        .clone();

    let parsed: serde_json::Value = serde_json::from_slice(&output).expect("stdout is valid JSON");
    assert_eq!(parsed["protocol"], "paddleocr");
    let page_errors = parsed["page_errors"].as_array().unwrap();
    assert_eq!(page_errors.len(), 1);
    assert!(
        page_errors[0]["message"]
            .as_str()
            .unwrap()
            .contains("127.0.0.1:1")
    );
}

/// §11.4: `layout`/`ocr`/`formula` have no `Local` implementation —
/// `--layout-backend local` must be a usage error, not silently ignored
/// or silently falling back to `Remote`.
#[test]
fn pipeline_local_backend_for_layout_stage_is_usage_error() {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(b"fake pdf bytes").unwrap();

    Command::cargo_bin("uparser")
        .unwrap()
        .args([
            "parse",
            file.path().to_str().unwrap(),
            "--protocol",
            "pipeline",
            "--layout-backend",
            "local",
            "--format",
            "json",
        ])
        .assert()
        .failure()
        .code(1);
}

/// T-9.2: `--stream` emits one NDJSON line per completed window instead
/// of one aggregate JSON document.
#[test]
fn stream_emits_ndjson_window_lines() {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(b"fake pdf bytes for streaming").unwrap();
    let cache_dir = isolated_cache_dir();

    let output = Command::cargo_bin("uparser")
        .unwrap()
        .env("UPARSER_CACHE_DIR", cache_dir.path())
        .args([
            "parse",
            file.path().to_str().unwrap(),
            "--protocol",
            "mock",
            "--stream",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let text = String::from_utf8(output).unwrap();
    let lines: Vec<&str> = text.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), 1, "expected exactly one NDJSON window line");
    let parsed: serde_json::Value = serde_json::from_str(lines[0]).expect("line is valid JSON");
    assert_eq!(parsed["window_pages"].as_array().unwrap().len(), 1);
    assert!(parsed["window_errors"].as_array().unwrap().is_empty());
}

/// T-9.1: a second `parse` of the same bytes/protocol/endpoint/model
/// hits the cache (logged to stderr) and returns an equivalent result;
/// `--no-cache` bypasses it and re-dispatches for real.
#[test]
fn cache_hits_on_second_identical_parse_and_no_cache_bypasses_it() {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(b"fake pdf bytes for caching").unwrap();
    let cache_dir = isolated_cache_dir();

    let first = Command::cargo_bin("uparser")
        .unwrap()
        .env("UPARSER_CACHE_DIR", cache_dir.path())
        .args([
            "parse",
            file.path().to_str().unwrap(),
            "--protocol",
            "mock",
            "--format",
            "json",
        ])
        .assert()
        .success();
    let first_stderr = String::from_utf8_lossy(&first.get_output().stderr).to_string();
    assert!(
        !first_stderr.contains("cache: hit"),
        "first run must not hit an empty cache: {first_stderr}"
    );

    let second = Command::cargo_bin("uparser")
        .unwrap()
        .env("UPARSER_CACHE_DIR", cache_dir.path())
        .args([
            "parse",
            file.path().to_str().unwrap(),
            "--protocol",
            "mock",
            "--format",
            "json",
        ])
        .assert()
        .success();
    let second_stderr = String::from_utf8_lossy(&second.get_output().stderr).to_string();
    assert!(
        second_stderr.contains("cache: hit"),
        "second identical run must hit the cache: {second_stderr}"
    );

    let bypassed = Command::cargo_bin("uparser")
        .unwrap()
        .env("UPARSER_CACHE_DIR", cache_dir.path())
        .args([
            "parse",
            file.path().to_str().unwrap(),
            "--protocol",
            "mock",
            "--format",
            "json",
            "--no-cache",
        ])
        .assert()
        .success();
    let bypassed_stderr = String::from_utf8_lossy(&bypassed.get_output().stderr).to_string();
    assert!(
        !bypassed_stderr.contains("cache: hit"),
        "--no-cache must bypass the cache even though an entry exists: {bypassed_stderr}"
    );
}

/// T-9.1: `cache stat`/`cache clear` operate on the cache directory and
/// reflect real entries written by `parse`.
#[test]
fn cache_stat_and_clear_reflect_real_entries() {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(b"fake pdf bytes for cache stat").unwrap();
    let cache_dir = isolated_cache_dir();

    Command::cargo_bin("uparser")
        .unwrap()
        .env("UPARSER_CACHE_DIR", cache_dir.path())
        .args(["parse", file.path().to_str().unwrap(), "--protocol", "mock"])
        .assert()
        .success();

    let stat_output = Command::cargo_bin("uparser")
        .unwrap()
        .env("UPARSER_CACHE_DIR", cache_dir.path())
        .args(["cache", "stat"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stats: serde_json::Value = serde_json::from_slice(&stat_output).unwrap();
    assert_eq!(stats["entries"], 1);
    assert!(stats["total_bytes"].as_u64().unwrap() > 0);

    Command::cargo_bin("uparser")
        .unwrap()
        .env("UPARSER_CACHE_DIR", cache_dir.path())
        .args(["cache", "clear"])
        .assert()
        .success();

    let stat_after_clear = Command::cargo_bin("uparser")
        .unwrap()
        .env("UPARSER_CACHE_DIR", cache_dir.path())
        .args(["cache", "stat"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stats_after: serde_json::Value = serde_json::from_slice(&stat_after_clear).unwrap();
    assert_eq!(stats_after["entries"], 0);
}

/// T-9.3: `doctor` on a protocol with no network endpoint reports a null
/// reachability rather than crashing or probing nonexistent state.
#[test]
fn doctor_mock_protocol_reports_no_endpoint_to_probe() {
    let output = Command::cargo_bin("uparser")
        .unwrap()
        .args(["doctor", "mock"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let parsed: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(parsed["protocol"], "mock");
    assert!(parsed["reachable"].is_null());
}

/// T-9.3: `doctor pipeline` reports a local resource advisory, not an
/// endpoint reachability probe.
#[test]
fn doctor_pipeline_reports_local_resource_advisory() {
    let output = Command::cargo_bin("uparser")
        .unwrap()
        .args(["doctor", "pipeline"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let parsed: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(parsed["protocol"], "pipeline");
    assert!(parsed["local_cpu_cores"].as_u64().unwrap() > 0);
    assert!(parsed.get("advice").is_some());
}

/// T-9.3: `doctor` against a refused port reports `reachable: false`
/// with a detail message, not a crash.
#[test]
fn doctor_unreachable_endpoint_reports_reachable_false() {
    let output = Command::cargo_bin("uparser")
        .unwrap()
        .args(["doctor", "mineru-vlm", "--endpoint", "http://127.0.0.1:1/x"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let parsed: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(parsed["reachable"], false);
}

/// T-9.4: `protocols` lists every built-in adapter with its declared
/// capabilities.
#[test]
fn protocols_lists_every_builtin_adapter() {
    let output = Command::cargo_bin("uparser")
        .unwrap()
        .args(["protocols"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let parsed: serde_json::Value = serde_json::from_slice(&output).unwrap();
    let names: Vec<&str> = parsed
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    for expected in [
        "mock",
        "mineru-vlm",
        "dots-ocr",
        "monkeyocr-v2",
        "paddleocr",
        "pipeline",
    ] {
        assert!(names.contains(&expected), "missing {expected} in {names:?}");
    }
}
