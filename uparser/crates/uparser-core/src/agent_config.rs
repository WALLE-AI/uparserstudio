//! Endpoint/model/auth resolution, so an Agent (or a human moving between
//! machines) doesn't have to repeat `--endpoint`/`--model` on every
//! `parse`/`doctor` call, and so a token-protected endpoint is usable at
//! all. Every field is resolved independently, highest precedence first:
//!
//!   1. the explicit CLI flag (`--endpoint` / `--model`)
//!   2. the `UPARSER_ENDPOINT` / `UPARSER_MODEL` / `UPARSER_API_KEY`
//!      environment variables
//!   3. `~/.config/uparser/config.toml` (or `$UPARSER_CONFIG`), the
//!      `[<protocol>]` section
//!   4. that same file's `[defaults]` section
//!   5. `None` — meaning "fall through to the protocol's built-in default",
//!      which lives in `protocol_spec.rs` and nowhere else.
//!
//! Only a value the caller *omitted* is ever filled in — an explicit flag
//! always wins. Config lookup is keyed by the **effective** protocol (i.e.
//! after `--protocol auto` has been resolved to a concrete adapter), so a
//! routed `mineru-vlm` picks up the `[mineru-vlm]` section.
//!
//! # Why real TOML now
//!
//! This module used to hand-roll a flat `[section]` + `key = value` reader
//! to avoid a dependency. That cannot express nested tables, which the
//! `pipeline` protocol's nine per-stage endpoints need
//! (`[pipeline.stages]`), so those were reachable only via CLI flags and
//! could not be configured at all. The `toml` crate replaces it.
//!
//! # Backward compatibility
//!
//! The old reader accepted unquoted values (`model = MinerU2.5-2604-1.2B`),
//! which is **not** valid TOML. Switching to a strict parser would have
//! silently dropped such a user's entire config. So a TOML parse failure
//! falls back to the original INI reader — retained verbatim below,
//! together with its tests — and warns on stderr. A config that parses as
//! TOML never touches the fallback path.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use crate::adapters::{MonkeyOcrConfig, NavidcConfig, NavidcLayoutMode, PipelineConfig};

/// The `[defaults]` section name. Not a protocol — `protocol_spec.rs` has
/// no such entry — so it can never collide with a real `[<protocol>]`.
const DEFAULTS_SECTION: &str = "defaults";

/// Whatever the caller already supplied on the command line. Anything
/// `None` here is open to being filled in from env/config.
#[derive(Debug, Clone, Default)]
pub struct CliOverrides {
    pub endpoint: Option<String>,
    pub model: Option<String>,
}

/// The fully resolved per-protocol configuration. A `None`/empty field
/// means "not configured anywhere" — the adapter's own `Default` (sourced
/// from `protocol_spec.rs`) then applies.
#[derive(Debug, Clone, Default)]
pub struct ResolvedConfig {
    pub endpoint: Option<String>,
    pub model: Option<String>,
    /// Never logged, never printed, never part of a cache key.
    pub api_key: Option<String>,
    pub headers: Vec<(String, String)>,
    pub timeout: Option<Duration>,
    pub max_retries: Option<u32>,
    pub pipeline: PipelineConfig,
    pub monkeyocr: MonkeyOcrConfig,
    pub navidc: NavidcConfig,
}

/// Resolve everything configurable for `protocol`.
pub fn resolve(protocol: &str, cli: CliOverrides) -> ResolvedConfig {
    resolve_with(protocol, cli, &ConfigFile::load())
}

fn resolve_with(protocol: &str, cli: CliOverrides, config: &ConfigFile) -> ResolvedConfig {
    let string_of = |key: &str| config.layered_string(protocol, key);

    ResolvedConfig {
        endpoint: cli
            .endpoint
            .or_else(|| env_nonempty("UPARSER_ENDPOINT"))
            .or_else(|| string_of("endpoint")),
        model: cli
            .model
            .or_else(|| env_nonempty("UPARSER_MODEL"))
            .or_else(|| string_of("model")),
        api_key: resolve_api_key(protocol, config),
        headers: config.layered_headers(protocol),
        timeout: config
            .layered_integer(protocol, "timeout_secs")
            .and_then(|secs| u64::try_from(secs).ok())
            .map(Duration::from_secs),
        max_retries: config
            .layered_integer(protocol, "max_retries")
            .and_then(|n| u32::try_from(n).ok()),
        pipeline: config.pipeline_config(),
        monkeyocr: MonkeyOcrConfig {
            retry_repeat: config.layered_bool(protocol, "retry_repeat"),
            retry_repeat_max_retries: config
                .layered_integer(protocol, "retry_repeat_max_retries")
                .and_then(|n| u32::try_from(n).ok()),
        },
        navidc: NavidcConfig {
            layout_mode: string_of("layout_mode").and_then(|raw| {
                match raw.trim().to_ascii_lowercase().as_str() {
                    "detection" => Some(NavidcLayoutMode::Detection),
                    "segmentation" => Some(NavidcLayoutMode::Segmentation),
                    other => {
                        eprintln!(
                            "warning: config [{protocol}] layout_mode = {other:?} is not one of \
                             'detection'/'segmentation'; ignoring"
                        );
                        None
                    }
                }
            }),
        },
    }
}

/// `api_key` has one extra hop the other fields don't: `api_key_env` names
/// an environment variable to read the secret *from*, so the key itself
/// need not be written to disk in plaintext. The indirect form is checked
/// before the literal one within each section.
fn resolve_api_key(protocol: &str, config: &ConfigFile) -> Option<String> {
    if let Some(key) = env_nonempty("UPARSER_API_KEY") {
        return Some(key);
    }
    for section in [protocol, DEFAULTS_SECTION] {
        if let Some(var) = config.string(section, "api_key_env")
            && let Some(key) = env_nonempty(&var)
        {
            return Some(key);
        }
        if let Some(key) = config.string(section, "api_key") {
            return Some(key);
        }
    }
    None
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

/// The parsed config file, or the evidence that there isn't a usable one.
#[derive(Debug, Default)]
enum ConfigFile {
    /// No file, or an unreadable one — never an error, just an empty
    /// fallback.
    #[default]
    Absent,
    Toml(toml::Table),
    /// A file that failed to parse as TOML. Held as raw text and read with
    /// the pre-TOML flat reader, so a config written against the old
    /// permissive syntax keeps working. Nested lookups yield `None` here.
    LegacyIni(String),
}

impl ConfigFile {
    fn load() -> Self {
        let Some(path) = config_path() else {
            return Self::Absent;
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Self::Absent;
        };
        Self::parse(&text, &path.display().to_string())
    }

    fn parse(text: &str, origin: &str) -> Self {
        match text.parse::<toml::Table>() {
            Ok(table) => Self::Toml(table),
            Err(error) => {
                eprintln!(
                    "warning: {origin} is not valid TOML ({error}); falling back to the legacy \
                     flat key=value reader. Quote string values to silence this."
                );
                Self::LegacyIni(text.to_owned())
            }
        }
    }

    /// One key from one explicit section, with no `[defaults]` fallback.
    fn string(&self, section: &str, key: &str) -> Option<String> {
        match self {
            Self::Absent => None,
            Self::Toml(table) => table
                .get(section)?
                .as_table()?
                .get(key)
                .and_then(scalar_to_string),
            Self::LegacyIni(text) => read_ini_value(text, section, key),
        }
    }

    /// `[<protocol>]` first, then `[defaults]`.
    fn layered_string(&self, protocol: &str, key: &str) -> Option<String> {
        self.string(protocol, key)
            .or_else(|| self.string(DEFAULTS_SECTION, key))
    }

    fn layered_integer(&self, protocol: &str, key: &str) -> Option<i64> {
        let raw = self.layered_string(protocol, key)?;
        match raw.trim().parse::<i64>() {
            Ok(value) if value >= 0 => Some(value),
            _ => {
                eprintln!(
                    "warning: config key {key:?} for protocol {protocol:?} is not a non-negative \
                     integer (got {raw:?}); ignoring"
                );
                None
            }
        }
    }

    fn layered_bool(&self, protocol: &str, key: &str) -> Option<bool> {
        let raw = self.layered_string(protocol, key)?;
        match raw.trim().to_ascii_lowercase().as_str() {
            "true" => Some(true),
            "false" => Some(false),
            other => {
                eprintln!(
                    "warning: config key {key:?} for protocol {protocol:?} is not a boolean (got \
                     {other:?}); ignoring"
                );
                None
            }
        }
    }

    /// A nested `[<section>.<sub>]` table as plain string pairs. Always
    /// empty in `LegacyIni` mode — nested tables are exactly what that
    /// syntax could not express.
    fn subtable(&self, section: &str, sub: &str) -> BTreeMap<String, String> {
        let Self::Toml(table) = self else {
            return BTreeMap::new();
        };
        let Some(nested) = table
            .get(section)
            .and_then(toml::Value::as_table)
            .and_then(|s| s.get(sub))
            .and_then(toml::Value::as_table)
        else {
            return BTreeMap::new();
        };
        nested
            .iter()
            .filter_map(|(k, v)| Some((k.clone(), scalar_to_string(v)?)))
            .collect()
    }

    /// `[defaults.headers]` merged under `[<protocol>.headers]`.
    fn layered_headers(&self, protocol: &str) -> Vec<(String, String)> {
        let mut merged = self.subtable(DEFAULTS_SECTION, "headers");
        merged.extend(self.subtable(protocol, "headers"));
        merged.into_iter().collect()
    }

    /// The `pipeline` protocol's per-stage endpoints and model-asset paths.
    /// Unlike every other protocol these need nested tables, which is the
    /// reason this module moved to real TOML.
    fn pipeline_config(&self) -> PipelineConfig {
        let stages = self.subtable("pipeline", "stages");
        let paths = self.subtable("pipeline", "paths");
        let stage = |key: &str| stages.get(key).cloned();

        PipelineConfig {
            // Stage *backends* are intentionally not configurable: Pipeline
            // V2 keeps every model in the service process and `cli.rs`
            // rejects a `local` request outright, so accepting one here
            // would only create a setting that silently does nothing.
            layout_backend: None,
            ocr_backend: None,
            formula_backend: None,
            table_backend: None,
            table_model_path: None,

            layout_endpoint: stage("layout"),
            bare_layout_endpoint: stage("bare_layout"),
            formula_detection_endpoint: stage("formula_detection"),
            ocr_endpoint: stage("ocr"),
            bare_ocr_endpoint_base: stage("bare_ocr_base"),
            formula_endpoint: stage("formula"),
            bare_formula_endpoint: stage("bare_formula"),
            table_endpoint: stage("table"),
            bare_table_endpoint_base: stage("bare_table_base"),

            ocr_dictionary_path: paths.get("ocr_dictionary").cloned(),
            formula_tokenizer_path: paths.get("formula_tokenizer").cloned(),

            language: self.string("pipeline", "language"),
        }
    }
}

/// Layer the config file's `[pipeline]` section under the per-stage CLI
/// flags. Done field by field rather than "whole struct if any flag is
/// set", so configuring eight stages in the file and overriding the ninth
/// on the command line works — which is the realistic shape, since the
/// nine stage endpoints are exactly the thing a config file exists to stop
/// you retyping.
pub fn merge_pipeline_config(flags: PipelineConfig, config: PipelineConfig) -> PipelineConfig {
    use PipelineConfig;
    PipelineConfig {
        // Stage backends are validated and hard-`None`d upstream (Pipeline
        // V2 keeps every model in the service process), and the config
        // layer never supplies them either. Carried through unchanged.
        layout_backend: flags.layout_backend,
        ocr_backend: flags.ocr_backend,
        formula_backend: flags.formula_backend,
        table_backend: flags.table_backend,
        table_model_path: flags.table_model_path,

        layout_endpoint: flags.layout_endpoint.or(config.layout_endpoint),
        bare_layout_endpoint: flags.bare_layout_endpoint.or(config.bare_layout_endpoint),
        formula_detection_endpoint: flags
            .formula_detection_endpoint
            .or(config.formula_detection_endpoint),
        ocr_endpoint: flags.ocr_endpoint.or(config.ocr_endpoint),
        bare_ocr_endpoint_base: flags
            .bare_ocr_endpoint_base
            .or(config.bare_ocr_endpoint_base),
        ocr_dictionary_path: flags.ocr_dictionary_path.or(config.ocr_dictionary_path),
        formula_endpoint: flags.formula_endpoint.or(config.formula_endpoint),
        bare_formula_endpoint: flags.bare_formula_endpoint.or(config.bare_formula_endpoint),
        formula_tokenizer_path: flags
            .formula_tokenizer_path
            .or(config.formula_tokenizer_path),
        table_endpoint: flags.table_endpoint.or(config.table_endpoint),
        bare_table_endpoint_base: flags
            .bare_table_endpoint_base
            .or(config.bare_table_endpoint_base),
        language: flags.language.or(config.language),
    }
}

/// Render a TOML scalar as the string the rest of this module works in.
/// Integers and booleans are accepted so `timeout_secs = 120` and
/// `retry_repeat = true` can be written naturally rather than quoted;
/// arrays and tables are rejected (they'd have no meaning for these keys).
fn scalar_to_string(value: &toml::Value) -> Option<String> {
    match value {
        toml::Value::String(s) => Some(s.clone()),
        toml::Value::Integer(i) => Some(i.to_string()),
        toml::Value::Boolean(b) => Some(b.to_string()),
        toml::Value::Float(f) => Some(f.to_string()),
        _ => None,
    }
}

// --- legacy flat reader (pre-TOML) ------------------------------------
//
// Retained as the fallback for a config file that doesn't parse as TOML —
// most plausibly one using unquoted string values, which the original
// reader accepted. Not used at all when the file is valid TOML.

/// Read one `key` from the `[section]` block of raw config text.
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

    // --- TOML layer ---------------------------------------------------

    const TOML_CFG: &str = r#"
[defaults]
endpoint     = "http://shared:8000/v1/chat/completions"
model        = "shared-model"
timeout_secs = 90
max_retries  = 5
api_key      = "default-key"

[defaults.headers]
X-Shared = "yes"

[mineru-vlm]
endpoint = "http://10.0.0.5:19122/v1/chat/completions"
model    = "MinerU2.5-Pro-2605-1.2B"

[mineru-vlm.headers]
X-Tenant = "acme"

[dots-ocr]
api_key_env = "TEST_DOTS_KEY"

[monkeyocr-v2]
retry_repeat             = true
retry_repeat_max_retries = 7

[navidc-ocr]
layout_mode = "segmentation"

[pipeline]
endpoint = "http://10.0.0.5:9001"
language = "en"

[pipeline.stages]
layout            = "http://gpu-a:9101/layout"
formula_detection = "http://gpu-a:9102/mfd"
ocr               = "http://gpu-b:9103/ocr"
formula           = "http://gpu-b:9104/mfr"
table             = "http://gpu-c:9105/table"
bare_layout       = "http://gpu-a:9201/bare-layout"
bare_ocr_base     = "http://gpu-b:9202"
bare_formula      = "http://gpu-b:9203/bare-mfr"
bare_table_base   = "http://gpu-c:9204"

[pipeline.paths]
ocr_dictionary    = "/models/ppocr_keys.txt"
formula_tokenizer = "/models/mfr-tokenizer.json"
"#;

    fn toml_cfg() -> ConfigFile {
        ConfigFile::parse(TOML_CFG, "<test>")
    }

    /// Env vars are process-global; these tests deliberately avoid setting
    /// any so they can run in parallel with the rest of the suite. The
    /// env layer itself is covered by the CLI integration tests, which get
    /// their own process.
    fn resolved(protocol: &str) -> ResolvedConfig {
        resolve_with(protocol, CliOverrides::default(), &toml_cfg())
    }

    #[test]
    fn protocol_section_overrides_defaults_section() {
        let cfg = resolved("mineru-vlm");
        assert_eq!(
            cfg.endpoint.as_deref(),
            Some("http://10.0.0.5:19122/v1/chat/completions")
        );
        assert_eq!(cfg.model.as_deref(), Some("MinerU2.5-Pro-2605-1.2B"));
    }

    #[test]
    fn defaults_section_applies_to_a_protocol_with_no_section_of_its_own() {
        let cfg = resolved("generic-vlm");
        assert_eq!(
            cfg.endpoint.as_deref(),
            Some("http://shared:8000/v1/chat/completions")
        );
        assert_eq!(cfg.model.as_deref(), Some("shared-model"));
    }

    #[test]
    fn defaults_section_supplies_keys_the_protocol_section_omits() {
        // [mineru-vlm] sets no timeout/max_retries, so [defaults] wins —
        // per-key layering, not whole-section replacement.
        let cfg = resolved("mineru-vlm");
        assert_eq!(cfg.timeout, Some(Duration::from_secs(90)));
        assert_eq!(cfg.max_retries, Some(5));
    }

    #[test]
    fn cli_override_beats_every_config_layer() {
        let cfg = resolve_with(
            "mineru-vlm",
            CliOverrides {
                endpoint: Some("http://explicit:1234/v1/chat/completions".into()),
                model: None,
            },
            &toml_cfg(),
        );
        assert_eq!(
            cfg.endpoint.as_deref(),
            Some("http://explicit:1234/v1/chat/completions")
        );
        // ...and does not disturb the fields it didn't set.
        assert_eq!(cfg.model.as_deref(), Some("MinerU2.5-Pro-2605-1.2B"));
    }

    #[test]
    fn nothing_configured_resolves_to_none_so_the_spec_default_applies() {
        let cfg = resolve_with("mineru-vlm", CliOverrides::default(), &ConfigFile::Absent);
        assert!(cfg.endpoint.is_none());
        assert!(cfg.model.is_none());
        assert!(cfg.api_key.is_none());
        assert!(cfg.timeout.is_none());
        assert!(cfg.headers.is_empty());
    }

    #[test]
    fn headers_merge_with_the_protocol_section_winning() {
        let cfg = resolved("mineru-vlm");
        let headers: BTreeMap<_, _> = cfg.headers.into_iter().collect();
        assert_eq!(headers.get("X-Shared").map(String::as_str), Some("yes"));
        assert_eq!(headers.get("X-Tenant").map(String::as_str), Some("acme"));
    }

    #[test]
    fn literal_api_key_is_read_from_the_defaults_section() {
        assert_eq!(
            resolved("mineru-vlm").api_key.as_deref(),
            Some("default-key")
        );
    }

    #[test]
    fn pipeline_stage_endpoints_come_from_the_nested_table() {
        // The whole reason this module moved to real TOML: these nine
        // values had no configuration path at all before.
        let p = resolved("pipeline").pipeline;
        assert_eq!(
            p.layout_endpoint.as_deref(),
            Some("http://gpu-a:9101/layout")
        );
        assert_eq!(
            p.formula_detection_endpoint.as_deref(),
            Some("http://gpu-a:9102/mfd")
        );
        assert_eq!(p.ocr_endpoint.as_deref(), Some("http://gpu-b:9103/ocr"));
        assert_eq!(p.formula_endpoint.as_deref(), Some("http://gpu-b:9104/mfr"));
        assert_eq!(p.table_endpoint.as_deref(), Some("http://gpu-c:9105/table"));
        assert_eq!(
            p.bare_layout_endpoint.as_deref(),
            Some("http://gpu-a:9201/bare-layout")
        );
        assert_eq!(
            p.bare_ocr_endpoint_base.as_deref(),
            Some("http://gpu-b:9202")
        );
        assert_eq!(
            p.bare_formula_endpoint.as_deref(),
            Some("http://gpu-b:9203/bare-mfr")
        );
        assert_eq!(
            p.bare_table_endpoint_base.as_deref(),
            Some("http://gpu-c:9204")
        );
        assert_eq!(
            p.ocr_dictionary_path.as_deref(),
            Some("/models/ppocr_keys.txt")
        );
        assert_eq!(
            p.formula_tokenizer_path.as_deref(),
            Some("/models/mfr-tokenizer.json")
        );
        assert_eq!(p.language.as_deref(), Some("en"));
    }

    #[test]
    fn monkeyocr_and_navidc_protocol_specific_keys_are_read() {
        let monkey = resolved("monkeyocr-v2").monkeyocr;
        assert_eq!(monkey.retry_repeat, Some(true));
        assert_eq!(monkey.retry_repeat_max_retries, Some(7));

        let navidc = resolved("navidc-ocr").navidc;
        assert_eq!(navidc.layout_mode, Some(NavidcLayoutMode::Segmentation));
    }

    #[test]
    fn unquoted_value_falls_back_to_the_legacy_reader_instead_of_dropping_the_file() {
        // `model = MinerU2.5-2604-1.2B` is not valid TOML. A strict parser
        // would have silently discarded this user's entire config; the
        // fallback keeps both keys readable.
        let cfg = ConfigFile::parse(CFG, "<test>");
        assert!(matches!(cfg, ConfigFile::LegacyIni(_)));
        let resolved = resolve_with("mineru-vlm", CliOverrides::default(), &cfg);
        assert_eq!(
            resolved.endpoint.as_deref(),
            Some("http://10.0.0.5:19122/v1/chat/completions")
        );
        assert_eq!(resolved.model.as_deref(), Some("MinerU2.5-2604-1.2B"));
    }

    #[test]
    fn legacy_fallback_yields_no_nested_values_rather_than_guessing() {
        let cfg = ConfigFile::parse(CFG, "<test>");
        let p = resolve_with("pipeline", CliOverrides::default(), &cfg).pipeline;
        assert!(p.layout_endpoint.is_none());
        assert!(
            resolve_with("mineru-vlm", CliOverrides::default(), &cfg)
                .headers
                .is_empty()
        );
    }

    #[test]
    fn a_malformed_scalar_is_warned_about_and_ignored_not_fatal() {
        let cfg = ConfigFile::parse(
            r#"
[mineru-vlm]
timeout_secs = "not-a-number"
max_retries  = -3
"#,
            "<test>",
        );
        let resolved = resolve_with("mineru-vlm", CliOverrides::default(), &cfg);
        assert!(resolved.timeout.is_none());
        assert!(resolved.max_retries.is_none());
    }

    #[test]
    fn integers_and_booleans_need_no_quoting() {
        let cfg = resolved("monkeyocr-v2");
        assert_eq!(cfg.timeout, Some(Duration::from_secs(90)));
        assert_eq!(cfg.monkeyocr.retry_repeat, Some(true));
    }
}
