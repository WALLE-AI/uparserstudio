//! Financial token splitting for consolidated value items.

use crate::types::TextItem;

/// Check if a whitespace-separated token looks like a financial number.
/// Must contain at least one digit; all chars must be `0-9 , . ( ) - + %`.
pub(crate) fn is_numeric_token(tok: &str) -> bool {
    if tok.is_empty() {
        return false;
    }
    let mut has_digit = false;
    for c in tok.chars() {
        match c {
            '0'..='9' => has_digit = true,
            ',' | '.' | '(' | ')' | '-' | '+' | '%' => {}
            _ => return false,
        }
    }
    has_digit
}

/// Check for em-dash, en-dash, or minus used as nil marker in financial tables.
pub(crate) fn is_dash_token(tok: &str) -> bool {
    matches!(tok, "\u{2014}" | "\u{2013}" | "-" | "\u{2012}")
}

/// Returns true if text contains 2+ consecutive alphabetic characters.
/// Fast early-exit to reject items like `"Land $ 778,177"`.
pub(crate) fn has_alphabetic_words(text: &str) -> bool {
    let mut consecutive = 0u32;
    for c in text.chars() {
        if c.is_alphabetic() {
            consecutive += 1;
            if consecutive >= 2 {
                return true;
            }
        } else {
            consecutive = 0;
        }
    }
    false
}

/// Splits text by whitespace, then groups tokens into financial values.
/// - `$` + numeric token → one value (`"$ 5,147,649"`)
/// - standalone numeric token → one value (`"114,167"`)
/// - dash token → one value (`"—"`)
/// - any unrecognized token → return `None` (not a pure-value item)
pub(crate) fn tokenize_financial_values(text: &str) -> Option<Vec<String>> {
    let tokens: Vec<&str> = text.split_whitespace().collect();
    if tokens.is_empty() {
        return None;
    }
    let mut values = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let tok = tokens[i];
        if tok == "$" {
            // Dollar sign followed by a numeric token → one value
            if i + 1 < tokens.len() && is_numeric_token(tokens[i + 1]) {
                values.push(format!("{} {}", tok, tokens[i + 1]));
                i += 2;
            } else {
                return None;
            }
        } else if is_numeric_token(tok) || is_dash_token(tok) {
            values.push(tok.to_string());
            i += 1;
        } else {
            return None;
        }
    }
    if values.is_empty() {
        None
    } else {
        Some(values)
    }
}

/// Try to split a consolidated financial item into individual sub-items.
/// Criteria: width > font_size × 20, no alphabetic words, tokenization yields 3+ values.
/// Creates sub-items with evenly-distributed X positions across the original item's span.
pub(crate) fn try_split_financial_item(item: &TextItem) -> Option<Vec<TextItem>> {
    if item.width <= item.font_size * 20.0 {
        return None;
    }
    let text = &item.text;
    if has_alphabetic_words(text) {
        return None;
    }
    let values = tokenize_financial_values(text)?;
    if values.len() < 3 {
        return None;
    }
    let n = values.len() as f32;
    let spacing = item.width / n;
    let sub_width = spacing * 0.9;
    let mut sub_items = Vec::with_capacity(values.len());
    for (i, val) in values.iter().enumerate() {
        sub_items.push(TextItem {
            text: val.clone(),
            x: item.x + spacing * i as f32 + spacing * 0.5,
            y: item.y,
            width: sub_width,
            height: item.height,
            font: item.font.clone(),
            font_size: item.font_size,
            page: item.page,
            is_bold: item.is_bold,
            is_italic: item.is_italic,
            is_underline: item.is_underline,
            is_strikeout: item.is_strikeout,
            item_type: item.item_type.clone(),
            mcid: item.mcid,
        });
    }
    Some(sub_items)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ItemType;

    fn item(text: &str, width: f32) -> TextItem {
        TextItem {
            text: text.to_string(),
            x: 10.0,
            y: 20.0,
            width,
            height: 12.0,
            font: "Ledger".to_string(),
            font_size: 10.0,
            page: 2,
            is_bold: true,
            is_italic: true,
            is_underline: true,
            is_strikeout: true,
            item_type: ItemType::Link("https://example.com".to_string()),
            mcid: Some(7),
        }
    }

    #[test]
    fn numeric_and_dash_token_classification_is_strict() {
        for token in ["0", "1,234.50", "(99)", "+12%", "-7"] {
            assert!(is_numeric_token(token), "{token}");
        }
        for token in ["", "$12", "12x", "()", "--"] {
            assert!(!is_numeric_token(token), "{token}");
        }
        for token in ["-", "\u{2012}", "\u{2013}", "\u{2014}"] {
            assert!(is_dash_token(token), "{token}");
        }
        assert!(!is_dash_token("--"));
    }

    #[test]
    fn alphabetic_word_detection_requires_adjacent_letters() {
        assert!(has_alphabetic_words("Land $ 778,177"));
        assert!(has_alphabetic_words("\u{6570}\u{636e} 12"));
        assert!(!has_alphabetic_words("A 12 B 34"));
        assert!(!has_alphabetic_words("$ 1,000 (20)"));
    }

    #[test]
    fn financial_tokenizer_groups_currency_and_rejects_mixed_text() {
        assert_eq!(
            tokenize_financial_values("$ 5,147,649 114,167 \u{2014} (20)"),
            Some(vec![
                "$ 5,147,649".to_string(),
                "114,167".to_string(),
                "\u{2014}".to_string(),
                "(20)".to_string(),
            ])
        );
        assert_eq!(
            tokenize_financial_values("1 +2% -3"),
            Some(vec!["1".into(), "+2%".into(), "-3".into()])
        );
        for invalid in ["", "$", "$ value", "12 revenue", "USD 12"] {
            assert_eq!(tokenize_financial_values(invalid), None, "{invalid}");
        }
    }

    #[test]
    fn split_financial_item_distributes_geometry_and_preserves_metadata() {
        let source = item("$ 300 200 (100)", 300.0);
        let parts = try_split_financial_item(&source).expect("three financial values");

        assert_eq!(parts.len(), 3);
        assert_eq!(
            parts
                .iter()
                .map(|part| part.text.as_str())
                .collect::<Vec<_>>(),
            vec!["$ 300", "200", "(100)"]
        );
        assert_eq!(
            parts.iter().map(|part| part.x).collect::<Vec<_>>(),
            vec![60.0, 160.0, 260.0]
        );
        assert!(parts.iter().all(|part| part.width == 90.0));
        assert!(parts.iter().all(|part| {
            part.y == source.y
                && part.height == source.height
                && part.font == source.font
                && part.font_size == source.font_size
                && part.page == source.page
                && part.is_bold
                && part.is_italic
                && part.is_underline
                && part.is_strikeout
                && part.mcid == source.mcid
                && matches!(&part.item_type, ItemType::Link(url) if url == "https://example.com")
        }));
    }

    #[test]
    fn split_financial_item_rejects_narrow_text_and_short_value_runs() {
        assert!(try_split_financial_item(&item("1 2 3", 200.0)).is_none());
        assert!(try_split_financial_item(&item("Land 1 2 3", 300.0)).is_none());
        assert!(try_split_financial_item(&item("1 2", 300.0)).is_none());
        assert!(try_split_financial_item(&item("1 invalid 3", 300.0)).is_none());
    }
}
