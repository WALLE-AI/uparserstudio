//! End-to-end coverage for the `native` structured-document path through the
//! real binary: asset materialisation, the `document-json` contract, the
//! agent-facing exit codes, and the parse options that reach the engine.

#![cfg(feature = "native")]

use assert_cmd::Command;
use std::io::Write;

/// A DOCX carrying one embedded PNG, wired through the relationships the
/// parser actually follows.
fn docx_with_image_bytes() -> Vec<u8> {
    // A 1x1 PNG — small, but genuinely decodable rather than a placeholder,
    // so a reader can open whatever gets written out.
    const PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];
    let parts: [(&str, &[u8]); 5] = [
        (
            "[Content_Types].xml",
            b"<Types><Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/></Types>",
        ),
        (
            "_rels/.rels",
            b"<Relationships><Relationship Id=\"r0\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/></Relationships>",
        ),
        (
            "word/_rels/document.xml.rels",
            b"<Relationships><Relationship Id=\"rImg\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/image\" Target=\"media/pic.png\"/></Relationships>",
        ),
        (
            "word/document.xml",
            b"<w:document xmlns:w=\"w\" xmlns:r=\"r\" xmlns:a=\"a\"><w:body><w:p><w:r><a:blip r:embed=\"rImg\"/></w:r></w:p></w:body></w:document>",
        ),
        ("word/media/pic.png", PNG),
    ];

    let mut buf = Vec::new();
    {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for (name, body) in parts {
            writer.start_file(name, options).unwrap();
            writer.write_all(body).unwrap();
        }
        writer.finish().unwrap();
    }
    buf
}

fn parse_markdown(source: &std::path::Path, extra: &[&str]) -> String {
    let mut args = vec![
        "parse",
        source.to_str().unwrap(),
        "--protocol",
        "native",
        "--format",
        "markdown",
    ];
    args.extend_from_slice(extra);
    let output = Command::cargo_bin("uparser")
        .unwrap()
        .args(&args)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    String::from_utf8(output).unwrap()
}

/// Structured formats previously kept their embedded images only in memory:
/// the Markdown referenced an opaque `asset-<hash>` id and nothing was ever
/// written, so every image link was dead.
#[test]
fn structured_document_image_is_written_to_disk_and_linked_from_markdown() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("pictured.docx");
    std::fs::write(&source, docx_with_image_bytes()).unwrap();

    let markdown = parse_markdown(&source, &[]);
    let link = markdown
        .split_once("](")
        .and_then(|(_, rest)| rest.split_once(')'))
        .map(|(target, _)| target.to_owned())
        .unwrap_or_else(|| panic!("expected an image link, got: {markdown}"));

    assert!(link.ends_with(".png"), "{link}");
    // The in-memory id must not leak into the link — it names nothing on disk.
    assert!(!link.contains("asset-"), "{link}");

    let written = dir.path().join(&link);
    assert!(written.exists(), "expected {} to exist", written.display());
    assert_eq!(
        std::fs::read(&written).unwrap()[..4],
        [0x89, 0x50, 0x4E, 0x47],
        "written asset should be the original PNG"
    );
}

/// `--no-assets` promises no filesystem side effect *and* no `![]()` links;
/// a link to an unwritten asset would satisfy neither.
#[test]
fn no_assets_writes_nothing_and_emits_no_image_link() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("pictured.docx");
    std::fs::write(&source, docx_with_image_bytes()).unwrap();

    assert!(!parse_markdown(&source, &["--no-assets"]).contains("!["));
    assert!(!dir.path().join("pictured_images").exists());
}

/// `document-json` must not carry raw asset bytes: `Vec<u8>` serializes as an
/// array of numbers, inflating the document several times over with content
/// no consumer can use as an image.
#[test]
fn document_json_records_asset_paths_and_never_inlines_asset_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("pictured.docx");
    std::fs::write(&source, docx_with_image_bytes()).unwrap();

    let output = Command::cargo_bin("uparser")
        .unwrap()
        .args([
            "parse",
            source.to_str().unwrap(),
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
    let json: serde_json::Value = serde_json::from_slice(&output).unwrap();
    let asset = &json["assets"][0];
    assert!(asset["bytes"].is_null(), "{asset}");
    assert!(asset["path"].as_str().unwrap().ends_with(".png"), "{asset}");
}

/// Exit codes are the agent-facing contract. "Internal error" tells an agent
/// to retry, which is the wrong advice for an input it should stop feeding in.
#[test]
fn structured_failures_map_onto_their_own_exit_codes() {
    let dir = tempfile::tempdir().unwrap();

    let unsupported = dir.path().join("mystery.bin");
    std::fs::write(&unsupported, b"not a document at all").unwrap();
    Command::cargo_bin("uparser")
        .unwrap()
        .args([
            "parse",
            unsupported.to_str().unwrap(),
            "--protocol",
            "native",
        ])
        .assert()
        .code(1);

    let oversized = dir.path().join("big.csv");
    std::fs::write(&oversized, b"a,b\n1,2\n").unwrap();
    Command::cargo_bin("uparser")
        .unwrap()
        .args([
            "parse",
            oversized.to_str().unwrap(),
            "--protocol",
            "native",
            "--max-input-mib",
            "0",
        ])
        .assert()
        .code(2);
}

/// Notes are extracted by default; `--no-notes` is the documented way to drop
/// them, and it previously did nothing because `ParseOptions` never reached
/// the engine.
#[test]
fn no_notes_drops_footnotes_that_are_otherwise_extracted() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("noted.rtf");
    std::fs::write(
        &source,
        br"{\rtf1\ansi \pard Body text{\footnote Note body}\par}",
    )
    .unwrap();

    assert!(parse_markdown(&source, &[]).contains("[^"));

    let without = parse_markdown(&source, &["--no-notes"]);
    assert!(!without.contains("[^"), "{without}");
    assert!(without.contains("Body text"), "{without}");
}

fn docx_with_header_and_footer_bytes() -> Vec<u8> {
    let parts: [(&str, &str); 6] = [
        (
            "[Content_Types].xml",
            "<Types><Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/></Types>",
        ),
        (
            "_rels/.rels",
            "<Relationships><Relationship Id=\"r0\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/></Relationships>",
        ),
        (
            "word/_rels/document.xml.rels",
            "<Relationships>\
             <Relationship Id=\"rH\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/header\" Target=\"header1.xml\"/>\
             <Relationship Id=\"rF\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer\" Target=\"footer1.xml\"/>\
             </Relationships>",
        ),
        (
            "word/header1.xml",
            "<w:hdr xmlns:w=\"w\"><w:p><w:r><w:t>Running header</w:t></w:r></w:p></w:hdr>",
        ),
        (
            "word/footer1.xml",
            "<w:ftr xmlns:w=\"w\"><w:p><w:r><w:t>Running footer</w:t></w:r></w:p></w:ftr>",
        ),
        (
            "word/document.xml",
            "<w:document xmlns:w=\"w\"><w:body><w:p><w:r><w:t>Body text</w:t></w:r></w:p></w:body></w:document>",
        ),
    ];
    let mut buf = Vec::new();
    {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for (name, body) in parts {
            writer.start_file(name, options).unwrap();
            writer.write_all(body.as_bytes()).unwrap();
        }
        writer.finish().unwrap();
    }
    buf
}

/// Headers/footers repeat on every page, so they are excluded by default —
/// but `--headers-footers` has to actually include them. The option existed
/// on `ParseOptions` while no frontend read it, so the flag was inert.
#[test]
fn headers_and_footers_are_excluded_by_default_and_included_on_request() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("paged.docx");
    std::fs::write(&source, docx_with_header_and_footer_bytes()).unwrap();

    let default = parse_markdown(&source, &[]);
    assert!(default.contains("Body text"), "{default}");
    assert!(!default.contains("Running header"), "{default}");
    assert!(!default.contains("Running footer"), "{default}");

    let included = parse_markdown(&source, &["--headers-footers"]);
    assert!(included.contains("Running header"), "{included}");
    assert!(included.contains("Body text"), "{included}");
    assert!(included.contains("Running footer"), "{included}");
    // Header before body before footer — the only ordering that reads
    // sensibly once pagination is gone.
    let header = included.find("Running header").unwrap();
    let body = included.find("Body text").unwrap();
    let footer = included.find("Running footer").unwrap();
    assert!(header < body && body < footer, "{included}");
}

/// A closed stdout is ordinary (`uparser … | head`), not a crash. `println!`
/// panics there, which surfaced a Rust panic trace to any agent that piped
/// the output.
#[test]
fn closed_stdout_is_not_a_panic() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("rows.csv");
    let mut body = String::from("a,b\n");
    for row in 0..20_000 {
        body.push_str(&format!("{row},value{row}\n"));
    }
    std::fs::write(&source, body).unwrap();

    let mut child = std::process::Command::new(assert_cmd::cargo::cargo_bin("uparser"))
        .args([
            "parse",
            source.to_str().unwrap(),
            "--protocol",
            "native",
            "--format",
            "markdown",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    // Drop the read end immediately: the child's next write sees a broken pipe.
    drop(child.stdout.take());
    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("panicked"), "{stderr}");
}
