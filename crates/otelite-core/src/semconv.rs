//! GenAI semantic-convention attribute vocabulary.
//!
//! Instrumentations from different LLM frameworks use different attribute names
//! for the same concept. Each constant below is the authoritative priority-ordered
//! list of aliases for one concept. Analytics code projects these lists into SQL
//! COALESCE fragments via [`coalesce_extract`] / [`coalesce_extract_cast`].
//!
//! Adding a new framework's aliases means appending to the relevant list here —
//! no SQL needs to change.
//!
//! Priority order within each list: OTel GenAI standard → OpenLLMetry/OpenInference
//! (`llm.*`) → raw vendor names → Claude Code flat names.

/// Prompt / input tokens.
pub const INPUT_TOKEN_KEYS: &[&str] = &[
    "gen_ai.usage.input_tokens",
    "gen_ai.usage.prompt_tokens",
    "llm.usage.prompt_tokens",
    "llm.token_count.prompt",
    "prompt_tokens",
    "input_tokens",
];

/// Completion / output tokens.
pub const OUTPUT_TOKEN_KEYS: &[&str] = &[
    "gen_ai.usage.output_tokens",
    "gen_ai.usage.completion_tokens",
    "llm.usage.completion_tokens",
    "llm.token_count.completion",
    "completion_tokens",
    "output_tokens",
];

/// Tokens written into the prompt cache on this request.
pub const CACHE_CREATION_TOKEN_KEYS: &[&str] = &[
    "gen_ai.usage.cache_creation.input_tokens",
    "gen_ai.usage.cache_creation_input_tokens",
    "gen_ai.usage.cache_creation_tokens",
    "cache_creation_input_tokens",
    "cache_creation_tokens",
];

/// Tokens read from the prompt cache on this request.
pub const CACHE_READ_TOKEN_KEYS: &[&str] = &[
    "gen_ai.usage.cache_read.input_tokens",
    "gen_ai.usage.cache_read_input_tokens",
    "gen_ai.usage.cache_read_tokens",
    "cache_read_input_tokens",
    "cache_read_tokens",
];

/// Model identifier.
pub const MODEL_KEYS: &[&str] = &[
    "gen_ai.request.model",
    "gen_ai.response.model",
    "llm.request.model",
    "llm.model_name",
    "model",
];

/// Request model identifier.
///
/// Capability reports must not substitute a response model, because providers
/// can route a request to a different serving model.
pub const REQUEST_MODEL_KEYS: &[&str] = &[
    "gen_ai.request.model",
    "llm.request.model",
    "llm.model_name",
    "model",
];

/// Provider / system (openai, anthropic, bedrock, ...).
pub const SYSTEM_KEYS: &[&str] = &[
    "gen_ai.provider.name",
    "gen_ai.system",
    "llm.system",
    "llm.vendor",
];

/// Attributes whose presence identifies a span as a GenAI / LLM call.
/// Used as a WHERE-clause guard for analytics that only apply to LLM spans.
pub const LLM_SPAN_MARKER_KEYS: &[&str] = &[
    "gen_ai.system",
    "gen_ai.provider.name",
    "llm.system",
    "llm.vendor",
    "llm.request.model",
];

/// OpenInference span-kind values that count as LLM activity for analytics.
pub const OPENINFERENCE_LLM_KINDS: &[&str] = &["LLM", "EMBEDDING"];

/// Span name prefixes for instrumentations that don't use the standard GenAI
/// attribute markers. Each entry is used as a LIKE pattern (`<prefix>%`).
///
/// Claude Code emits `claude_code.llm_request` spans with flat `model`,
/// `input_tokens`, `output_tokens` attributes but no `gen_ai.*` / `llm.*`
/// marker attributes, so the normal guard misses them entirely.
pub const VENDOR_SPAN_NAME_PREFIXES: &[&str] = &["claude_code.llm_request"];

/// Codex CLI's span for one completed model sampling request.
///
/// Codex adds its flat `model` attribute to several internal spans in a turn.
/// Only this span represents one model call; its nested
/// `model_client.stream_responses_api` span is transport timing, not another
/// request. Codex currently does not emit token usage or request-outcome
/// attributes, so it is only included in request-count and latency analytics.
pub const CODEX_LLM_REQUEST_SPAN_NAME: &str = "run_sampling_request";
pub const CODEX_OTEL_SCOPE_NAME: &str = "codex_cli_rs";

/// Build a `COALESCE(json_extract(col, '$."k1"'), ...)` expression over `keys`.
pub fn coalesce_extract(attributes_col: &str, keys: &[&str]) -> String {
    coalesce_inner(attributes_col, keys, None)
}

/// Build a `COALESCE(CAST(json_extract(col, '$."k1"') AS <cast>), ...)` expression.
pub fn coalesce_extract_cast(attributes_col: &str, keys: &[&str], cast: &str) -> String {
    coalesce_inner(attributes_col, keys, Some(cast))
}

fn coalesce_inner(attributes_col: &str, keys: &[&str], cast: Option<&str>) -> String {
    assert!(!keys.is_empty(), "coalesce over empty key list");
    let parts: Vec<String> = keys
        .iter()
        .map(|k| match cast {
            Some(c) => format!(
                "CAST(json_extract({col}, '$.\"{k}\"') AS {c})",
                col = attributes_col,
                k = k,
                c = c
            ),
            None => format!(
                "json_extract({col}, '$.\"{k}\"')",
                col = attributes_col,
                k = k
            ),
        })
        .collect();
    format!("COALESCE({})", parts.join(", "))
}

/// Parenthesised OR-chain that matches spans with standard LLM telemetry.
///
/// Includes the OpenInference `openinference.span.kind` IN (...) clause and
/// vendor-specific span-name prefix patterns. This deliberately excludes Codex
/// sampling spans because they lack token and request-outcome attributes.
///
/// Every `json_extract` clause is gated by `json_valid` so the guard is
/// *total*: it returns false (instead of raising "malformed JSON") for rows
/// whose `attributes` text is corrupt or NULL. This matters for two reasons:
/// - the guard is used as a partial-index predicate, which SQLite evaluates
///   on every INSERT; a raising predicate would reject valid telemetry
///   batches that contain a single corrupt span;
/// - without it, one corrupt row inside a time window would make every GenAI
///   analytics query over that window fail.
pub fn llm_span_guard(attributes_col: &str) -> String {
    let mut clauses: Vec<String> = LLM_SPAN_MARKER_KEYS
        .iter()
        .map(|k| {
            format!(
                "(json_valid({col}) AND json_extract({col}, '$.\"{k}\"') IS NOT NULL)",
                col = attributes_col,
                k = k
            )
        })
        .collect();
    let kinds = OPENINFERENCE_LLM_KINDS
        .iter()
        .map(|k| format!("'{}'", k))
        .collect::<Vec<_>>()
        .join(", ");
    clauses.push(format!(
        "(json_valid({col}) AND json_extract({col}, '$.\"openinference.span.kind\"') IN ({kinds}))",
        col = attributes_col,
        kinds = kinds
    ));
    for prefix in VENDOR_SPAN_NAME_PREFIXES {
        clauses.push(format!("name LIKE '{}%'", prefix));
    }
    format!("({})", clauses.join(" OR "))
}

/// LLM guard for analytics which only require a completed request and duration.
///
/// Extends [`llm_span_guard`] with Codex's native completed-sampling span while
/// excluding nested transport spans.
pub fn request_span_guard(attributes_col: &str) -> String {
    let llm_guard = llm_span_guard(attributes_col);
    let codex_guard = format!(
        "(name = '{span_name}' \
          AND json_valid({col}) \
          AND json_extract({col}, '$.\"model\"') IS NOT NULL \
          AND json_extract({col}, '$.\"otel.scope.name\"') = '{scope_name}')",
        span_name = CODEX_LLM_REQUEST_SPAN_NAME,
        col = attributes_col,
        scope_name = CODEX_OTEL_SCOPE_NAME,
    );
    format!("({llm_guard} OR {codex_guard})")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coalesce_extract_formats_keys() {
        let sql = coalesce_extract("attributes", &["a.b", "c"]);
        assert_eq!(
            sql,
            "COALESCE(json_extract(attributes, '$.\"a.b\"'), json_extract(attributes, '$.\"c\"'))"
        );
    }

    #[test]
    fn coalesce_extract_cast_wraps_each_clause() {
        let sql = coalesce_extract_cast("attributes", INPUT_TOKEN_KEYS, "INTEGER");
        assert!(sql.starts_with("COALESCE(CAST(json_extract(attributes, "));
        assert!(sql.contains("gen_ai.usage.input_tokens"));
        assert!(sql.contains("input_tokens"));
        assert!(sql.contains("AS INTEGER"));
    }

    #[test]
    fn llm_span_guard_includes_openinference_kinds() {
        let sql = llm_span_guard("attributes");
        assert!(sql.contains("gen_ai.system"));
        assert!(sql.contains("llm.request.model"));
        assert!(sql.contains("openinference.span.kind"));
        assert!(sql.contains("'LLM'"));
        assert!(sql.contains("'EMBEDDING'"));
        assert!(sql.starts_with('(') && sql.ends_with(')'));
    }

    #[test]
    fn llm_span_guard_includes_vendor_span_name_prefixes() {
        let sql = llm_span_guard("attributes");
        // Claude Code spans must be matched even without gen_ai.* attributes.
        assert!(
            sql.contains("name LIKE 'claude_code.llm_request%'"),
            "expected claude_code.llm_request LIKE clause in: {sql}"
        );
    }

    #[test]
    fn request_span_guard_includes_strict_codex_request_signature() {
        let sql = request_span_guard("attributes");
        assert!(
            sql.contains("name = 'run_sampling_request'"),
            "expected Codex request span clause in: {sql}"
        );
        assert!(
            sql.contains("json_extract(attributes, '$.\"model\"') IS NOT NULL"),
            "expected Codex model requirement in: {sql}"
        );
        assert!(
            sql.contains("json_extract(attributes, '$.\"otel.scope.name\"') = 'codex_cli_rs'"),
            "expected Codex scope requirement in: {sql}"
        );
        assert!(
            !llm_span_guard("attributes").contains("run_sampling_request"),
            "standard LLM guard must exclude Codex spans without token or outcome attributes"
        );
    }
}
