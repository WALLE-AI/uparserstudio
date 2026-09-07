//! Print the structure hints a PDF produces, next to the Markdown headings
//! the same pass emitted.
//!
//! Diagnostic for the renderer-unification work: when a heading appears in
//! the Markdown but not in a consumer's IR, this shows whether the hint was
//! never recorded or merely failed to match the consumer's line boxes.
//!
//!     cargo run -p uparser-native-engine --example dump_structure_hints -- <file.pdf>

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: dump_structure_hints <file.pdf>");
        std::process::exit(2);
    };
    let bytes = std::fs::read(&path).expect("read pdf");
    let result = uparser_native_engine::process_pdf_mem(&bytes).expect("parse pdf");
    let hints = &result.structure_hints;

    let markdown_headings: Vec<&str> = result
        .markdown
        .as_deref()
        .unwrap_or_default()
        .lines()
        .filter(|line| line.starts_with('#'))
        .collect();

    println!("markdown headings ({}):", markdown_headings.len());
    for heading in &markdown_headings {
        println!("  {heading}");
    }

    println!("\nheading hints ({}):", hints.headings.len());
    for hint in &hints.headings {
        println!(
            "  page={} level={} bbox={:?} text={:?}",
            hint.page, hint.level, hint.bbox, hint.text
        );
    }

    println!("\ntable hints ({}):", hints.tables.len());
    for hint in &hints.tables {
        println!(
            "  page={} {}x{} bbox={:?} kind={:?}",
            hint.page, hint.rows, hint.columns, hint.bbox, hint.kind
        );
    }

    println!("\nline hints: {}", hints.lines.len());
    for hint in hints.lines.iter().take(12) {
        println!(
            "  order={} page={} bbox={:?} pieces={} text={:?}",
            hint.order,
            hint.page,
            hint.bbox,
            hint.item_texts.len(),
            hint.text.chars().take(50).collect::<String>()
        );
    }
}
