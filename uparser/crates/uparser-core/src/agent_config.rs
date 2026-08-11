//! Endpoint/model resolution for the CLI, so an Agent (or a human moving
//! between machines) doesn't have to repeat `--endpoint`/`--model` on every
//! `parse`/`doctor` call. Precedence, highest first:
//!
//!   1. the explicit CLI flag (`--endpoint` / `--model`)
//!   2. the `UPARSER_ENDPOINT` / `UPARSER_MODEL` environment variables
//!   3. `~/.config/uparser/config.toml` (or `$UPARSER_CONFIG`), the
//!      `[<protocol>]` section's `endpoint` / `model` key
//!
//! Only a value the caller *omitted* is ever filled in — an explicit flag
//! always wins. Config lookup is keyed by the **effective** protocol (i.e.
//! after `--protocol auto` has been resolved to a concrete adapter), so a
//! routed `mineru-vlm` picks up the `[mineru-vlm]` section.
//!
//! The config reader is a deliberately minimal `[section]` + `key = value`
//! parser (quotes stripped), matching `skills/uparser/references/
//! config.example.toml`'s simple shape and the shell wrapper's `awk` reader —
//! not a full TOML parser, to avoid pulling in a new dependency (the same
//! no-new-dep posture the rest of this crate takes).

use std::path::PathBuf;

/// Resolve `(endpoint, model)` for `protocol`, given whatever the CLI already
/// provided, applying the env → config fallback chain documented above.
pub fn resolve_endpoint_model(
    protocol: &str,
    cli_endpoint: Option<String>,
    cli_model: Option<String>,
) -> (Option<String>, Option<String>) {
    let endpoint = cli_endpoint
        .or_else(|| env_nonempty("UPARSER_ENDPOINT"))
        .or_else(|| config_value(protocol, "endpoint"));
    let model = cli_model
        .or_else(|| env_nonempty("UPARSER_MODEL"))
        .or_else(|| config_value(protocol, "model"));
    (endpoint, model)
}

fn env_nonempty(key: &str) -> Option<String> {
    match std::env::var(key) {
        Ok(v) if !v.trim().is_empty() => Some(v),
        _ => None,
    }
}

/// `$UPARSER_CONFIG` if set, else `~/.config/uparser/config.toml`.
fn config_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("UPARSER_CONFIG") {
        return Some(PathBuf::from(p));
    }
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".config/uparser/config.toml"))
}

/// Read one `key` from the `[section]` block of the resolved config file.
/// Returns `None` if the file is absent/unreadable or the key isn't present —
/// a missing config is never an error, just an empty fallback.
pub fn config_value(section: &str, key: &str) -> Option<String> {
    let text = std::fs::read_to_string(config_path()?).ok()?;
    read_ini_value(&text, section, key)
}

/// Pure string parse, factored out so it's unit-testable without touching the
/// filesystem or environment.
fn read_ini_value(text: &str, section: &str, key: &str) -> Option<String> {
    let mut cur = "";
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            cur = name.trim();
            continue;
        }
        if cur == section
            && let Some((k, v)) = line.split_once('=')
            && k.trim() == key
        {
            return Some(strip_quotes(v.trim()).to_string());
        }
    }
    None
}

/// Strip a single matching pair of surrounding single or double quotes.
fn strip_quotes(s: &str) -> &str {
    let b = s.as_bytes();
    if b.len() >= 2 && (b[0] == b'"' || b[0] == b'\'') && b[b.len() - 1] == b[0] {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CFG: &str = r#"
# a comment
[mineru-vlm]
endpoint = "http://10.0.0.5:19122/v1/chat/completions"
model    = MinerU2.5-2604-1.2B

[dots-ocr]
endpoint = 'http://127.0.0.1:8000/v1/chat/completions'
"#;

    #[test]
    fn reads_double_quoted_value_from_the_right_section() {
        assert_eq!(
            read_ini_value(CFG, "mineru-vlm", "endpoint").as_deref(),
            Some("http://10.0.0.5:19122/v1/chat/completions")
        );
    }

    #[test]
    fn reads_unquoted_value() {
        assert_eq!(
            read_ini_value(CFG, "mineru-vlm", "model").as_deref(),
            Some("MinerU2.5-2604-1.2B")
        );
    }

    #[test]
    fn reads_single_quoted_value() {
        assert_eq!(
            read_ini_value(CFG, "dots-ocr", "endpoint").as_deref(),
            Some("http://127.0.0.1:8000/v1/chat/completions")
        );
    }

    #[test]
    fn missing_section_or_key_is_none() {
        assert_eq!(read_ini_value(CFG, "mineru-vlm", "nope"), None);
        assert_eq!(read_ini_value(CFG, "no-such-section", "endpoint"), None);
        assert_eq!(read_ini_value(CFG, "dots-ocr", "model"), None);
    }

    #[test]
    fn key_does_not_leak_across_sections() {
        // `model` exists only under [mineru-vlm], not [dots-ocr]
        assert_eq!(read_ini_value(CFG, "dots-ocr", "model"), None);
    }
}
