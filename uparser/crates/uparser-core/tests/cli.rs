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

fn fixture_png() -> Vec<u8> {
    let image = image::RgbImage::from_pixel(2, 2, image::Rgb([255, 255, 255]));
    let mut bytes = Vec::new();
    image::DynamicImage::ImageRgb8(image)
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .unwrap();
    bytes
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
fn help_exits_successfully_on_stdout() {
    Command::cargo_bin("uparser")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"))
        .stderr(predicate::str::is_empty());
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
    file.write_all(&fixture_png()).unwrap();

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
    file.write_all(&fixture_png()).unwrap();
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
    assert_eq!(parsed["route_decision"]["protocol"], "mock");
    assert_eq!(parsed["route_decision"]["origin"], "explicit");
    assert_eq!(parsed["preprocess_plan"]["input_channel"], "visual_pages");
}

#[test]
fn canonical_markdown_source_is_an_explicit_supported_mode() {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(&fixture_png()).unwrap();
    let cache_dir = isolated_cache_dir();

    Command::cargo_bin("uparser")
        .unwrap()
        .env("UPARSER_CACHE_DIR", cache_dir.path())
        .args([
            "parse",
            file.path().to_str().unwrap(),
            "--protocol",
            "mock",
            "--format",
            "markdown",
            "--markdown-source",
            "canonical",
            "--no-cache",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("mock page 1"));
}

#[test]
fn plan_reports_detection_profile_candidates_and_preprocessing() {
    let mut file = tempfile::Builder::new().suffix(".csv").tempfile().unwrap();
    file.write_all(b"name,age\nAlice,30\n").unwrap();

    let output = Command::cargo_bin("uparser")
        .unwrap()
        .args(["plan", file.path().to_str().unwrap()])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let parsed: serde_json::Value = serde_json::from_slice(&output).expect("plan is valid JSON");
    assert_eq!(parsed["format"]["format"], "csv");
    assert_eq!(parsed["profile"]["genre"]["primary"], "spreadsheet");
    assert!(
        parsed["plan"]["route"]["candidates"]
            .as_array()
            .is_some_and(|candidates| candidates.len() >= 3)
    );
    assert!(parsed["plan"]["preprocess"]["input_channel"].is_string());
}

#[test]
fn native_markdown_output_writes_in_process_and_keeps_stdout_empty() {
    let mut input = tempfile::Builder::new().suffix(".csv").tempfile().unwrap();
    input.write_all(b"name,value\nalpha,1\n").unwrap();
    let output_dir = tempfile::tempdir().unwrap();
    let output = output_dir.path().join("result.md");

    Command::cargo_bin("uparser")
        .unwrap()
        .args([
            "parse",
            input.path().to_str().unwrap(),
            "--mode",
            "native",
            "--format",
            "markdown",
            "--no-cache",
            "--no-assets",
            "--output",
            output.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::is_empty());

    let markdown = std::fs::read_to_string(output).unwrap();
    assert!(markdown.contains("alpha"));
    assert!(markdown.contains('1'));
}

#[test]
fn aggregate_output_rejects_streaming_mode() {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(&fixture_png()).unwrap();
    let output_dir = tempfile::tempdir().unwrap();

    Command::cargo_bin("uparser")
        .unwrap()
        .args([
            "parse",
            file.path().to_str().unwrap(),
            "--protocol",
            "mock",
            "--stream",
            "--output",
            output_dir.path().join("result.json").to_str().unwrap(),
        ])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains(
            "--output cannot be combined with --stream",
        ));
}

#[test]
fn plan_reports_mode_conflicts_and_missing_inputs_as_structured_errors() {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(&fixture_png()).unwrap();

    Command::cargo_bin("uparser")
        .unwrap()
        .args([
            "plan",
            file.path().to_str().unwrap(),
            "--mode",
            "native",
            "--protocol",
            "mock",
        ])
        .assert()
        .failure()
        .code(1)
        .stdout(predicate::str::contains("invalid_mode_selection"));

    Command::cargo_bin("uparser")
        .unwrap()
        .args(["plan", "/no/such/input.pdf"])
        .assert()
        .failure()
        .code(2)
        .stdout(predicate::str::contains("read_failed"));
}

#[cfg(feature = "native")]
fn native_pdf_fixture() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../opensource/pdf-inspector/tests/fixtures/bare_name_struct.pdf")
}

#[cfg(feature = "native")]
fn image_only_pdf_fixture() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../benchmark/opendataloader-bench/pdfs/01030000000141.pdf")
}

#[cfg(feature = "native")]
#[test]
fn native_markdown_fast_path_handles_structured_pdf_and_malformed_inputs() {
    let mut csv = tempfile::Builder::new().suffix(".csv").tempfile().unwrap();
    csv.write_all(b"name,value\nalpha,42\n").unwrap();
    Command::cargo_bin("uparser")
        .unwrap()
        .args([
            "parse",
            csv.path().to_str().unwrap(),
            "--protocol",
            "native",
            "--format",
            "markdown",
            "--no-assets",
            "--no-cache",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("| alpha | 42 |"));

    let pdf = native_pdf_fixture();
    assert!(pdf.is_file(), "missing fixture: {}", pdf.display());
    Command::cargo_bin("uparser")
        .unwrap()
        .args([
            "parse",
            pdf.to_str().unwrap(),
            "--protocol",
            "native",
            "--format",
            "markdown",
            "--no-assets",
            "--no-cache",
        ])
        .assert()
        .success()
        .stdout(predicate::str::is_empty().not());

    let mut broken = tempfile::Builder::new().suffix(".pdf").tempfile().unwrap();
    broken.write_all(b"%PDF-1.7\nnot a valid PDF").unwrap();
    Command::cargo_bin("uparser")
        .unwrap()
        .args([
            "parse",
            broken.path().to_str().unwrap(),
            "--protocol",
            "native",
            "--format",
            "markdown",
            "--no-assets",
            "--no-cache",
        ])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("Invalid PDF structure"));
}

#[cfg(feature = "native")]
#[test]
fn native_markdown_fast_path_reports_image_only_pdf_metadata() {
    let pdf = image_only_pdf_fixture();
    assert!(pdf.is_file(), "missing fixture: {}", pdf.display());

    Command::cargo_bin("uparser")
        .unwrap()
        .args([
            "parse",
            pdf.to_str().unwrap(),
            "--protocol",
            "native",
            "--format",
            "markdown",
            "--no-assets",
            "--no-cache",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "INFOGRAPHIC- 10 Things to Know about Copyright",
        ))
        .stdout(predicate::str::contains("[Image-only PDF: OCR required]"));
}

#[cfg(feature = "native")]
#[test]
fn native_markdown_fast_path_reports_image_only_pdf_without_metadata() {
    let mut bytes = std::fs::read(image_only_pdf_fixture()).unwrap();
    let title_key = b"/Title(";
    let key_start = bytes
        .windows(title_key.len())
        .position(|window| window == title_key)
        .expect("fixture Info dictionary title");
    bytes[key_start + 1] = b'X';
    let mut pdf = tempfile::Builder::new().suffix(".pdf").tempfile().unwrap();
    pdf.write_all(&bytes).unwrap();

    Command::cargo_bin("uparser")
        .unwrap()
        .args([
            "parse",
            pdf.path().to_str().unwrap(),
            "--protocol",
            "native",
            "--format",
            "markdown",
            "--no-assets",
            "--no-cache",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("[Image-only PDF: OCR required]"))
        .stdout(predicate::str::contains("INFOGRAPHIC").not());
}

#[test]
fn aggregate_output_writes_file_and_keeps_stdout_empty() {
    let mut input = tempfile::Builder::new().suffix(".png").tempfile().unwrap();
    input.write_all(&fixture_png()).unwrap();
    let output_dir = tempfile::tempdir().unwrap();
    let output = output_dir.path().join("result.md");

    Command::cargo_bin("uparser")
        .unwrap()
        .args([
            "parse",
            input.path().to_str().unwrap(),
            "--protocol",
            "mock",
            "--format",
            "markdown",
            "--no-cache",
            "--output",
            output.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::is_empty());

    assert!(
        std::fs::read_to_string(output)
            .unwrap()
            .contains("mock page 1")
    );
}

#[test]
fn output_write_failure_is_a_dependency_error() {
    let mut input = tempfile::Builder::new().suffix(".png").tempfile().unwrap();
    input.write_all(&fixture_png()).unwrap();
    let output_dir = tempfile::tempdir().unwrap();

    Command::cargo_bin("uparser")
        .unwrap()
        .args([
            "parse",
            input.path().to_str().unwrap(),
            "--protocol",
            "mock",
            "--format",
            "markdown",
            "--no-cache",
            "--output",
            output_dir.path().to_str().unwrap(),
        ])
        .assert()
        .failure()
        .code(2)
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::is_empty().not());
}

#[cfg(feature = "native")]
#[test]
fn native_pdf_rejects_document_json_and_native_flags_warn_when_ignored() {
    let pdf = native_pdf_fixture();
    assert!(pdf.is_file(), "missing fixture: {}", pdf.display());
    Command::cargo_bin("uparser")
        .unwrap()
        .args([
            "parse",
            pdf.to_str().unwrap(),
            "--protocol",
            "native",
            "--format",
            "document-json",
            "--no-cache",
        ])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains(
            "document-json is available for structured native documents",
        ));

    let mut csv = tempfile::Builder::new().suffix(".csv").tempfile().unwrap();
    csv.write_all(b"name,value\nalpha,42\n").unwrap();
    Command::cargo_bin("uparser")
        .unwrap()
        .args([
            "parse",
            csv.path().to_str().unwrap(),
            "--protocol",
            "native",
            "--format",
            "json",
            "--pages",
            "1",
            "--stream",
            "--window-size",
            "2",
            "--max-concurrency",
            "2",
            "--no-cache",
        ])
        .assert()
        .success()
        .stderr(
            predicate::str::contains("--pages has no effect")
                .and(predicate::str::contains("--stream has no effect"))
                .and(predicate::str::contains("--window-size has no effect"))
                .and(predicate::str::contains("--max-concurrency has no effect")),
        );
}

#[test]
fn unknown_content_is_rejected_before_adapter_execution() {
    let mut file = tempfile::Builder::new().suffix(".pdf").tempfile().unwrap();
    file.write_all(b"not actually a pdf").unwrap();

    Command::cargo_bin("uparser")
        .unwrap()
        .args([
            "parse",
            file.path().to_str().unwrap(),
            "--protocol",
            "mock",
            "--format",
            "json",
        ])
        .assert()
        .failure()
        .code(1)
        .stdout(predicate::str::contains("preflight_failed"));
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
    let mut file = tempfile::Builder::new().suffix(".csv").tempfile().unwrap();
    file.write_all(b"name,age\nAlice,30\n").unwrap();

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
    file.write_all(&fixture_png()).unwrap();

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
    file.write_all(&fixture_png()).unwrap();
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

#[test]
fn mode_protocol_selects_a_declared_model_protocol_for_planning() {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(&fixture_png()).unwrap();

    Command::cargo_bin("uparser")
        .unwrap()
        .args([
            "plan",
            file.path().to_str().unwrap(),
            "--mode",
            "protocol",
            "--protocol",
            "generic-vlm",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"protocol\": \"generic-vlm\""));
}

#[test]
fn mode_protocol_requires_a_concrete_model_protocol() {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(&fixture_png()).unwrap();

    Command::cargo_bin("uparser")
        .unwrap()
        .args(["parse", file.path().to_str().unwrap(), "--mode", "protocol"])
        .assert()
        .failure()
        .code(1)
        .stdout(predicate::str::contains("invalid_mode_selection"));
}

#[test]
fn mode_and_legacy_protocol_must_not_conflict() {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(&fixture_png()).unwrap();

    Command::cargo_bin("uparser")
        .unwrap()
        .args([
            "parse",
            file.path().to_str().unwrap(),
            "--mode",
            "native",
            "--protocol",
            "mock",
        ])
        .assert()
        .failure()
        .code(1)
        .stdout(predicate::str::contains("conflicts"));
}

/// `--protocol pipeline --layout-endpoint ...` proves the stage-endpoint
/// override actually reaches `PipelineAdapter` (not silently ignored):
/// pointing at a guaranteed-refused port surfaces a connection failure
/// isolated to a `PageError` (exit 3), with the overridden endpoint in
/// the message — same shape as mineru-vlm's equivalent test above.
#[test]
fn pipeline_with_overridden_layout_endpoint_surfaces_connection_failure_as_partial() {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(&fixture_png()).unwrap();
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
    file.write_all(&fixture_png()).unwrap();

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
    file.write_all(&fixture_png()).unwrap();
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
    file.write_all(&fixture_png()).unwrap();
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
    file.write_all(&fixture_png()).unwrap();
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

#[test]
fn doctor_unknown_protocol_is_a_structured_usage_error() {
    let output = Command::cargo_bin("uparser")
        .unwrap()
        .args(["doctor", "not-a-protocol"])
        .assert()
        .failure()
        .code(1)
        .get_output()
        .stdout
        .clone();

    let parsed: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(parsed["error"]["code"], "unknown_protocol");
    assert_eq!(parsed["error"]["protocol"], "not-a-protocol");
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
        "generic-vlm",
        "monkeyocr-v2",
        "paddleocr",
        "paddlex-structure",
        "pipeline",
    ] {
        assert!(names.contains(&expected), "missing {expected} in {names:?}");
    }
}

/// Proves `postprocess.rs` is genuinely wired into the real CLI `parse`
/// path (not just its own unit tests) — mock emits 2 raw mergeable
/// blocks per page; `--no-postprocess` should return them unmerged.
#[test]
fn postprocess_merges_by_default_and_no_postprocess_bypasses_it() {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(&fixture_png()).unwrap();

    let merged_cache = isolated_cache_dir();
    let merged_output = Command::cargo_bin("uparser")
        .unwrap()
        .env("UPARSER_CACHE_DIR", merged_cache.path())
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
        .get_output()
        .stdout
        .clone();
    let merged: serde_json::Value = serde_json::from_slice(&merged_output).unwrap();
    assert_eq!(merged["pages"][0]["blocks"].as_array().unwrap().len(), 1);

    let raw_cache = isolated_cache_dir();
    let raw_output = Command::cargo_bin("uparser")
        .unwrap()
        .env("UPARSER_CACHE_DIR", raw_cache.path())
        .args([
            "parse",
            file.path().to_str().unwrap(),
            "--protocol",
            "mock",
            "--format",
            "json",
            "--no-postprocess",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let raw: serde_json::Value = serde_json::from_slice(&raw_output).unwrap();
    assert_eq!(raw["pages"][0]["blocks"].as_array().unwrap().len(), 2);
}

#[test]
fn csv_input_auto_routes_to_native_document_engine() {
    let mut file = tempfile::Builder::new().suffix(".csv").tempfile().unwrap();
    file.write_all(b"Name,Age\nAlice,30\n").unwrap();
    let cache_dir = isolated_cache_dir();
    let output = Command::cargo_bin("uparser")
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
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let parsed: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(parsed["protocol"], "native:csv");
    assert_eq!(parsed["routed_by"]["kind"], "auto");
    assert_eq!(
        parsed["pages"][0]["blocks"][0]["source"],
        "structured_native"
    );
    // A table block carries its content as HTML so merged cells survive the
    // lowering; `text` is deliberately unset for it.
    assert!(
        parsed["pages"][0]["blocks"][0]["html"]
            .as_str()
            .unwrap()
            .contains("Alice")
    );
    assert_eq!(parsed["pages"][0]["blocks"][0]["reading_order"], 0);
}

#[test]
fn document_json_outputs_canonical_contract_and_rejects_non_native_protocol() {
    let mut file = tempfile::Builder::new().suffix(".csv").tempfile().unwrap();
    file.write_all(b"Name,Age\nAlice,30\n").unwrap();
    let output = Command::cargo_bin("uparser")
        .unwrap()
        .args([
            "parse",
            file.path().to_str().unwrap(),
            "--protocol",
            "native",
            "--format",
            "document-json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let parsed: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(parsed["schema_version"], "uparser.document.v1");
    assert_eq!(parsed["units"][0]["kind"], "sheet");

    Command::cargo_bin("uparser")
        .unwrap()
        .args([
            "parse",
            file.path().to_str().unwrap(),
            "--protocol",
            "mock",
            "--format",
            "document-json",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "document-json requires the native protocol",
        ));
}

/// A minimal real ZIP archive containing a `word/` entry — the
/// `file-format` crate's OOXML sniffing needs a genuinely parseable
/// zip, not just a `PK` magic-byte prefix, to classify this as DOCX.
fn minimal_docx_bytes() -> Vec<u8> {
    let mut buf = Vec::new();
    {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        writer.start_file("[Content_Types].xml", options).unwrap();
        writer
            .write_all(b"<Types><Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/></Types>")
            .unwrap();
        writer.start_file("_rels/.rels", options).unwrap();
        writer
            .write_all(b"<Relationships><Relationship Id=\"r0\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/></Relationships>")
            .unwrap();
        writer.start_file("word/document.xml", options).unwrap();
        writer
            .write_all(b"<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:body><w:p><w:r><w:t>hello</w:t></w:r></w:p></w:body></w:document>")
            .unwrap();
        writer.finish().unwrap();
    }
    buf
}

/// Proves adapter-level warnings (previously only ever reaching a bare
/// `eprintln!`) genuinely surface into `ParseResult.warnings` through
/// the real CLI `parse` path — a real (wiremock-backed) mineru-vlm
/// endpoint returns a layout box with a category not in
/// `category_map.rs`'s vocab, which should show up both as a page
/// succeeding (the block still gets extracted, just falls back to
/// `"unknown"`) and as a warning string in the JSON output, not just on
/// stderr.
#[tokio::test]
async fn unrecognized_category_warning_surfaces_in_parse_result_warnings() {
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{"message": {"content":
                "<|box_start|>0 0 500 200<|box_end|><|ref_start|>sidebar_note<|ref_end|>"
            }}]
        })))
        .mount(&server)
        .await;

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

    let output = tokio::task::spawn_blocking({
        let path = file.path().to_str().unwrap().to_string();
        let cache_dir = cache_dir.path().to_path_buf();
        let endpoint = format!("{}/v1/chat/completions", server.uri());
        move || {
            Command::cargo_bin("uparser")
                .unwrap()
                .env("UPARSER_CACHE_DIR", &cache_dir)
                .env("NO_PROXY", "127.0.0.1,localhost")
                .env("no_proxy", "127.0.0.1,localhost")
                .args([
                    "parse",
                    &path,
                    "--protocol",
                    "mineru-vlm",
                    "--endpoint",
                    &endpoint,
                    "--format",
                    "json",
                ])
                .assert()
                .success()
                .get_output()
                .stdout
                .clone()
        }
    })
    .await
    .unwrap();

    let parsed: serde_json::Value = serde_json::from_slice(&output).unwrap();
    let warnings = parsed["warnings"].as_array().unwrap();
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str().unwrap().contains("sidebar_note")),
        "expected a category-fallback warning mentioning sidebar_note, got {warnings:?}"
    );
}

/// Proves the image-asset pipeline (`image_link_gap_report.md`) end to
/// end through the real CLI binary: an "image"-category layout box
/// results in a real PNG file on disk under `--assets-dir`, referenced
/// by `asset_path` in the JSON output, with no `asset_bytes` key
/// anywhere (that field is `#[serde(skip)]` and must never leak).
#[tokio::test]
async fn image_category_block_writes_a_real_asset_file_and_references_it_in_json() {
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{"message": {"content":
                "<|box_start|>100 100 500 500<|box_end|><|ref_start|>image<|ref_end|>"
            }}]
        })))
        .mount(&server)
        .await;

    // A real 100x100 page — "image" is in mineru-vlm's SKIP_CONTENT, so
    // no stage-2 request is ever seeded/expected for it.
    let png_bytes = {
        let img = image::RgbImage::from_pixel(100, 100, image::Rgb([200, 100, 50]));
        let mut out = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    };
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(&png_bytes).unwrap();
    let cache_dir = isolated_cache_dir();
    let assets_dir = tempfile::tempdir().unwrap();

    let output = tokio::task::spawn_blocking({
        let path = file.path().to_str().unwrap().to_string();
        let cache_dir = cache_dir.path().to_path_buf();
        let assets_dir_path = assets_dir.path().to_path_buf();
        let endpoint = format!("{}/v1/chat/completions", server.uri());
        move || {
            Command::cargo_bin("uparser")
                .unwrap()
                .env("UPARSER_CACHE_DIR", &cache_dir)
                .env("NO_PROXY", "127.0.0.1,localhost")
                .env("no_proxy", "127.0.0.1,localhost")
                .args([
                    "parse",
                    &path,
                    "--protocol",
                    "mineru-vlm",
                    "--endpoint",
                    &endpoint,
                    "--format",
                    "json",
                    "--assets-dir",
                    assets_dir_path.to_str().unwrap(),
                ])
                .assert()
                .success()
                .get_output()
                .stdout
                .clone()
        }
    })
    .await
    .unwrap();

    let stdout_str = String::from_utf8(output.clone()).unwrap();
    assert!(
        !stdout_str.contains("asset_bytes"),
        "asset_bytes must never appear in JSON output: {stdout_str}"
    );

    let parsed: serde_json::Value = serde_json::from_slice(&output).unwrap();
    let block = &parsed["pages"][0]["blocks"][0];
    assert_eq!(block["category"], "image");
    let asset_path = block["asset_path"]
        .as_str()
        .expect("image block must have asset_path set");
    assert!(asset_path.ends_with(".png"));

    let filename = std::path::Path::new(asset_path)
        .file_name()
        .expect("asset_path must have a filename component");
    let on_disk_path = assets_dir.path().join(filename);
    let on_disk_bytes =
        std::fs::read(&on_disk_path).unwrap_or_else(|e| panic!("{on_disk_path:?}: {e}"));
    image::load_from_memory(&on_disk_bytes).expect("written file must be valid PNG bytes");
}

/// Proves DOCX input reaches real `normalize_format` conversion logic
/// through the CLI (previously: silently degraded to the 1x1 placeholder
/// and got fed to a protocol adapter as a blank image, exit 0, no
/// error). LibreOffice isn't installed in this sandbox, so the expected
/// outcome is a clean dependency-error exit, not a panic or a silently
/// wrong success.
#[test]
fn docx_input_without_libreoffice_is_a_clean_dependency_error() {
    let mut file = tempfile::Builder::new().suffix(".docx").tempfile().unwrap();
    file.write_all(&minimal_docx_bytes()).unwrap();
    let cache_dir = isolated_cache_dir();

    Command::cargo_bin("uparser")
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
        .failure()
        .code(2)
        .stdout(predicate::str::contains("\"error\""));
}

/// `--pages` on a single-page fallback input: page 1 is kept, an
/// out-of-range page number filters everything out — proving the flag
/// is genuinely applied, not silently ignored.
#[test]
fn pages_filter_keeps_only_requested_pages() {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(&fixture_png()).unwrap();

    let keep_cache = isolated_cache_dir();
    let keep_output = Command::cargo_bin("uparser")
        .unwrap()
        .env("UPARSER_CACHE_DIR", keep_cache.path())
        .args([
            "parse",
            file.path().to_str().unwrap(),
            "--protocol",
            "mock",
            "--format",
            "json",
            "--pages",
            "1",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let keep: serde_json::Value = serde_json::from_slice(&keep_output).unwrap();
    assert_eq!(keep["pages"].as_array().unwrap().len(), 1);

    let exclude_cache = isolated_cache_dir();
    let exclude_output = Command::cargo_bin("uparser")
        .unwrap()
        .env("UPARSER_CACHE_DIR", exclude_cache.path())
        .args([
            "parse",
            file.path().to_str().unwrap(),
            "--protocol",
            "mock",
            "--format",
            "json",
            "--pages",
            "999",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let exclude: serde_json::Value = serde_json::from_slice(&exclude_output).unwrap();
    assert!(exclude["pages"].as_array().unwrap().is_empty());
}

/// A malformed `--pages` value is a usage error, not a panic or a
/// silently-empty result.
#[test]
fn invalid_pages_value_is_a_usage_error() {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(&fixture_png()).unwrap();

    Command::cargo_bin("uparser")
        .unwrap()
        .args([
            "parse",
            file.path().to_str().unwrap(),
            "--protocol",
            "mock",
            "--pages",
            "5-2",
        ])
        .assert()
        .failure()
        .code(1);
}

// --- endpoint/model resolution from env + config (agent_config) ------------
// These assert the *resolved* endpoint the binary echoes back in `doctor`'s
// JSON `endpoint` field — deliberately NOT `reachable`, so they don't depend
// on any network/proxy behavior of the host.

fn doctor_endpoint_field(cmd: &mut Command) -> String {
    let out = cmd.assert().success().get_output().stdout.clone();
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    v["endpoint"].as_str().unwrap_or_default().to_string()
}

#[test]
fn doctor_resolves_endpoint_from_config_file() {
    let mut cfg = tempfile::NamedTempFile::new().unwrap();
    writeln!(
        cfg,
        "[mineru-vlm]\nendpoint = \"http://config.example/v1/chat/completions\"\nmodel = \"cfg-model\""
    )
    .unwrap();

    let mut cmd = Command::cargo_bin("uparser").unwrap();
    cmd.args(["doctor", "mineru-vlm"])
        .env("UPARSER_CONFIG", cfg.path())
        .env_remove("UPARSER_ENDPOINT");
    assert_eq!(
        doctor_endpoint_field(&mut cmd),
        "http://config.example/v1/chat/completions"
    );
}

#[test]
fn doctor_resolves_endpoint_from_env_var() {
    // Point UPARSER_CONFIG at a nonexistent file so only the env var can win.
    let mut cmd = Command::cargo_bin("uparser").unwrap();
    cmd.args(["doctor", "mineru-vlm"])
        .env("UPARSER_CONFIG", "/no/such/uparser-config.toml")
        .env("UPARSER_ENDPOINT", "http://env.example/v1/chat/completions");
    assert_eq!(
        doctor_endpoint_field(&mut cmd),
        "http://env.example/v1/chat/completions"
    );
}

#[test]
fn explicit_endpoint_flag_overrides_env_and_config() {
    let mut cfg = tempfile::NamedTempFile::new().unwrap();
    writeln!(
        cfg,
        "[mineru-vlm]\nendpoint = \"http://config.example/v1/chat/completions\""
    )
    .unwrap();

    let mut cmd = Command::cargo_bin("uparser").unwrap();
    cmd.args([
        "doctor",
        "mineru-vlm",
        "--endpoint",
        "http://flag.example/v1/chat/completions",
    ])
    .env("UPARSER_CONFIG", cfg.path())
    .env("UPARSER_ENDPOINT", "http://env.example/v1/chat/completions");
    assert_eq!(
        doctor_endpoint_field(&mut cmd),
        "http://flag.example/v1/chat/completions"
    );
}
