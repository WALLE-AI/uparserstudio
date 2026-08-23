//! Markdown cleanup and post-processing.

use std::collections::{HashMap, HashSet};

use regex::Regex;

use super::{MarkdownOptions, MarkdownProfile};
use crate::text_utils::is_page_number_line;

/// Clean up markdown output with post-processing
pub(crate) fn clean_markdown(mut text: String, options: &MarkdownOptions) -> String {
    if options.profile == MarkdownProfile::Compact {
        // Dot-leader collapse saves tokens but changes source text, so it is
        // reserved for the explicit compact profile.
        text = collapse_dot_leaders(&text);
    }

    // Fix hyphenation first (before other processing)
    if options.fix_hyphenation {
        text = fix_hyphenation(&text);
    }

    // Remove standalone page numbers
    if options.remove_page_numbers {
        text = remove_page_numbers(&text);
    }

    // Format URLs as markdown links
    if options.format_urls {
        text = repair_wrapped_arxiv_urls(&text);
        text = format_urls(&text);
    }

    // Collapse consecutive spaces within text lines.
    // OCR text layers and some PDF producers emit trailing spaces on each
    // text item, which combine with gap-based space insertion to produce
    // double spaces ("Vice  President" instead of "Vice President").
    collapse_consecutive_spaces(&mut text);
    remove_spaces_before_closing_brackets(&mut text);
    remove_spaces_before_sentence_punctuation(&mut text);
    text = refine_table_lines(&text);
    text = repair_trailing_toc_part_headings(&text);
    text = refine_heading_blocks(&text);

    // Remove excessive newlines (more than 2 in a row)
    while text.contains("\n\n\n") {
        text = text.replace("\n\n\n", "\n\n");
    }

    // Trim leading and trailing whitespace, ensure ends with single newline
    text = text.trim().to_string();
    text.push('\n');

    text
}

fn repair_trailing_toc_part_headings(text: &str) -> String {
    let lines: Vec<String> = text.lines().map(str::to_owned).collect();
    let has_contents_marker = lines.iter().any(|line| {
        let visible = strip_inline_markup(line.trim_start_matches('#').trim());
        matches!(
            visible.trim().to_ascii_lowercase().as_str(),
            "contents" | "table of contents"
        )
    });

    let mut headings: Vec<(usize, usize, String)> = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| toc_part_number(line).map(|part| (index, part, line.clone())))
        .collect();
    if headings.len() < 2 || (!has_contents_marker && headings.len() < 3) {
        return text.to_owned();
    }

    let mut section_positions = HashMap::new();
    for (_, part, _) in &headings {
        let prefix = format!("Section {part}.");
        let Some(position) = lines.iter().position(|line| {
            strip_inline_markup(line.trim_start_matches('#').trim()).starts_with(&prefix)
        }) else {
            return text.to_owned();
        };
        section_positions.insert(*part, position);
    }
    headings.retain(|(heading_index, part, _)| {
        section_positions
            .get(part)
            .is_some_and(|section_index| heading_index > section_index)
    });
    if headings.len() < 2 {
        return text.to_owned();
    }

    let heading_indices: HashSet<usize> = headings.iter().map(|(index, _, _)| *index).collect();
    let mut repaired: Vec<String> = lines
        .into_iter()
        .enumerate()
        .filter(|(index, _)| !heading_indices.contains(index))
        .map(|(_, line)| line)
        .collect();
    headings.sort_by_key(|(_, part, _)| *part);
    for (_, part, heading) in headings {
        let prefix = format!("Section {part}.");
        let Some(position) = repaired.iter().position(|line| {
            strip_inline_markup(line.trim_start_matches('#').trim()).starts_with(&prefix)
        }) else {
            continue;
        };
        repaired.splice(position..position, [String::new(), heading, String::new()]);
    }
    repaired.join("\n")
}

fn toc_part_number(line: &str) -> Option<usize> {
    let visible = strip_inline_markup(line.trim_start_matches('#').trim());
    let rest = visible.strip_prefix("Part ")?;
    if !rest.to_ascii_lowercase().contains("chapter") {
        return None;
    }
    match rest.split_whitespace().next()?.trim_end_matches('.') {
        "I" => Some(1),
        "II" => Some(2),
        "III" => Some(3),
        "IV" => Some(4),
        "V" => Some(5),
        "VI" => Some(6),
        "VII" => Some(7),
        "VIII" => Some(8),
        "IX" => Some(9),
        "X" => Some(10),
        "XI" => Some(11),
        "XII" => Some(12),
        _ => None,
    }
}

fn refine_heading_blocks(text: &str) -> String {
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();

    for line in &mut lines {
        *line = separate_numbered_heading_prefix(line);
    }

    let mut in_contents = false;
    for line in &mut lines {
        let trimmed = line.trim();
        if let Some((marker, heading)) = trimmed.split_once(' ') {
            if !marker.is_empty() && marker.len() <= 6 && marker.chars().all(|c| c == '#') {
                in_contents = strip_inline_markup(heading)
                    .to_ascii_lowercase()
                    .contains("contents");
                continue;
            }
        }
        let Some(inner) = trimmed
            .strip_prefix("**")
            .and_then(|value| value.strip_suffix("**"))
        else {
            continue;
        };
        if inner.contains("**") {
            continue;
        }
        let letters: Vec<char> = inner.chars().filter(|c| c.is_alphabetic()).collect();
        let uppercase_title = !letters.is_empty()
            && letters.iter().all(|c| !c.is_lowercase())
            && inner.split_whitespace().count() <= 8;
        let long_enumerated_prose =
            super::classify::is_list_item(inner) && inner.chars().count() > 40;
        if !in_contents
            && !long_enumerated_prose
            && (uppercase_title || has_multilevel_number(inner))
        {
            *line = format!("# {inner}");
        }
    }

    let mut previous_nonempty_was_heading = false;
    for line in &mut lines {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Some((marker, heading)) = trimmed.split_once(' ') else {
            previous_nonempty_was_heading = false;
            continue;
        };
        if marker.is_empty() || marker.len() > 6 || !marker.chars().all(|c| c == '#') {
            previous_nonempty_was_heading = false;
            continue;
        }

        let visible = strip_inline_markup(heading);
        let starts_lowercase = visible
            .chars()
            .find(|c| c.is_alphabetic())
            .is_some_and(|c| c.is_lowercase());
        let word_count = heading.split_whitespace().count();
        let sentence_like = heading.ends_with('.') && word_count >= 12;
        let numeric_callout = word_count <= 2
            && visible.chars().any(|c| c.is_ascii_digit())
            && visible.chars().any(|c| matches!(c, '%' | '↑' | '↓'));
        let equation_fragment = super::analysis::is_heading_fragment(&visible)
            || looks_like_numbered_display_equation(&visible);
        let adjacent_short_sentence = previous_nonempty_was_heading
            && heading.ends_with('.')
            && (4..=10).contains(&word_count)
            && !starts_with_roman_numeral(heading);
        if starts_lowercase
            || sentence_like
            || numeric_callout
            || equation_fragment
            || adjacent_short_sentence
        {
            *line = heading.to_owned();
            previous_nonempty_was_heading = false;
        } else {
            previous_nonempty_was_heading = true;
        }
    }

    demote_visual_heading_runs(&mut lines);
    deduplicate_adjacent_headings(&lines).join("\n")
}

fn deduplicate_adjacent_headings(lines: &[String]) -> Vec<String> {
    let mut output = Vec::with_capacity(lines.len());
    let mut previous_heading: Option<String> = None;
    for line in lines {
        if let Some((_, heading)) = markdown_heading(line) {
            let normalized = strip_inline_markup(heading).trim().to_lowercase();
            if previous_heading.as_deref() == Some(normalized.as_str()) {
                while output
                    .last()
                    .is_some_and(|line: &String| line.trim().is_empty())
                {
                    output.pop();
                }
                continue;
            }
            previous_heading = Some(normalized);
        } else if !line.trim().is_empty() {
            previous_heading = None;
        }
        output.push(line.clone());
    }
    output
}

/// PDF text runs frequently split a section number and its title into
/// separate positioned items without a visible gap. The line grouper then
/// produces `### 3.2Decode`. Repair only existing Markdown headings and only
/// when an ASCII uppercase title immediately follows a numeric section path;
/// this keeps model names and ordinary numeric prose untouched.
fn separate_numbered_heading_prefix(line: &str) -> String {
    use once_cell::sync::Lazy;
    static GLUED_SECTION: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"^(#{1,6}\s+)(\d+(?:\.\d+)*)([A-Z][A-Za-z])").unwrap());

    GLUED_SECTION.replace(line, "$1$2 $3").to_string()
}

fn looks_like_numbered_display_equation(text: &str) -> bool {
    let trimmed = text.trim_end();
    let Some(open) = trimmed.rfind('(') else {
        return false;
    };
    let Some(number) = trimmed[open + 1..].strip_suffix(')') else {
        return false;
    };
    !number.is_empty()
        && number.len() <= 3
        && number.chars().all(|ch| ch.is_ascii_digit())
        && trimmed[..open].chars().any(|ch| {
            matches!(
                ch,
                '=' | '<' | '>' | '≤' | '≥' | '≈' | '∑' | '∫' | '√' | '∪'
            )
        })
}

/// A PDF font tier can turn every wrapped line in a quotation or callout into
/// a low-level heading. Demote only sustained runs of long, non-structural
/// headings; chapter and contents sequences retain their hierarchy.
fn demote_visual_heading_runs(lines: &mut [String]) {
    let mut index = 0;
    while index < lines.len() {
        let Some((level, _)) = markdown_heading(&lines[index]) else {
            index += 1;
            continue;
        };
        if level < 4 {
            index += 1;
            continue;
        }

        let start = index;
        let mut cursor = index;
        let mut headings = Vec::new();
        while cursor < lines.len() {
            if lines[cursor].trim().is_empty() {
                cursor += 1;
                continue;
            }
            match markdown_heading(&lines[cursor]) {
                Some((candidate_level, heading)) if candidate_level == level => {
                    if !headings.is_empty() && looks_like_structural_heading(heading) {
                        break;
                    }
                    headings.push((cursor, heading.to_owned()));
                    cursor += 1;
                }
                _ => break,
            }
        }

        let visible_chars: usize = headings
            .iter()
            .map(|(_, heading)| strip_inline_markup(heading).chars().count())
            .sum();
        let structural = headings
            .first()
            .is_some_and(|(_, heading)| looks_like_structural_heading(heading));
        if headings.len() >= 4 && visible_chars >= 80 && !structural {
            for (line_index, heading) in headings {
                lines[line_index] = heading;
            }
        }
        index = cursor.max(start + 1);
    }
}

fn markdown_heading(line: &str) -> Option<(usize, &str)> {
    let trimmed = line.trim();
    let (marker, heading) = trimmed.split_once(' ')?;
    (!marker.is_empty()
        && marker.len() <= 6
        && marker.chars().all(|ch| ch == '#')
        && !heading.trim().is_empty())
    .then_some((marker.len(), heading))
}

fn looks_like_structural_heading(text: &str) -> bool {
    let visible = strip_inline_markup(text);
    let compact = visible.trim_start();
    if compact.chars().next().is_some_and(|ch| ch.is_ascii_digit()) {
        return true;
    }
    if compact.starts_with('第')
        && compact
            .chars()
            .take(8)
            .any(|ch| matches!(ch, '章' | '節' | '节' | '編' | '编' | '部' | '卷'))
    {
        return true;
    }
    let Some((prefix, _)) = compact.split_once(['、', '．', '.']) else {
        return false;
    };
    !prefix.is_empty()
        && prefix.chars().count() <= 4
        && prefix
            .chars()
            .all(|ch| "一二三四五六七八九十百壹貳參肆伍陸柒捌玖拾".contains(ch))
}

fn has_multilevel_number(text: &str) -> bool {
    let token = text
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .trim_end_matches('.');
    let parts: Vec<&str> = token.split('.').collect();
    parts.len() >= 2
        && parts.iter().all(|part| {
            !part.is_empty() && part.len() <= 3 && part.chars().all(|c| c.is_ascii_digit())
        })
}

fn starts_with_roman_numeral(text: &str) -> bool {
    let token = text
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .trim_end_matches('.');
    !token.is_empty()
        && token
            .chars()
            .all(|c| matches!(c, 'I' | 'V' | 'X' | 'L' | 'C' | 'D' | 'M'))
}

fn strip_inline_markup(text: &str) -> String {
    let mut visible = String::with_capacity(text.len());
    let mut in_tag = false;
    for ch in text.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            '*' | '_' | '`' | '~' if !in_tag => {}
            _ if !in_tag => visible.push(ch),
            _ => {}
        }
    }
    visible
}

fn refine_table_lines(text: &str) -> String {
    let text = demote_equation_markdown_tables(text);
    let text = demote_parallel_cjk_markdown_tables(&text);
    let mut output = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with('|') || !trimmed.ends_with('|') {
            output.push(line.to_owned());
            continue;
        }
        let original_cells: Vec<&str> = trimmed.trim_matches('|').split('|').collect();
        let mut cells: Vec<String> = original_cells
            .iter()
            .map(|cell| normalize_tracked_caps(cell.trim()))
            .collect();
        let changed = original_cells
            .iter()
            .zip(&cells)
            .any(|(original, refined)| original.trim() != refined);
        if cells.len() == 2 {
            if let Some((label, section)) = split_outcomes_label(&cells[0]) {
                cells[0] = label;
                output.push(format!("|{}|{}|", cells[0], cells[1]));
                output.push(format!("|{section}||"));
                continue;
            }
        }
        if changed {
            output.push(format!("|{}|", cells.join("|")));
        } else {
            output.push(line.to_owned());
        }
    }
    output.join("\n")
}

/// Geometry-only table detection can mistake a two-dimensional display
/// equation for a sparse table: aligned symbols become columns and a fraction
/// bar becomes the Markdown separator. Preserve the extracted tokens as plain
/// display text instead of asserting a false row/column schema. This is a
/// deliberately narrow fallback; real tables containing an occasional formula
/// remain tables.
fn demote_equation_markdown_tables(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut output = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        if !is_markdown_table_line(lines[index]) {
            output.push(lines[index].to_owned());
            index += 1;
            continue;
        }

        let start = index;
        while index < lines.len() && is_markdown_table_line(lines[index]) {
            index += 1;
        }
        let group = &lines[start..index];
        if !equation_like_markdown_table(group) {
            output.extend(group.iter().map(|line| (*line).to_owned()));
            continue;
        }

        for line in group {
            let cells: Vec<&str> = line.trim().trim_matches('|').split('|').collect();
            if is_markdown_separator_row(&cells) {
                continue;
            }
            let reconstructed = cells
                .iter()
                .map(|cell| cell.trim())
                .filter(|cell| !cell.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            if !reconstructed.is_empty() {
                output.push(reconstructed);
            }
        }
    }
    output.join("\n")
}

fn equation_like_markdown_table(group: &[&str]) -> bool {
    if !(3..=8).contains(&group.len()) {
        return false;
    }
    let rows: Vec<Vec<&str>> = group
        .iter()
        .map(|line| line.trim().trim_matches('|').split('|').collect())
        .collect();
    if rows.len() < 2 || !is_markdown_separator_row(&rows[1]) {
        return false;
    }

    let columns = rows[1].len();
    if columns < 4 || rows.iter().any(|row| row.len() > columns + 2) {
        return false;
    }
    let header_nonempty = rows[0]
        .iter()
        .filter(|cell| !cell.trim().is_empty())
        .count();
    if header_nonempty > 2 || header_nonempty * 3 > columns + 1 {
        return false;
    }

    let visible = rows
        .iter()
        .enumerate()
        .filter(|(row, _)| *row != 1)
        .flat_map(|(_, cells)| cells.iter())
        .map(|cell| cell.trim())
        .filter(|cell| !cell.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let has_equation_number = (1..=99).any(|number| visible.contains(&format!("({number})")));
    let math_evidence = ['=', '≈', '∪', '⋆', '−', '∑', '∫', '√']
        .iter()
        .filter(|operator| visible.contains(**operator))
        .count();
    let has_math_markup = visible.contains("<u>") || visible.contains("max(");
    let short_symbol_cells = rows
        .iter()
        .enumerate()
        .filter(|(row, _)| *row != 1)
        .flat_map(|(_, cells)| cells.iter())
        .filter(|cell| {
            let width = cell.trim().chars().count();
            width > 0 && width <= 3
        })
        .count();

    has_equation_number
        && (math_evidence >= 2 || (math_evidence >= 1 && has_math_markup))
        && short_symbol_cells >= 3
}

fn demote_parallel_cjk_markdown_tables(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut output = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        if !is_markdown_table_line(lines[index]) {
            output.push(lines[index].to_owned());
            index += 1;
            continue;
        }
        let start = index;
        while index < lines.len() && is_markdown_table_line(lines[index]) {
            index += 1;
        }
        let group = &lines[start..index];
        let Some(columns) = parallel_cjk_prose_columns(group) else {
            output.extend(group.iter().map(|line| (*line).to_owned()));
            continue;
        };
        for column in columns {
            output.push(column);
            output.push(String::new());
        }
        if output.last().is_some_and(String::is_empty) {
            output.pop();
        }
    }
    output.join("\n")
}

fn is_markdown_table_line(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.starts_with('|') && trimmed.ends_with('|')
}

fn parallel_cjk_prose_columns(group: &[&str]) -> Option<Vec<String>> {
    if !(4..=7).contains(&group.len()) {
        return None;
    }
    let rows: Vec<Vec<&str>> = group
        .iter()
        .map(|line| line.trim().trim_matches('|').split('|').collect())
        .collect();
    let column_count = rows.iter().map(Vec::len).max()?;
    if !(2..=4).contains(&column_count) || !is_markdown_separator_row(&rows[1]) {
        return None;
    }
    let data_rows = [&rows[..1], &rows[2..]].concat();
    let nonempty = data_rows
        .iter()
        .flatten()
        .filter(|cell| !cell.trim().is_empty())
        .count();
    if nonempty >= data_rows.len() * column_count {
        return None;
    }

    let first_filled: Vec<&str> = data_rows[0]
        .iter()
        .map(|cell| cell.trim())
        .filter(|cell| !cell.is_empty())
        .collect();
    if first_filled.len() < 2 || first_filled.iter().all(|cell| cell.chars().count() <= 12) {
        return None;
    }

    let cjk_prose_cells: Vec<&str> = data_rows
        .iter()
        .flatten()
        .map(|cell| cell.trim())
        .filter(|cell| {
            cell.chars().count() >= 10
                && cell.chars().any(
                    |ch| matches!(ch as u32, 0x3400..=0x4dbf | 0x4e00..=0x9fff | 0xf900..=0xfaff),
                )
        })
        .collect();
    let cjk_chars: usize = cjk_prose_cells
        .iter()
        .map(|cell| cell.chars().filter(|ch| ch.is_alphabetic()).count())
        .sum();
    if cjk_prose_cells.len() < 4 || cjk_prose_cells.len() * 5 < nonempty * 3 || cjk_chars < 70 {
        return None;
    }

    let mut continuation_count = 0;
    let mut continuation_columns = HashSet::new();
    for adjacent in data_rows.windows(2) {
        for column in 0..column_count {
            let previous = adjacent[0].get(column).copied().unwrap_or("");
            let current = adjacent[1].get(column).copied().unwrap_or("");
            if super::is_cross_row_prose_continuation(previous, current) {
                continuation_count += 1;
                continuation_columns.insert(column);
            }
        }
    }
    let compact_sparse_fragment = data_rows.len() == 3
        && nonempty <= 6
        && !data_rows
            .iter()
            .flatten()
            .any(|cell| cell.chars().filter(|ch| ch.is_ascii_digit()).count() >= 2);
    if (continuation_count < 3 || continuation_columns.len() < 2) && !compact_sparse_fragment {
        return None;
    }

    let columns: Vec<String> = (0..column_count)
        .filter_map(|column| {
            let text = data_rows
                .iter()
                .filter_map(|row| row.get(column))
                .map(|cell| cell.trim())
                .filter(|cell| !cell.is_empty())
                .collect::<Vec<_>>()
                .join("");
            (!text.is_empty()).then_some(text)
        })
        .collect();
    Some(columns)
}

fn is_markdown_separator_row(cells: &[&str]) -> bool {
    cells.iter().all(|cell| {
        let cell = cell.trim().trim_matches(':');
        cell.len() >= 3 && cell.chars().all(|ch| ch == '-')
    })
}

fn normalize_tracked_caps(cell: &str) -> String {
    use once_cell::sync::Lazy;
    static FRAGMENT: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b([A-Z])\s+([A-Z]{2,})\b").unwrap());
    static SINGLE_PAIR: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b([A-Z])\s+([A-Z])\b").unwrap());
    static PUNCTUATION: Lazy<Regex> = Lazy::new(|| Regex::new(r"\s+([:;,])").unwrap());
    static HYPHEN: Lazy<Regex> = Lazy::new(|| Regex::new(r"\s*-\s*").unwrap());

    if FRAGMENT.find_iter(cell).count() < 2 {
        return cell.to_owned();
    }
    let mut normalized = cell.to_owned();
    loop {
        let next = FRAGMENT.replace_all(&normalized, "$1$2").to_string();
        if next == normalized {
            break;
        }
        normalized = next;
    }
    loop {
        let next = SINGLE_PAIR.replace_all(&normalized, "$1$2").to_string();
        if next == normalized {
            break;
        }
        normalized = next;
    }
    normalized = PUNCTUATION.replace_all(&normalized, "$1").to_string();
    HYPHEN.replace_all(&normalized, "-").to_string()
}

fn split_outcomes_label(cell: &str) -> Option<(String, String)> {
    const SECTIONS: &[&str] = &["Learning Outcomes", "Expected Outcomes", "Key Outcomes"];
    const PREFIXES: &[&str] = &["Statement", "Description"];
    for section in SECTIONS {
        let Some(prefix) = cell.strip_suffix(section).map(str::trim_end) else {
            continue;
        };
        if PREFIXES.iter().any(|ending| prefix.ends_with(ending)) {
            return Some((prefix.to_owned(), (*section).to_owned()));
        }
    }
    None
}

/// Collapse runs of 2+ spaces to a single space within each line.
/// Preserves leading indentation and markdown table pipe alignment.
fn collapse_consecutive_spaces(text: &mut String) {
    let mut result = String::with_capacity(text.len());
    for line in text.split('\n') {
        if !result.is_empty() {
            result.push('\n');
        }
        // Preserve leading whitespace
        let trimmed = line.trim_start();
        let leading = &line[..line.len() - trimmed.len()];
        result.push_str(leading);
        // Collapse inner runs of spaces to single space
        let mut prev_space = false;
        for ch in trimmed.chars() {
            if ch == ' ' {
                if !prev_space {
                    result.push(' ');
                }
                prev_space = true;
            } else {
                prev_space = false;
                result.push(ch);
            }
        }
    }
    *text = result;
}

/// Remove spaces before closing square brackets.
/// Unit markers and markdown links occasionally pick up a gap-inserted space
/// before `]` (e.g. `[kg/m3 ]`), which is cosmetic padding.
fn remove_spaces_before_closing_brackets(text: &mut String) {
    let mut result = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch == ']' && result.ends_with(' ') {
            result.pop();
        }
        result.push(ch);
    }
    *text = result;
}

/// Remove a stray space before sentence punctuation ("word ." → "word.").
/// Style-boundary item splits (bold/italic/underline runs) can strand a
/// trailing period or comma in its own fragment, and several assembly paths
/// join fragments with spaces. Only fires when the punctuation ends the
/// token (followed by whitespace or end of text), so decimals ("3 .14" stays
/// untouched — no such input exists, but the guard is cheap) and dot leaders
/// (" ... ") are unaffected.
fn remove_spaces_before_sentence_punctuation(text: &mut String) {
    let chars: Vec<char> = text.chars().collect();
    let mut result = String::with_capacity(text.len());
    for (i, &ch) in chars.iter().enumerate() {
        if matches!(ch, '.' | ',' | ';') && result.ends_with(' ') {
            let next = chars.get(i + 1);
            // `|` counts as a token end so table cells get the same fix.
            let token_ends = next.is_none_or(|c| c.is_whitespace() || *c == '|');
            // Never touch runs of dots (ellipsis / dot leaders).
            let in_dot_run = ch == '.' && next == Some(&'.');
            if token_ends && !in_dot_run {
                result.pop();
            }
        }
        result.push(ch);
    }
    *text = result;
}

/// Collapse dot leaders (runs of 4+ dots) into " ... "
/// Common in tables of contents: "Introduction...............................1" -> "Introduction ... 1"
fn collapse_dot_leaders(text: &str) -> String {
    use once_cell::sync::Lazy;
    static DOT_LEADER_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\.{4,}").unwrap());

    DOT_LEADER_RE.replace_all(text, " ... ").to_string()
}

/// Fix words broken across lines with spaces before the continuation
/// e.g., "Limoeiro do Nort e" -> "Limoeiro do Norte"
fn fix_hyphenation(text: &str) -> String {
    use once_cell::sync::Lazy;

    // Fix "word - word" patterns that should be "word-word" (compound words)
    // But be careful not to break list items (which start with "- ")
    static SPACED_HYPHEN_RE: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r"([a-zA-ZáàâãéèêíïóôõöúçñÁÀÂÃÉÈÊÍÏÓÔÕÖÚÇÑ]) - ([a-zA-ZáàâãéèêíïóôõöúçñÁÀÂÃÉÈÊÍÏÓÔÕÖÚÇÑ])").unwrap()
    });

    let result = SPACED_HYPHEN_RE
        .replace_all(text, |caps: &regex::Captures| {
            format!("{}-{}", &caps[1], &caps[2])
        })
        .to_string();

    result
}

/// Remove isolated page-number expressions from Markdown.
fn remove_page_numbers(text: &str) -> String {
    let mut result = Vec::new();
    let lines: Vec<&str> = text.lines().collect();
    let front_matter_headings: HashSet<String> = lines
        .iter()
        .take(16)
        .filter_map(|line| markdown_heading(line).map(|(_, heading)| strip_inline_markup(heading)))
        .collect();

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();

        if is_front_matter_running_header(trimmed, &front_matter_headings) {
            continue;
        }

        // Check for page number patterns
        if is_page_number_line(trimmed) {
            // Check context to determine if this is isolated
            let prev_is_break = i > 0 && lines[i - 1].trim() == "---";
            let next_is_break = i + 1 < lines.len() && lines[i + 1].trim() == "---";
            let prev_is_empty = i > 0 && lines[i - 1].trim().is_empty();
            let next_is_empty = i + 1 < lines.len() && lines[i + 1].trim().is_empty();

            // Check if it's on its own line (surrounded by empty lines or page breaks)
            let is_isolated = (prev_is_break || prev_is_empty || i == 0)
                && (next_is_break || next_is_empty || i + 1 == lines.len());

            // Also remove numbers that appear right before a page break
            let before_break = i + 1 < lines.len()
                && (lines[i + 1].trim() == "---"
                    || (i + 2 < lines.len()
                        && lines[i + 1].trim().is_empty()
                        && lines[i + 2].trim() == "---"));

            if is_isolated || before_break {
                continue;
            }
        }

        result.push(*line);
    }

    result.join("\n")
}

fn is_front_matter_running_header(line: &str, headings: &HashSet<String>) -> bool {
    let mut tokens = line.split_whitespace();
    let Some(first) = tokens.next() else {
        return false;
    };
    if first.len() <= 4 && first.chars().all(|ch| ch.is_ascii_digit()) {
        let remainder = tokens.collect::<Vec<_>>().join(" ");
        if headings.contains(&remainder) {
            return true;
        }
    }

    let Some((prefix, page)) = line.rsplit_once(char::is_whitespace) else {
        return false;
    };
    page.len() <= 4
        && page.chars().all(|ch| ch.is_ascii_digit())
        && headings.contains(prefix.trim())
}

/// Convert URLs to markdown links
fn format_urls(text: &str) -> String {
    use once_cell::sync::Lazy;

    // Match URLs - we'll check context manually to avoid formatting already-linked URLs
    static URL_RE: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"https?://[^\s<>\)\]]+[^\s<>\)\]\.\,;]").unwrap());

    let mut result = String::with_capacity(text.len());
    let mut last_end = 0;

    for mat in URL_RE.find_iter(text) {
        let start = mat.start();
        let url = mat.as_str();

        // Check if this URL is already in a markdown link by looking at preceding chars
        // Use safe character boundary checking for multi-byte UTF-8
        let before = {
            let mut check_start = start.saturating_sub(2);
            // Find a valid character boundary
            while check_start > 0 && !text.is_char_boundary(check_start) {
                check_start -= 1;
            }
            if check_start < start && text.is_char_boundary(start) {
                &text[check_start..start]
            } else {
                ""
            }
        };
        let already_linked = before.ends_with("](") || before.ends_with("](");

        // Also check if it's inside square brackets (link text)
        // Ensure we're slicing at a valid char boundary
        let prefix = if text.is_char_boundary(start) {
            &text[..start]
        } else {
            // Find the nearest valid boundary before start
            let mut safe_start = start;
            while safe_start > 0 && !text.is_char_boundary(safe_start) {
                safe_start -= 1;
            }
            &text[..safe_start]
        };
        let open_brackets = prefix.matches('[').count();
        let close_brackets = prefix.matches(']').count();
        let inside_link_text = open_brackets > close_brackets;

        // Ensure mat boundaries are valid char boundaries
        let safe_last_end = if text.is_char_boundary(last_end) {
            last_end
        } else {
            let mut pos = last_end;
            while pos < text.len() && !text.is_char_boundary(pos) {
                pos += 1;
            }
            pos
        };
        let safe_start = if text.is_char_boundary(start) {
            start
        } else {
            let mut pos = start;
            while pos < text.len() && !text.is_char_boundary(pos) {
                pos += 1;
            }
            pos
        };
        let safe_end = if text.is_char_boundary(mat.end()) {
            mat.end()
        } else {
            let mut pos = mat.end();
            while pos < text.len() && !text.is_char_boundary(pos) {
                pos += 1;
            }
            pos
        };

        if already_linked || inside_link_text {
            // Already formatted, keep as-is
            if safe_last_end <= safe_end {
                result.push_str(&text[safe_last_end..safe_end]);
            }
        } else {
            // Add text before this URL
            if safe_last_end <= safe_start {
                result.push_str(&text[safe_last_end..safe_start]);
            }
            // Format as markdown link
            result.push_str(&format!("[{}]({})", url, url));
        }
        last_end = safe_end;
    }

    // Add remaining text (ensure valid char boundary)
    let safe_last_end = if text.is_char_boundary(last_end) {
        last_end
    } else {
        let mut pos = last_end;
        while pos < text.len() && !text.is_char_boundary(pos) {
            pos += 1;
        }
        pos
    };
    if safe_last_end < text.len() {
        result.push_str(&text[safe_last_end..]);
    }
    result
}

fn repair_wrapped_arxiv_urls(text: &str) -> String {
    use once_cell::sync::Lazy;
    static SCHEME_GAP: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?i)https:\s+//\s*").unwrap());
    static DOMAIN_GAP: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"(?i)arxiv(?:\.\s+org|\.or\s+g)").unwrap());
    static PATH_GAP: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"(?i)(https?://arxiv\.org/)\s+(abs/\d{4}\.\d{4,6})").unwrap());
    static LINE_WRAP: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r"(?i)(https?://arxi[^\s]*)[ \t]*\n[ \t]*\n?[ \t]*([a-z0-9.][a-z0-9./-]{2,})")
            .unwrap()
    });

    let mut repaired = SCHEME_GAP.replace_all(text, "https://").to_string();
    repaired = DOMAIN_GAP.replace_all(&repaired, "arxiv.org").to_string();
    repaired = PATH_GAP.replace_all(&repaired, "$1$2").to_string();
    loop {
        let next = LINE_WRAP.replace_all(&repaired, "$1$2").to_string();
        if next == repaired {
            return repaired;
        }
        repaired = next;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fidelity_profile_preserves_dot_leaders() {
        let input = "Introduction............................1".to_string();
        let result = clean_markdown(input.clone(), &MarkdownOptions::default());
        assert_eq!(result, format!("{input}\n"));
    }

    #[test]
    fn compact_profile_collapses_dot_leaders() {
        let input = "Introduction............................1".to_string();
        let options = MarkdownOptions {
            profile: MarkdownProfile::Compact,
            ..MarkdownOptions::default()
        };
        assert_eq!(clean_markdown(input, &options), "Introduction ... 1\n");
    }

    // --- collapse_dot_leaders ---

    #[test]
    fn test_collapse_dot_leaders_four_or_more_dots() {
        assert_eq!(
            collapse_dot_leaders("Introduction............................1"),
            "Introduction ... 1"
        );
    }

    #[test]
    fn test_collapse_dot_leaders_three_dots_unchanged() {
        assert_eq!(collapse_dot_leaders("wait...what"), "wait...what");
    }

    #[test]
    fn test_collapse_dot_leaders_no_dots() {
        assert_eq!(collapse_dot_leaders("Hello World"), "Hello World");
    }

    #[test]
    fn test_collapse_dot_leaders_mixed() {
        let input = "Chapter 1.......10\nSome text... ok\nChapter 2........20";
        let result = collapse_dot_leaders(input);
        assert!(result.contains("Chapter 1 ... 10"));
        assert!(result.contains("Some text... ok"));
        assert!(result.contains("Chapter 2 ... 20"));
    }

    // --- remove_spaces_before_closing_brackets ---

    #[test]
    fn test_remove_spaces_before_closing_brackets() {
        let mut input = "Density [kg/m3 ] and [linked text ](https://example.com)".to_string();
        remove_spaces_before_closing_brackets(&mut input);
        assert_eq!(
            input,
            "Density [kg/m3] and [linked text](https://example.com)"
        );
    }

    // --- remove_spaces_before_sentence_punctuation ---

    #[test]
    fn strips_space_before_trailing_period() {
        let mut t = "Foreign insurance companies . The provisions".to_string();
        remove_spaces_before_sentence_punctuation(&mut t);
        assert_eq!(t, "Foreign insurance companies. The provisions");
    }

    #[test]
    fn strips_space_before_period_at_cell_boundary() {
        let mut t = "|Applicability date .|This section|".to_string();
        remove_spaces_before_sentence_punctuation(&mut t);
        assert_eq!(t, "|Applicability date.|This section|");
    }

    #[test]
    fn keeps_dot_leaders_and_ellipses() {
        let mut t = "Introduction ... 1".to_string();
        remove_spaces_before_sentence_punctuation(&mut t);
        assert_eq!(t, "Introduction ... 1");
    }

    #[test]
    fn keeps_mid_token_periods() {
        let mut t = "version 3 .14 released".to_string();
        remove_spaces_before_sentence_punctuation(&mut t);
        assert_eq!(t, "version 3 .14 released");
    }

    // --- fix_hyphenation ---

    #[test]
    fn test_fix_hyphenation_spaced_hyphen() {
        assert_eq!(fix_hyphenation("Limoeiro - Norte"), "Limoeiro-Norte");
    }

    #[test]
    fn test_fix_hyphenation_list_item_unchanged() {
        assert_eq!(
            fix_hyphenation("- item one\n- item two"),
            "- item one\n- item two"
        );
    }

    #[test]
    fn test_fix_hyphenation_accented_chars() {
        assert_eq!(fix_hyphenation("São - Paulo"), "São-Paulo");
    }

    #[test]
    fn test_fix_hyphenation_multiple_instances() {
        assert_eq!(
            fix_hyphenation("one - two and three - four"),
            "one-two and three-four"
        );
    }

    // --- is_page_number_line ---

    #[test]
    fn test_is_page_number_digits_1_to_4() {
        assert!(is_page_number_line("1"));
        assert!(is_page_number_line("42"));
        assert!(is_page_number_line("123"));
        assert!(is_page_number_line("9999"));
        assert!(!is_page_number_line("12345"));
    }

    #[test]
    fn test_is_page_number_page_x() {
        assert!(is_page_number_line("Page 5"));
        assert!(is_page_number_line("page 12"));
        assert!(is_page_number_line("Page123"));
    }

    #[test]
    fn test_is_page_number_page_x_of_y() {
        assert!(is_page_number_line("Page 3 of 10"));
        assert!(is_page_number_line("page 1 of 5"));
        assert!(is_page_number_line("Page 3 of 10 Report header"));
    }

    #[test]
    fn test_is_page_number_x_of_y() {
        assert!(is_page_number_line("3 of 10"));
    }

    #[test]
    fn test_is_page_number_centered_dash() {
        assert!(is_page_number_line("- 5 -"));
        assert!(is_page_number_line("-12-"));
    }

    #[test]
    fn test_is_page_number_page_of() {
        assert!(is_page_number_line("Page of"));
        assert!(is_page_number_line("page of 10"));
    }

    #[test]
    fn test_is_page_number_empty() {
        assert!(!is_page_number_line(""));
    }

    #[test]
    fn remove_page_numbers_uses_front_matter_to_filter_academic_running_headers() {
        let input = "## A Formulation of Recursive Self-Improvement\n\n#### Wenyi Wang\n\nBody\n\n2 Wenyi Wang\n\nMore body\n\nA Formulation of Recursive Self-Improvement 3";
        let result = remove_page_numbers(input);
        assert!(result.contains("## A Formulation"));
        assert!(result.contains("#### Wenyi Wang"));
        assert!(!result.contains("2 Wenyi Wang"));
        assert!(!result.contains("Improvement 3"));
    }

    #[test]
    fn test_is_page_number_non_match() {
        assert!(!is_page_number_line("Hello World"));
        assert!(!is_page_number_line("Chapter 1"));
        assert!(!is_page_number_line("Total: 500"));
    }

    #[test]
    fn test_is_page_number_labeled_running_header() {
        assert!(is_page_number_line("Page 42 Chapter 5"));
        assert!(is_page_number_line("Page 42 explains the result"));
    }

    // --- remove_page_numbers ---

    #[test]
    fn test_remove_page_numbers_isolated_number() {
        let input = "Some text\n\n42\n\nMore text";
        let result = remove_page_numbers(input);
        assert!(!result.contains("\n42\n"));
        assert!(result.contains("Some text"));
        assert!(result.contains("More text"));
    }

    #[test]
    fn test_remove_page_numbers_before_break() {
        let input = "Content\n\n5\n---\nNext page";
        let result = remove_page_numbers(input);
        assert!(!result.contains("\n5\n"));
    }

    #[test]
    fn test_remove_page_numbers_in_context_kept() {
        let input = "Line A\nLine B\n42\nLine C\nLine D";
        let result = remove_page_numbers(input);
        assert!(result.contains("42"));
    }

    #[test]
    fn test_remove_page_numbers_labeled_header_with_content() {
        let input = "Content\n\nPage 42 explains the result\n---\nEnd";
        let result = remove_page_numbers(input);

        assert!(!result.contains("Page 42 explains the result"));
        assert!(result.contains("Content"));
        assert!(result.contains("End"));
    }

    #[test]
    fn test_remove_page_numbers_multiple_patterns() {
        let input = "\n1\n\nContent\n\n2\n\n---\nMore\n\n3\n";
        let result = remove_page_numbers(input);
        assert!(!result.contains("\n1\n"));
        assert!(!result.contains("\n2\n"));
        assert!(!result.contains("\n3\n"));
    }

    #[test]
    fn test_remove_page_numbers_empty() {
        assert_eq!(remove_page_numbers(""), "");
    }

    // --- format_urls ---

    #[test]
    fn test_format_urls_bare_url() {
        let result = format_urls("Visit https://example.com for info");
        assert!(result.contains("[https://example.com](https://example.com)"));
    }

    #[test]
    fn test_format_urls_already_linked() {
        let input = "[click](https://example.com)";
        assert_eq!(format_urls(input), input);
    }

    #[test]
    fn test_format_urls_inside_brackets() {
        let input = "[https://example.com](https://example.com)";
        let result = format_urls(input);
        assert!(!result.contains("[["));
    }

    #[test]
    fn test_format_urls_multiple() {
        let input = "See https://a.com and https://b.com";
        let result = format_urls(input);
        assert!(result.contains("[https://a.com](https://a.com)"));
        assert!(result.contains("[https://b.com](https://b.com)"));
    }

    #[test]
    fn test_format_urls_no_urls() {
        let input = "No links here";
        assert_eq!(format_urls(input), input);
    }

    #[test]
    fn repairs_arxiv_urls_split_by_layout_lines_and_spacing() {
        let input = "URL https://arxiv.org/abs/2604\n\n.23472.\nURL https: //arxiv.org/abs/2606.1\nURL https://arxiv.or g/abs/2605.2\nURL https://arxiv.org/ abs/2606.30774";
        let repaired = repair_wrapped_arxiv_urls(input);
        assert!(repaired.contains("https://arxiv.org/abs/2604.23472."));
        assert!(repaired.contains("https://arxiv.org/abs/2606.1"));
        assert!(repaired.contains("https://arxiv.org/abs/2605.2"));
        assert!(repaired.contains("https://arxiv.org/abs/2606.30774"));
    }

    #[test]
    fn refine_heading_blocks_promotes_strong_bold_blocks() {
        let input = "**IMPLEMENTATION**\n\nBody\n\n**1.5. Migrant Workers at Risk**";
        let result = refine_heading_blocks(input);
        assert!(result.contains("# IMPLEMENTATION"));
        assert!(result.contains("# 1.5. Migrant Workers at Risk"));
    }

    #[test]
    fn refine_heading_blocks_demotes_visual_body_lines() {
        let input = "# False Causation\n\n## Correlation does not imply causation.\n\n# <u>Reference frameworks:</u>\n\n## and call it Synth.";
        let result = refine_heading_blocks(input);
        assert!(result.contains("# False Causation"));
        assert!(result.contains("\nCorrelation does not imply causation."));
        assert!(result.contains("# <u>Reference frameworks:</u>"));
        assert!(result.contains("\nand call it Synth."));
    }

    #[test]
    fn refine_heading_blocks_preserves_roman_and_mixed_bold_text() {
        let input = "# Section\n\n# III.\n\n**Project No:** : **123**";
        let result = refine_heading_blocks(input);
        assert!(result.contains("# III."));
        assert!(result.contains("**Project No:** : **123**"));
    }

    #[test]
    fn refine_heading_blocks_does_not_promote_contents_entries() {
        let input =
            "# Contents\n\n**1. Overview**\n**2. FAQ**\n\n# Chapter One\n\n**IMPLEMENTATION**";
        let result = refine_heading_blocks(input);
        assert!(result.contains("**1. Overview**"));
        assert!(result.contains("**2. FAQ**"));
        assert!(result.contains("# IMPLEMENTATION"));
    }

    #[test]
    fn trailing_toc_part_headings_move_before_their_sections() {
        let input = "# Contents\n\nIntroduction\t1\nSection 1.1: Data\t3\nSection 1.2: Tests\t5\nSection 2.1: Values\t12\nSection 2.2: Effects\t16\n\nPart I. <u>Chapter One-Exploring Data</u>\n\n# Part II. <u>Chapter Two-Test Statistics</u>";
        let result = repair_trailing_toc_part_headings(input);

        assert!(result.find("Part I.").unwrap() < result.find("Section 1.1").unwrap());
        assert!(result.find("Part II.").unwrap() < result.find("Section 2.1").unwrap());
        assert!(result.find("Section 1.2").unwrap() < result.find("Part II.").unwrap());
    }

    #[test]
    fn correctly_ordered_toc_part_headings_are_unchanged() {
        let input = "# Contents\n\nPart I. Chapter One\nSection 1.1: Data\t3\n\nPart II. Chapter Two\nSection 2.1: Values\t12";
        assert_eq!(repair_trailing_toc_part_headings(input), input);
    }

    #[test]
    fn toc_continuation_repairs_three_trailing_part_headings() {
        let input = "# Part V. Chapter Five\nSection 5.1: Model\t35\n# Part VI. Chapter Six\nSection 6.1: Groups\t49\nSection 7.1: Mediation\t64\nSection 8.1: Factors\t75\nSection 9.1: Tests\t91\n\n# Part VII. Chapter Seven\n# Part VIII. Chapter Eight\n# Part IX. Chapter Nine";
        let result = repair_trailing_toc_part_headings(input);

        assert!(result.find("Part V.").unwrap() < result.find("Section 5.1").unwrap());
        assert!(result.find("Part VI.").unwrap() < result.find("Section 6.1").unwrap());
        assert!(result.find("Part VII.").unwrap() < result.find("Section 7.1").unwrap());
        assert!(result.find("Part VIII.").unwrap() < result.find("Section 8.1").unwrap());
        assert!(result.find("Part IX.").unwrap() < result.find("Section 9.1").unwrap());
    }

    #[test]
    fn refine_heading_blocks_demotes_long_sentences_and_numeric_callouts() {
        let input = "# This page provides a record of edits and changes made to this book since its initial publication.\n\n#### 14.3%↑\n\n# 1.7X↑";
        let result = refine_heading_blocks(input);
        assert!(!result.contains("# This page"));
        assert!(!result.contains("#### 14.3%↑"));
        assert!(!result.contains("# 1.7X↑"));
    }

    #[test]
    fn refine_heading_blocks_demotes_wrapped_chinese_quote_run() {
        let input = "# 第二章\n\n##### 禱告婆那種冗長的祈禱跟迷信一樣，它裡面不涉及任何屬靈\n\n##### 爭戰或聖經基礎，只是那些禱告婆自吹自擂罷了，和它比起\n\n##### 來，靈裡宣告要更根本些，靈裡宣告是去釋放原本就屬你的\n\n##### 財富，財富的轉換甚麼時候發生，是上帝來決定的\n\n##### 四、結語";
        let result = refine_heading_blocks(input);
        assert!(result.contains("# 第二章"));
        assert!(!result.contains("##### 禱告婆"));
        assert!(!result.contains("##### 爭戰"));
        assert!(result.contains("##### 四、結語"));
    }

    #[test]
    fn refine_heading_blocks_preserves_structural_heading_run() {
        let input = "##### 一、全球化與中國化交織張力下的發展路徑\n\n##### 二、研究視角與方法論的歷史背景和主要脈絡\n\n##### 三、定義與本書梗概以及各章節的主要內容\n\n##### 四、結語與後續研究方向的總體概述";
        assert_eq!(refine_heading_blocks(input), input);
    }

    #[test]
    fn refine_heading_blocks_separates_glued_academic_section_numbers() {
        let input = "### 1Introduction\n\n#### 3.2Decode Codesign\n\n#### Qwen3.6-35B-A3B\n\nBody 3.2Decode";
        let result = refine_heading_blocks(input);
        assert!(result.contains("### 1 Introduction"));
        assert!(result.contains("#### 3.2 Decode Codesign"));
        assert!(result.contains("#### Qwen3.6-35B-A3B"));
        assert!(result.contains("Body 3.2Decode"));
    }

    #[test]
    fn refine_heading_blocks_demotes_equations_and_deduplicates_references() {
        let input = "#### M=F ∪C, q=|F|.(1)\n\n### References\n\n### References\n\n[1] Citation";
        let result = refine_heading_blocks(input);
        assert!(result.starts_with("M=F ∪C, q=|F|.(1)"));
        assert_eq!(result.matches("### References").count(), 1);
        assert!(result.contains("[1] Citation"));
    }

    #[test]
    fn refine_table_lines_repairs_tracked_caps_and_outcomes_row() {
        let input = "|Competence Area|#1 T HE 3 R S : R ECYCLE -R EUSE -R EDUCE|\n|---|---|\n|Competence Statement Learning Outcomes|Details|";
        let result = refine_table_lines(input);
        assert!(result.contains("|Competence Area|#1 THE 3 RS: RECYCLE-REUSE-REDUCE|"));
        assert!(result.contains("|Competence Statement|Details|"));
        assert!(result.contains("|Learning Outcomes||"));
    }

    #[test]
    fn refine_table_lines_leaves_ordinary_cells_unchanged() {
        let input = "| Initials | A B |\n|---|---|";
        assert_eq!(refine_table_lines(input), input);
    }

    #[test]
    fn refine_table_lines_restores_sparse_traditional_chinese_prose_columns() {
        let input = "|道家經典中，都沒有對善惡的清晰劃分。|中國傳統強調禮，這段論述仍在下一行延續||\n|---|---|---|---|\n|使得善惡間的區分完全模糊了，並形成一段連續正文|而對於佛教徒來說，儘管行善是重要實踐||\n|得善報的關鍵，但它並不被看作是一個終極目標|||基督教強勢有|";
        let result = refine_table_lines(input);
        assert!(!result.contains("|---|"));
        assert!(result.starts_with("道家經典中"));
        assert!(result.contains("終極目標\n\n中國傳統"));
        assert!(result.ends_with("基督教強勢有"));
    }

    #[test]
    fn refine_table_lines_demotes_sparse_display_equation_not_real_table() {
        let input = "||B = max(B||−B ,0)(2)|||\n|---|---|---|---|---|---|\n||R|H|P|||\n|fill ⋆|P H|cpu P P|⋆ H|H P P H|P ⋆|";
        let result = refine_table_lines(input);
        assert!(!result.contains("|---|"));
        assert!(result.contains("B = max(B −B ,0)(2)"));
        assert!(result.contains("R H P"));
        assert!(result.contains("fill ⋆ P H cpu P P ⋆ H H P P H P ⋆"));
    }

    #[test]
    fn refine_table_lines_keeps_real_table_with_formula_cells() {
        let input = "|Method|Equation|Score|Notes|\n|---|---|---|---|\n|A|q=m×B/B|(2)|stable|";
        assert_eq!(refine_table_lines(input), input);
    }
}
