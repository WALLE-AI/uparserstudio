//! Parsers for each protocol's raw model-output encoding, per
//! ARCHITECTURE.md §2.0 (`RawOutputFormat`). `custom_token` (T-1.4) is
//! mineru-vlm's stage-1 layout grammar; `strict_json` (T-2.2) is
//! dots.ocr's single-round cell array, with a fault-tolerant repair
//! chain ported from `output_cleaner.py`.

use regex::Regex;
use serde_json::Value;
use std::sync::LazyLock;

/// A parsed stage-1 layout line, before category mapping or coordinate
/// denormalization.
#[derive(Debug, Clone, PartialEq)]
pub struct LayoutBox {
    /// Raw `[0,1000]` integer coordinates, `[x1, y1, x2, y2]`, sorted
    /// min/max per axis.
    pub bbox_1000: [u32; 4],
    pub category_raw: String,
    pub angle: Option<u32>,
}

static LAYOUT_LINE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^<\|box_start\|>(\d+)\s+(\d+)\s+(\d+)\s+(\d+)<\|box_end\|><\|ref_start\|>(\w+?)<\|ref_end\|>(.*)$",
    )
    .expect("static regex is valid")
});

/// Fallback for a line that doesn't fit `LAYOUT_LINE_RE`'s exact,
/// anchored shape but still plausibly contains a real box: no `^...$`
/// anchor (so leading/trailing junk around the tag sequence doesn't
/// reject the whole line), and `<|ref_end|>` is optional (a truncated
/// generation can cut off exactly at the closing tag while everything
/// needed to build a valid box — coordinates, category — is still
/// intact). Previously `custom_token` had **zero** rescue levels (unlike
/// `strict_json`'s 5 and `python_literal`'s 2) — one unparseable line
/// meant that box was gone, even when most of it was salvageable (see
/// D.11 in `CLI_ENHANCEMENT_PROPOSAL.md`).
static LAYOUT_LINE_RELAXED_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        // Category is greedy (`\w+`, not `\w+?`) here — with the
        // trailing `<|ref_end|>` made optional, a lazy quantifier could
        // satisfy the whole pattern by capturing just one character of
        // the category and letting the rest fall into the trailing
        // `(.*)`, since matching zero `<|ref_end|>` occurrences is
        // always valid. The strict regex avoids this because `<|ref_end|>`
        // is mandatory there, forcing the lazy category match to expand
        // until it's found.
        r"<\|box_start\|>\s*(\d+)\s+(\d+)\s+(\d+)\s+(\d+)\s*<\|box_end\|>\s*<\|ref_start\|>\s*(\w+)\s*(?:<\|ref_end\|>)?(.*)",
    )
    .expect("static regex is valid")
});

/// MinerU >=1.0.5's exact layout matcher. Unlike the enhanced line parser
/// below, this scans the whole response, requires every closing token, and
/// lets the tail span newlines until the next box. Keeping it separate makes
/// official-parity experiments reproducible without removing fault recovery
/// from the normal `mineru-vlm` protocol.
static MINERU_OFFICIAL_LAYOUT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"<\|box_start\|>(\d+)\s+(\d+)\s+(\d+)\s+(\d+)<\|box_end\|><\|ref_start\|>(\w+?)<\|ref_end\|>(?:(<\|rotate_(?:up|right|down|left)\|>))?",
    )
    .expect("static regex is valid")
});

/// Parse with MinerU 1.0.5's strict whole-response semantics. Coordinates
/// outside `[0,1000]` are rejected rather than clamped.
pub fn parse_custom_tokens_official(raw: &str) -> (Vec<LayoutBox>, Vec<String>) {
    let mut boxes = Vec::new();
    let mut warnings = Vec::new();

    for caps in MINERU_OFFICIAL_LAYOUT_RE.captures_iter(raw) {
        let coords: Option<[u32; 4]> = (|| {
            Some([
                caps[1].parse().ok()?,
                caps[2].parse().ok()?,
                caps[3].parse().ok()?,
                caps[4].parse().ok()?,
            ])
        })();
        let Some([x1, y1, x2, y2]) = coords else {
            warnings.push(format!("unparseable coordinates: {:?}", &caps[0]));
            continue;
        };
        if [x1, y1, x2, y2].iter().any(|&coord| coord > 1000) {
            warnings.push(format!("out-of-range coordinates: {:?}", &caps[0]));
            continue;
        }

        let (xa, xb) = (x1.min(x2), x1.max(x2));
        let (ya, yb) = (y1.min(y2), y1.max(y2));
        if xa == xb || ya == yb {
            warnings.push(format!("degenerate box: {:?}", &caps[0]));
            continue;
        }

        boxes.push(LayoutBox {
            bbox_1000: [xa, ya, xb, yb],
            category_raw: caps[5].to_lowercase(),
            angle: caps.get(6).and_then(|m| parse_rotation(m.as_str())),
        });
    }

    if boxes.is_empty() && !raw.trim().is_empty() {
        warnings.push("layout output does not match official MinerU grammar".to_string());
    }
    (boxes, warnings)
}

/// Parse mineru-vlm's stage-1 `custom_token` output: one candidate block
/// per line. Malformed lines, out-of-range/degenerate boxes are skipped
/// (recorded as warnings) rather than failing the whole page — mirrors
/// the confirmed v0.1.14 behavior.
pub fn parse_custom_tokens(raw: &str) -> (Vec<LayoutBox>, Vec<String>) {
    let mut boxes = Vec::new();
    let mut warnings = Vec::new();

    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (caps, rescued) = match LAYOUT_LINE_RE.captures(line) {
            Some(caps) => (caps, false),
            None => match LAYOUT_LINE_RELAXED_RE.captures(line) {
                Some(caps) => (caps, true),
                None => {
                    warnings.push(format!("unparseable layout line: {line:?}"));
                    continue;
                }
            },
        };
        if rescued {
            warnings.push(format!(
                "rescued a malformed layout line via relaxed matching: {line:?}"
            ));
        }

        let coords: Option<[u32; 4]> = (|| {
            Some([
                caps[1].parse().ok()?,
                caps[2].parse().ok()?,
                caps[3].parse().ok()?,
                caps[4].parse().ok()?,
            ])
        })();
        let Some([x1, y1, x2, y2]) = coords else {
            warnings.push(format!("unparseable coordinates: {line:?}"));
            continue;
        };

        // VLM quantization error nudging a coordinate 1-5 units past
        // 1000 at a page edge is common and recoverable — clamp rather
        // than discarding the whole box (see D.2 in
        // `CLI_ENHANCEMENT_PROPOSAL.md`). Only warn if a coordinate
        // actually needed clamping, so well-formed lines stay silent.
        let clamped = [x1.min(1000), y1.min(1000), x2.min(1000), y2.min(1000)];
        if clamped != [x1, y1, x2, y2] {
            warnings.push(format!(
                "clamped out-of-range coordinate(s) to [0,1000]: {line:?}"
            ));
        }
        let [x1, y1, x2, y2] = clamped;

        let (xa, xb) = (x1.min(x2), x1.max(x2));
        let (ya, yb) = (y1.min(y2), y1.max(y2));
        if xa == xb || ya == yb {
            warnings.push(format!("degenerate box: {line:?}"));
            continue;
        }

        let category_raw = caps[5].to_lowercase();
        let tail = &caps[6];
        let angle = parse_rotation(tail);

        boxes.push(LayoutBox {
            bbox_1000: [xa, ya, xb, yb],
            category_raw,
            angle,
        });
    }

    (boxes, warnings)
}

fn parse_rotation(tail: &str) -> Option<u32> {
    if tail.contains("<|rotate_up|>") {
        Some(0)
    } else if tail.contains("<|rotate_right|>") {
        Some(90)
    } else if tail.contains("<|rotate_down|>") {
        Some(180)
    } else if tail.contains("<|rotate_left|>") {
        Some(270)
    } else {
        None
    }
}

/// A parsed dots.ocr cell, before category mapping or coordinate
/// rescaling.
#[derive(Debug, Clone, PartialEq)]
pub struct DotsCell {
    pub bbox: [f32; 4],
    pub category_raw: String,
    pub text: Option<String>,
}

static DICT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"\{[^{}]*?"bbox"\s*:\s*\[[^\]]*?\][^{}]*?\}"#).expect("valid regex")
});

// The real `output_cleaner.py` pattern is `\}\s*\{(?!")`, a negative
// lookahead the `regex` crate can't express. That lookahead would
// actually skip the common case of two dicts adjacent as `}{"bbox":...`
// (the char after `{` almost always *is* a quote), so a plain `\}\s*\{`
// match is arguably more useful here anyway — documented deviation.
static MISSING_DELIMITER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\}\s*\{").expect("valid regex"));

static BBOX_FIELD_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#""bbox"\s*:\s*\[([^\]]+)\]"#).expect("valid regex"));
static CATEGORY_FIELD_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#""category"\s*:\s*"([^"]+)""#).expect("valid regex"));
static TEXT_FIELD_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#""text"\s*:\s*"([^"]{0,10000})"#).expect("valid regex"));

/// Parse dots.ocr's `strict_json` stage output: a JSON array of
/// `{bbox, category, text}` cells. Ports `output_cleaner.py`'s repair
/// chain for malformed/truncated output: fix missing `},{'` delimiters
/// → drop an incomplete trailing element → dedupe exact-duplicate dicts
/// → ensure `[...]` wrapping → parse, with two further fallbacks
/// (per-dict extraction, then a single-incomplete-dict rescue) if that
/// still fails.
pub fn parse_strict_json(raw: &str) -> (Vec<DotsCell>, Vec<String>) {
    let mut warnings = Vec::new();

    if let Some(values) = try_parse_array(raw) {
        return (cells_from_values(values, &mut warnings), warnings);
    }

    let (fixed, delim_fixes) = fix_missing_delimiters(raw);
    if delim_fixes > 0 {
        warnings.push(format!("fixed {delim_fixes} missing delimiter(s)"));
    }
    let (truncated, was_truncated) = truncate_incomplete_tail(&fixed);
    if was_truncated {
        warnings.push("truncated an incomplete trailing element".to_string());
    }
    let (deduped, dup_count) = dedupe_complete_dicts(&truncated);
    if dup_count > 0 {
        warnings.push(format!("removed {dup_count} duplicate dict(s)"));
    }
    let ensured = ensure_json_array(&deduped);

    if let Some(values) = try_parse_array(&ensured) {
        return (cells_from_values(values, &mut warnings), warnings);
    }

    let extracted = extract_valid_dicts(&ensured);
    if !extracted.is_empty() {
        warnings.push(format!(
            "recovered {} dict(s) via per-dict extraction",
            extracted.len()
        ));
        return (cells_from_values(extracted, &mut warnings), warnings);
    }

    if let Some(rescued) = rescue_single_incomplete_dict(&ensured) {
        warnings.push("rescued a single incomplete dict".to_string());
        return (cells_from_values(rescued, &mut warnings), warnings);
    }

    warnings.push("all fault-tolerant recovery strategies failed".to_string());
    (Vec::new(), warnings)
}

fn try_parse_array(text: &str) -> Option<Vec<Value>> {
    serde_json::from_str::<Vec<Value>>(text).ok()
}

fn fix_missing_delimiters(text: &str) -> (String, usize) {
    let mut fixes = 0;
    let fixed = MISSING_DELIMITER_RE
        .replace_all(text, |_: &regex::Captures| {
            fixes += 1;
            "},{"
        })
        .to_string();
    (fixed, fixes)
}

fn truncate_incomplete_tail(text: &str) -> (String, bool) {
    let needs_truncation = text.len() > 50_000 || !text.trim_end().ends_with(']');
    if !needs_truncation {
        return (text.to_string(), false);
    }

    let bbox_count = text.matches(r#"{"bbox":"#).count();
    if bbox_count <= 1 {
        return (text.to_string(), false);
    }

    let Some(last_pos) = text.rfind(r#"{"bbox":"#) else {
        return (text.to_string(), false);
    };

    let mut truncated = text[..last_pos].trim_end().to_string();
    if truncated.ends_with(',') {
        truncated.pop();
    }
    (truncated, true)
}

fn dedupe_complete_dicts(text: &str) -> (String, usize) {
    let matches: Vec<&str> = DICT_RE.find_iter(text).map(|m| m.as_str()).collect();
    if matches.is_empty() {
        return (text.to_string(), 0);
    }

    let mut seen = std::collections::HashSet::new();
    let mut unique = Vec::new();
    let mut duplicates = 0;
    for m in &matches {
        if seen.insert(*m) {
            unique.push(*m);
        } else {
            duplicates += 1;
        }
    }

    if duplicates == 0 {
        return (text.to_string(), 0);
    }
    (format!("[{}]", unique.join(", ")), duplicates)
}

fn ensure_json_array(text: &str) -> String {
    let mut out = text.trim().to_string();
    if !out.starts_with('[') {
        out = format!("[{out}");
    }
    if !out.ends_with(']') {
        while out.ends_with(',') || out.ends_with(char::is_whitespace) {
            out.pop();
        }
        out.push(']');
    }
    out
}

fn extract_valid_dicts(text: &str) -> Vec<Value> {
    DICT_RE
        .find_iter(text)
        .filter_map(|m| serde_json::from_str::<Value>(m.as_str()).ok())
        .collect()
}

fn rescue_single_incomplete_dict(text: &str) -> Option<Vec<Value>> {
    if !text.trim_start().starts_with(r#"[{"bbox":"#) {
        return None;
    }

    let bbox_caps = BBOX_FIELD_RE.captures(text)?;
    let coords: Option<Vec<i64>> = bbox_caps[1]
        .split(',')
        .map(|s| s.trim().parse::<i64>().ok())
        .collect();
    let coords = coords?;
    if coords.len() != 4 {
        return None;
    }

    let category = CATEGORY_FIELD_RE
        .captures(text)
        .map(|c| c[1].to_string())
        .unwrap_or_else(|| "Text".to_string());
    let text_content = TEXT_FIELD_RE.captures(text).map(|c| c[1].to_string());

    let mut obj = serde_json::json!({
        "bbox": coords,
        "category": category,
    });
    if let Some(t) = text_content {
        obj["text"] = Value::String(t);
    }
    Some(vec![obj])
}

fn cells_from_values(values: Vec<Value>, warnings: &mut Vec<String>) -> Vec<DotsCell> {
    let mut cells = Vec::with_capacity(values.len());
    for value in values {
        let Some(obj) = value.as_object() else {
            warnings.push("dropped non-object array entry".to_string());
            continue;
        };

        let bbox = obj.get("bbox").and_then(|b| b.as_array());
        let Some(bbox) = bbox else {
            warnings.push("dropped cell missing bbox".to_string());
            continue;
        };
        if bbox.len() != 4 {
            warnings.push(format!("dropped cell with {}-element bbox", bbox.len()));
            continue;
        }
        let Some(bbox): Option<Vec<f32>> =
            bbox.iter().map(|v| v.as_f64().map(|f| f as f32)).collect()
        else {
            warnings.push("dropped cell with non-numeric bbox".to_string());
            continue;
        };

        let category_raw = obj
            .get("category")
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .to_string();
        let text = obj
            .get("text")
            .and_then(|t| t.as_str())
            .map(|s| s.to_string());

        cells.push(DotsCell {
            bbox: [bbox[0], bbox[1], bbox[2], bbox[3]],
            category_raw,
            text,
        });
    }
    cells
}

/// A parsed MonkeyOCRv2 cell, before category mapping or coordinate
/// mapping.
#[derive(Debug, Clone, PartialEq)]
pub struct MonkeyCell {
    pub bbox: [f32; 4],
    pub label: String,
    pub content: Option<String>,
}

/// Hand-written recursive-descent parser for the Python literal subset
/// MonkeyOCRv2 actually emits (lists, dicts with string keys,
/// single/double-quoted strings, numbers, `True`/`False`/`None`).
/// Deliberately **not** an `eval` — the real service parses via
/// `eval(text, {"__builtins__": {}}, {})`; we never invoke an
/// interpreter, satisfying T-3.3 by construction.
struct LiteralParser {
    chars: Vec<char>,
    pos: usize,
}

impl LiteralParser {
    fn new(s: &str) -> Self {
        Self {
            chars: s.chars().collect(),
            pos: 0,
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(c) if c.is_whitespace()) {
            self.pos += 1;
        }
    }

    fn matches_keyword(&self, kw: &str) -> bool {
        let kw_chars: Vec<char> = kw.chars().collect();
        if self.pos + kw_chars.len() > self.chars.len() {
            return false;
        }
        if self.chars[self.pos..self.pos + kw_chars.len()] != kw_chars[..] {
            return false;
        }
        match self.chars.get(self.pos + kw_chars.len()) {
            Some(c) => !(c.is_alphanumeric() || *c == '_'),
            None => true,
        }
    }

    fn parse_value(&mut self) -> Result<Value, String> {
        self.skip_ws();
        match self.peek() {
            Some('[') => self.parse_list(),
            Some('{') => self.parse_dict(),
            Some('\'') | Some('"') => self.parse_string().map(Value::String),
            Some('T') if self.matches_keyword("True") => {
                self.pos += 4;
                Ok(Value::Bool(true))
            }
            Some('F') if self.matches_keyword("False") => {
                self.pos += 5;
                Ok(Value::Bool(false))
            }
            Some('N') if self.matches_keyword("None") => {
                self.pos += 4;
                Ok(Value::Null)
            }
            Some(c) if c == '-' || c.is_ascii_digit() => self.parse_number(),
            other => Err(format!(
                "unexpected token {other:?} at position {}",
                self.pos
            )),
        }
    }

    fn parse_string(&mut self) -> Result<String, String> {
        let quote = self.peek().ok_or("unexpected end of input in string")?;
        self.pos += 1;
        let mut out = String::new();
        loop {
            let c = self
                .peek()
                .ok_or_else(|| "unterminated string literal".to_string())?;
            self.pos += 1;
            if c == '\\' {
                let esc = self
                    .peek()
                    .ok_or_else(|| "unterminated escape sequence".to_string())?;
                self.pos += 1;
                match esc {
                    'n' => out.push('\n'),
                    't' => out.push('\t'),
                    'r' => out.push('\r'),
                    '\\' => out.push('\\'),
                    '\'' => out.push('\''),
                    '"' => out.push('"'),
                    'u' => self.decode_hex_escape('u', 4, &mut out),
                    'x' => self.decode_hex_escape('x', 2, &mut out),
                    // An escape sequence this parser doesn't recognize
                    // (e.g. a Chinese-punctuation-adjacent `\，` from a
                    // model that over-escapes, or any other unknown
                    // letter) — previously this silently dropped the
                    // backslash, which for `\u`/`\x` corrupted real
                    // text into raw hex digits (see D.3). Preserving
                    // the backslash keeps the sequence recognizable
                    // instead of fabricating a different character.
                    other => {
                        out.push('\\');
                        out.push(other);
                    }
                }
            } else if c == quote {
                break;
            } else {
                out.push(c);
            }
        }
        Ok(out)
    }

    /// Decode a `\uXXXX` (`digits == 4`) or `\xHH` (`digits == 2`) escape
    /// starting right after the already-consumed `\u`/`\x` marker. On any
    /// failure (too few hex digits, invalid hex, or a value that isn't a
    /// valid Unicode scalar — e.g. an unpaired surrogate half) falls back
    /// to emitting the marker literally (`\u`/`\x`) rather than silently
    /// dropping or mis-decoding it, and does not consume the trailing
    /// characters so they're re-parsed as plain text.
    fn decode_hex_escape(&mut self, marker: char, digits: usize, out: &mut String) {
        let end = (self.pos + digits).min(self.chars.len());
        let hex: String = self.chars[self.pos..end].iter().collect();
        let decoded = if hex.len() == digits {
            u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32)
        } else {
            None
        };
        match decoded {
            Some(ch) => {
                out.push(ch);
                self.pos = end;
            }
            None => {
                out.push('\\');
                out.push(marker);
            }
        }
    }

    fn parse_number(&mut self) -> Result<Value, String> {
        let start = self.pos;
        if self.peek() == Some('-') {
            self.pos += 1;
        }
        while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
            self.pos += 1;
        }
        if self.peek() == Some('.') {
            self.pos += 1;
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                self.pos += 1;
            }
        }
        let s: String = self.chars[start..self.pos].iter().collect();
        let f: f64 = s.parse().map_err(|_| format!("invalid number {s:?}"))?;
        serde_json::Number::from_f64(f)
            .map(Value::Number)
            .ok_or_else(|| format!("non-finite number {s:?}"))
    }

    fn parse_list(&mut self) -> Result<Value, String> {
        self.pos += 1; // consume '['
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(']') {
            self.pos += 1;
            return Ok(Value::Array(items));
        }
        loop {
            items.push(self.parse_value()?);
            self.skip_ws();
            match self.peek() {
                Some(',') => {
                    self.pos += 1;
                    self.skip_ws();
                    if self.peek() == Some(']') {
                        self.pos += 1;
                        break;
                    }
                }
                Some(']') => {
                    self.pos += 1;
                    break;
                }
                other => {
                    return Err(format!(
                        "expected ',' or ']', got {other:?} at position {}",
                        self.pos
                    ));
                }
            }
        }
        Ok(Value::Array(items))
    }

    fn parse_dict(&mut self) -> Result<Value, String> {
        self.pos += 1; // consume '{'
        let mut map = serde_json::Map::new();
        self.skip_ws();
        if self.peek() == Some('}') {
            self.pos += 1;
            return Ok(Value::Object(map));
        }
        loop {
            self.skip_ws();
            let key = match self.peek() {
                Some('\'') | Some('"') => self.parse_string()?,
                other => {
                    return Err(format!(
                        "expected string key, got {other:?} at position {}",
                        self.pos
                    ));
                }
            };
            self.skip_ws();
            if self.peek() != Some(':') {
                return Err(format!("expected ':' at position {}", self.pos));
            }
            self.pos += 1;
            let value = self.parse_value()?;
            map.insert(key, value);
            self.skip_ws();
            match self.peek() {
                Some(',') => {
                    self.pos += 1;
                    self.skip_ws();
                    if self.peek() == Some('}') {
                        self.pos += 1;
                        break;
                    }
                }
                Some('}') => {
                    self.pos += 1;
                    break;
                }
                other => {
                    return Err(format!(
                        "expected ',' or '}}', got {other:?} at position {}",
                        self.pos
                    ));
                }
            }
        }
        Ok(Value::Object(map))
    }
}

/// Parse a Python-literal value (list/dict/string/number/bool/None),
/// requiring the whole input to be consumed.
pub fn parse_py_literal(s: &str) -> Result<Value, String> {
    let mut parser = LiteralParser::new(s);
    let value = parser.parse_value()?;
    parser.skip_ws();
    if parser.pos != parser.chars.len() {
        return Err(format!("trailing content at position {}", parser.pos));
    }
    Ok(value)
}

fn normalize_monkey_item(v: &Value) -> Option<MonkeyCell> {
    let obj = v.as_object()?;
    let bbox = obj.get("bbox")?.as_array()?;
    if bbox.len() != 4 {
        return None;
    }
    let bbox: Vec<f32> = bbox
        .iter()
        .map(|x| x.as_f64().map(|f| f as f32))
        .collect::<Option<Vec<_>>>()?;
    let label = obj.get("label")?.as_str()?.to_string();
    let content = obj
        .get("content")
        .and_then(|c| c.as_str())
        .map(|s| s.to_string());
    Some(MonkeyCell {
        bbox: [bbox[0], bbox[1], bbox[2], bbox[3]],
        label,
        content,
    })
}

fn normalize_monkey_list(v: &Value) -> Vec<MonkeyCell> {
    v.as_array()
        .map(|arr| arr.iter().filter_map(normalize_monkey_item).collect())
        .unwrap_or_default()
}

fn dedup_keep_order(seq: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for s in seq {
        if seen.insert(s.clone()) {
            out.push(s);
        }
    }
    out
}

/// Bracket-depth scan for balanced substrings delimited by `lch`/`rch`.
/// Deliberately **not** string-aware (matches the real
/// `_extract_balanced_blocks`'s naive behavior — a faithful port, not an
/// improvement).
fn extract_balanced_blocks(text: &str, lch: char, rch: char) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut res = Vec::new();
    let mut depth = 0i32;
    let mut start: Option<usize> = None;
    for (i, &c) in chars.iter().enumerate() {
        if c == lch {
            if depth == 0 {
                start = Some(i);
            }
            depth += 1;
        } else if c == rch && depth > 0 {
            depth -= 1;
            if depth == 0
                && let Some(s) = start
            {
                res.push(chars[s..=i].iter().collect());
                start = None;
            }
        }
    }
    res
}

fn extract_tolerant_list_blocks(text: &str) -> Vec<String> {
    let mut blocks = extract_balanced_blocks(text, '[', ']');
    if let Some(first) = text.find('[') {
        let tail = text[first..].trim();
        if !tail.is_empty() {
            let lcnt = tail.matches('[').count();
            let rcnt = tail.matches(']').count();
            let mut tail = tail.to_string();
            if lcnt > rcnt {
                tail.push_str(&"]".repeat(lcnt - rcnt));
            }
            blocks.push(tail);
        }
    }
    dedup_keep_order(blocks)
}

fn extract_tolerant_dict_blocks(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut blocks = Vec::new();
    for i in 0..n {
        if chars[i] != '{' {
            continue;
        }
        let mut depth = 0i32;
        let mut end = None;
        for (j, &cj) in chars.iter().enumerate().skip(i) {
            match cj {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(j + 1);
                        break;
                    }
                }
                _ => {}
            }
        }
        let blk = match end {
            Some(e) => chars[i..e].iter().collect::<String>(),
            None => {
                let mut s: String = chars[i..].iter().collect();
                s.push_str(&"}".repeat(depth.max(1) as usize));
                s
            }
        };
        blocks.push(blk);
    }
    dedup_keep_order(blocks)
}

/// Parse MonkeyOCRv2's `python_literal_eval` stage output: a Python
/// literal list of `{bbox, label, content}` cells. Ports
/// `_parse_one_output`'s recovery algorithm: try a full parse first;
/// on failure/empty result, bracket-scan for balanced `[...]` substrings
/// (tolerating a truncated tail via bracket completion) and balanced
/// `{...}` substrings (same tolerance), and keep whichever candidate
/// recovered the most valid cells.
pub fn parse_python_literal_list(raw: &str) -> (Vec<MonkeyCell>, Vec<String>) {
    let text = raw.trim();
    let mut warnings = Vec::new();
    if text.is_empty() {
        return (Vec::new(), warnings);
    }

    if let Ok(v) = parse_py_literal(text) {
        let full = normalize_monkey_list(&v);
        if !full.is_empty() {
            return (full, warnings);
        }
    }
    warnings
        .push("direct literal parse failed or empty, attempting tolerant extraction".to_string());

    let mut best: Vec<MonkeyCell> = Vec::new();
    for blk in extract_tolerant_list_blocks(text) {
        if let Ok(v) = parse_py_literal(&blk) {
            let cur = normalize_monkey_list(&v);
            if cur.len() > best.len() {
                best = cur;
            }
        }
    }

    let mut dict_items = Vec::new();
    for blk in extract_tolerant_dict_blocks(text) {
        if let Ok(v) = parse_py_literal(&blk)
            && let Some(item) = normalize_monkey_item(&v)
        {
            dict_items.push(item);
        }
    }
    if dict_items.len() > best.len() {
        best = dict_items;
    }

    if best.is_empty() {
        warnings.push("all tolerant recovery strategies failed".to_string());
    } else {
        warnings.push(format!(
            "recovered {} cell(s) via tolerant extraction",
            best.len()
        ));
    }
    (best, warnings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_normal_line() {
        let raw = "<|box_start|>100 200 300 400<|box_end|><|ref_start|>text<|ref_end|>";
        let (boxes, warnings) = parse_custom_tokens(raw);
        assert!(warnings.is_empty());
        assert_eq!(
            boxes,
            vec![LayoutBox {
                bbox_1000: [100, 200, 300, 400],
                category_raw: "text".into(),
                angle: None,
            }]
        );
    }

    #[test]
    fn official_parser_scans_adjacent_boxes_across_newlines() {
        let raw = "prefix<|box_start|>1 2 30 40<|box_end|><|ref_start|>TEXT<|ref_end|><|rotate_right|>tail\ntext\n<|box_start|>50 60 70 80<|box_end|><|ref_start|>table<|ref_end|>";
        let (boxes, warnings) = parse_custom_tokens_official(raw);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(boxes.len(), 2);
        assert_eq!(boxes[0].category_raw, "text");
        assert_eq!(boxes[0].angle, Some(90));
        assert_eq!(boxes[1].bbox_1000, [50, 60, 70, 80]);
    }

    #[test]
    fn official_parser_rejects_out_of_range_and_missing_closing_tokens() {
        let raw = "<|box_start|>0 0 1001 20<|box_end|><|ref_start|>text<|ref_end|>\n<|box_start|>1 2 3 4<|box_end|><|ref_start|>title";
        let (boxes, warnings) = parse_custom_tokens_official(raw);
        assert!(boxes.is_empty());
        assert!(warnings.iter().any(|w| w.contains("out-of-range")));
    }

    #[test]
    fn parses_rotated_line() {
        let raw = "<|box_start|>0 0 10 10<|box_end|><|ref_start|>title<|ref_end|><|rotate_right|>";
        let (boxes, warnings) = parse_custom_tokens(raw);
        assert!(warnings.is_empty());
        assert_eq!(boxes[0].angle, Some(90));
    }

    #[test]
    fn uppercase_category_is_lowercased() {
        let raw = "<|box_start|>0 0 10 10<|box_end|><|ref_start|>TEXT<|ref_end|>";
        let (boxes, _) = parse_custom_tokens(raw);
        assert_eq!(boxes[0].category_raw, "text");
    }

    #[test]
    fn out_of_range_coordinate_is_clamped_with_warning_not_dropped() {
        // A coordinate a little past 1000 (VLM quantization error at a
        // page edge) is recoverable — clamp it rather than discarding
        // the whole box (D.2).
        let raw = "<|box_start|>0 0 1500 10<|box_end|><|ref_start|>text<|ref_end|>";
        let (boxes, warnings) = parse_custom_tokens(raw);
        assert_eq!(boxes.len(), 1);
        assert_eq!(boxes[0].bbox_1000, [0, 0, 1000, 10]);
        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn well_formed_coordinates_produce_no_clamp_warning() {
        let raw = "<|box_start|>100 200 300 400<|box_end|><|ref_start|>text<|ref_end|>";
        let (boxes, warnings) = parse_custom_tokens(raw);
        assert_eq!(boxes.len(), 1);
        assert!(warnings.is_empty());
    }

    #[test]
    fn degenerate_box_is_skipped() {
        let raw = "<|box_start|>10 10 10 20<|box_end|><|ref_start|>text<|ref_end|>";
        let (boxes, warnings) = parse_custom_tokens(raw);
        assert!(boxes.is_empty());
        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn unparseable_line_is_skipped_not_fatal() {
        let raw = "this is not a layout line at all";
        let (boxes, warnings) = parse_custom_tokens(raw);
        assert!(boxes.is_empty());
        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn line_missing_ref_end_is_rescued_via_relaxed_matching() {
        // Truncated exactly at the closing tag — everything needed to
        // build a valid box is present, but the strict regex's `$`
        // anchor after `<|ref_end|>` fails to match at all (D.11).
        let raw = "<|box_start|>100 200 300 400<|box_end|><|ref_start|>text";
        let (boxes, warnings) = parse_custom_tokens(raw);
        assert_eq!(boxes.len(), 1);
        assert_eq!(boxes[0].category_raw, "text");
        assert!(
            warnings.iter().any(|w| w.contains("rescued")),
            "{warnings:?}"
        );
    }

    #[test]
    fn line_with_leading_garbage_before_tags_is_rescued() {
        // Some prefix junk before the actual box tag sequence — the
        // strict regex's `^` anchor rejects this outright even though
        // the box itself is well-formed.
        let raw = "garbage<|box_start|>0 0 10 10<|box_end|><|ref_start|>title<|ref_end|>";
        let (boxes, warnings) = parse_custom_tokens(raw);
        assert_eq!(boxes.len(), 1);
        assert_eq!(boxes[0].category_raw, "title");
        assert!(
            warnings.iter().any(|w| w.contains("rescued")),
            "{warnings:?}"
        );
    }

    #[test]
    fn well_formed_line_is_not_flagged_as_rescued() {
        let raw = "<|box_start|>100 200 300 400<|box_end|><|ref_start|>text<|ref_end|>";
        let (boxes, warnings) = parse_custom_tokens(raw);
        assert_eq!(boxes.len(), 1);
        assert!(warnings.is_empty());
    }

    #[test]
    fn empty_input_yields_nothing() {
        let (boxes, warnings) = parse_custom_tokens("");
        assert!(boxes.is_empty());
        assert!(warnings.is_empty());
    }

    #[test]
    fn multiple_lines_mixed_valid_and_invalid() {
        let raw = "<|box_start|>0 0 10 10<|box_end|><|ref_start|>text<|ref_end|>\ngarbage\n<|box_start|>20 20 30 30<|box_end|><|ref_start|>table<|ref_end|>";
        let (boxes, warnings) = parse_custom_tokens(raw);
        assert_eq!(boxes.len(), 2);
        assert_eq!(warnings.len(), 1);
    }

    proptest::proptest! {
        #[test]
        fn arbitrary_input_never_panics(s in ".*") {
            let _ = parse_custom_tokens(&s);
        }
    }

    #[test]
    fn strict_json_parses_well_formed_array() {
        let raw = r#"[{"bbox":[1,2,3,4],"category":"Text","text":"hello"}]"#;
        let (cells, warnings) = parse_strict_json(raw);
        assert!(warnings.is_empty());
        assert_eq!(
            cells,
            vec![DotsCell {
                bbox: [1.0, 2.0, 3.0, 4.0],
                category_raw: "Text".into(),
                text: Some("hello".into()),
            }]
        );
    }

    #[test]
    fn strict_json_repairs_missing_delimiter() {
        let raw = r#"[{"bbox":[1,2,3,4],"category":"Text","text":"a"}{"bbox":[5,6,7,8],"category":"Title","text":"b"}]"#;
        let (cells, _) = parse_strict_json(raw);
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[1].category_raw, "Title");
    }

    #[test]
    fn strict_json_drops_incomplete_trailing_element_when_multiple_present() {
        let raw = r#"[{"bbox":[1,2,3,4],"category":"Text","text":"a"},{"bbox":[5,6,7,8],"category":"Title","text":"unfinis"#;
        let (cells, warnings) = parse_strict_json(raw);
        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0].category_raw, "Text");
        assert!(warnings.iter().any(|w| w.contains("truncated")));
    }

    #[test]
    fn strict_json_does_not_truncate_the_only_element() {
        // Long/unterminated but only one dict present — truncation must
        // be skipped, or the sole element would be lost.
        let raw = r#"[{"bbox":[1,2,3,4],"category":"Text","text":"unfinis"#;
        let (cells, _) = parse_strict_json(raw);
        assert_eq!(cells.len(), 1);
    }

    #[test]
    fn strict_json_dedupes_exact_duplicate_dicts_preserving_order() {
        // A well-formed duplicate array parses fine on the first try (no
        // repair needed) — dedup only runs as part of the repair chain,
        // matching `output_cleaner.py`'s behavior (it's only invoked
        // after a direct `json.loads` failure). So pair the duplicate
        // with a missing delimiter to force entry into the repair path.
        let raw = r#"[{"bbox":[1,2,3,4],"category":"Text","text":"a"}{"bbox":[1,2,3,4],"category":"Text","text":"a"},{"bbox":[5,6,7,8],"category":"Title","text":"b"}]"#;
        let (cells, warnings) = parse_strict_json(raw);
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[0].category_raw, "Text");
        assert_eq!(cells[1].category_raw, "Title");
        assert!(warnings.iter().any(|w| w.contains("duplicate")));
    }

    #[test]
    fn strict_json_rescues_single_malformed_dict() {
        let raw = r#"[{"bbox": [10, 20, 30, 40], "category": "Text", "text": "partial cont"#;
        let (cells, warnings) = parse_strict_json(raw);
        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0].bbox, [10.0, 20.0, 30.0, 40.0]);
        assert_eq!(cells[0].category_raw, "Text");
        assert!(warnings.iter().any(|w| w.contains("rescued")));
    }

    #[test]
    fn strict_json_cell_missing_bbox_is_dropped_with_warning() {
        let raw = r#"[{"category":"Text","text":"no bbox here"}]"#;
        let (cells, warnings) = parse_strict_json(raw);
        assert!(cells.is_empty());
        assert!(warnings.iter().any(|w| w.contains("missing bbox")));
    }

    #[test]
    fn strict_json_completely_unparseable_yields_empty_with_warning() {
        let (cells, warnings) = parse_strict_json("not json at all, no braces");
        assert!(cells.is_empty());
        assert!(!warnings.is_empty());
    }

    proptest::proptest! {
        #[test]
        fn strict_json_arbitrary_input_never_panics(s in ".*") {
            let _ = parse_strict_json(&s);
        }
    }

    #[test]
    fn py_literal_parses_double_quoted_list_of_dicts() {
        let v = parse_py_literal(r#"[{"bbox": [1, 2, 3, 4], "label": "Text", "content": "hi"}]"#)
            .unwrap();
        let cells = normalize_monkey_list(&v);
        assert_eq!(
            cells,
            vec![MonkeyCell {
                bbox: [1.0, 2.0, 3.0, 4.0],
                label: "Text".into(),
                content: Some("hi".into()),
            }]
        );
    }

    #[test]
    fn py_literal_parses_single_quoted_strings() {
        let v = parse_py_literal(
            r#"[{'bbox': [1, 2, 3, 4], 'label': 'Table', 'content': 'it\'s ok'}]"#,
        )
        .unwrap();
        let cells = normalize_monkey_list(&v);
        assert_eq!(cells[0].label, "Table");
        assert_eq!(cells[0].content.as_deref(), Some("it's ok"));
    }

    #[test]
    fn py_literal_decodes_unicode_escape_sequences() {
        // 中文 is "中文" ("Chinese text"); previously the parser
        // dropped the backslash for unrecognized escapes, which for `\u`
        // corrupted this into the literal characters "u4e2du6587".
        let v = parse_py_literal(r#"'中文'"#).unwrap();
        assert_eq!(v, Value::String("中文".to_string()));
    }

    #[test]
    fn py_literal_decodes_hex_escape_sequences() {
        let v = parse_py_literal(r#"'\x41\x42'"#).unwrap();
        assert_eq!(v, Value::String("AB".to_string()));
    }

    #[test]
    fn py_literal_preserves_backslash_for_unknown_escapes() {
        let v = parse_py_literal(r#"'\p{L}'"#).unwrap();
        assert_eq!(v, Value::String("\\p{L}".to_string()));
    }

    #[test]
    fn py_literal_falls_back_to_literal_marker_on_truncated_unicode_escape() {
        // Only 2 hex digits follow \u instead of the required 4 — should
        // emit the literal `\u` marker rather than misdecoding or
        // consuming characters that aren't actually part of the escape.
        let v = parse_py_literal(r#"'\u4eX'"#).unwrap();
        assert_eq!(v, Value::String("\\u4eX".to_string()));
    }

    #[test]
    fn py_literal_parses_true_false_none() {
        let v = parse_py_literal("[True, False, None]").unwrap();
        assert_eq!(
            v,
            Value::Array(vec![Value::Bool(true), Value::Bool(false), Value::Null])
        );
    }

    #[test]
    fn py_literal_rejects_trailing_garbage() {
        assert!(parse_py_literal("[1, 2] garbage").is_err());
    }

    #[test]
    fn python_literal_list_parses_well_formed_input() {
        let raw = r#"[{"bbox": [0, 0, 100, 50], "label": "Text", "content": "hello"}, {"bbox": [0, 60, 100, 100], "label": "Picture"}]"#;
        let (cells, warnings) = parse_python_literal_list(raw);
        assert!(warnings.is_empty());
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[1].label, "Picture");
        assert_eq!(cells[1].content, None);
    }

    #[test]
    fn python_literal_list_recovers_from_unclosed_trailing_bracket() {
        // A well-formed first cell, then an unclosed second dict/list —
        // the tolerant list-block extraction should auto-close and
        // recover at least the first cell.
        let raw = r#"[{"bbox": [0, 0, 100, 50], "label": "Text", "content": "hello"}, {"bbox": [0, 60, 100, 100"#;
        let (cells, warnings) = parse_python_literal_list(raw);
        assert!(!cells.is_empty());
        assert_eq!(cells[0].label, "Text");
        assert!(!warnings.is_empty());
    }

    #[test]
    fn python_literal_list_picks_higher_count_candidate() {
        // The outer list is truncated/unparseable as a whole, but a
        // per-dict fallback scan should still recover both cells.
        let raw = r#"[{"bbox": [0, 0, 10, 10], "label": "Text", "content": "a"}, {"bbox": [0, 20, 10, 30], "label": "Title", "content": "b"} extra garbage not valid"#;
        let (cells, _) = parse_python_literal_list(raw);
        assert_eq!(cells.len(), 2);
    }

    #[test]
    fn python_literal_list_empty_input_yields_nothing() {
        let (cells, warnings) = parse_python_literal_list("");
        assert!(cells.is_empty());
        assert!(warnings.is_empty());
    }

    #[test]
    fn python_literal_list_completely_unparseable_yields_empty_with_warning() {
        let (cells, warnings) = parse_python_literal_list("not a literal at all");
        assert!(cells.is_empty());
        assert!(!warnings.is_empty());
    }

    proptest::proptest! {
        #[test]
        fn py_literal_arbitrary_input_never_panics(s in ".*") {
            let _ = parse_py_literal(&s);
        }

        #[test]
        fn python_literal_list_arbitrary_input_never_panics(s in ".*") {
            let _ = parse_python_literal_list(&s);
        }
    }
}
