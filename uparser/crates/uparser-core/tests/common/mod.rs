//! Fixtures shared by the integration-test crates.
//!
//! Each file under `tests/` compiles as its own crate, so anything two of
//! them need has to live here rather than being copied — these builders
//! define what "a born-digital PDF" and "an image-only PDF" mean for the
//! whole suite, and a copy that drifted would silently weaken one of them.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

pub fn write_fixture(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

fn assemble_pdf(objects: &[String]) -> Vec<u8> {
    let mut pdf = Vec::new();
    let mut offsets = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.7\n");
    for (index, body) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", index + 1).as_bytes());
    }
    let xref = pdf.len();
    pdf.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
    );
    for offset in &offsets {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    pdf
}

/// A born-digital PDF with a real text layer.
pub fn text_pdf() -> Vec<u8> {
    let content = "BT /F1 24 Tf 72 700 Td (Hello uparser) Tj ET";
    assemble_pdf(&[
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
         /Resources << /Font << /F1 5 0 R >> >> >>"
            .to_owned(),
        format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        ),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
    ])
}

/// A PDF with a page but no text operators at all: the `native` protocol
/// cannot read it, and must say so rather than inventing a placeholder.
pub fn image_only_pdf() -> Vec<u8> {
    assemble_pdf(&[
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>".to_owned(),
        "<< /Length 0 >>\nstream\n\nendstream".to_owned(),
    ])
}

/// RTF exercising the block kinds the canonical model distinguishes:
/// a real heading (`\outlinelevel`), body text, and a two-item list
/// (`\ls`, which is what this frontend treats as list membership).
pub fn rtf() -> Vec<u8> {
    br"{\rtf1\ansi
\pard\outlinelevel0 Report title\par
\pard Body paragraph text.\par
\pard\ls1\ilvl0 First item\par
\pard\ls1\ilvl0 Second item\par
\pard Closing paragraph.\par
}"
    .to_vec()
}

pub fn csv() -> Vec<u8> {
    b"name,value\nalpha,42\nbeta,7\n".to_vec()
}
