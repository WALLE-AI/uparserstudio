//! P0 smoke: `cargo run -p uparser-native-engine --example smoke -- <pdf>`
//! Prints pdf_type + markdown length, then the markdown, to prove the
//! vendored engine works end-to-end inside the uparser workspace.

fn main() {
    let path = std::env::args().nth(1).expect("usage: smoke <pdf>");
    let bytes = std::fs::read(&path).expect("read pdf");
    let result = uparser_native_engine::process_pdf_mem(&bytes).expect("process");
    eprintln!(
        "pdf_type={:?} pages={} md_len={} confidence={:.2} encoding_issues={} layout={:?}",
        result.pdf_type,
        result.page_count,
        result.markdown.as_ref().map(|m| m.len()).unwrap_or(0),
        result.confidence,
        result.has_encoding_issues,
        result.layout,
    );
    print!("{}", result.markdown.unwrap_or_default());
}
