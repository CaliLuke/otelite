//! Cross-cutting GenAI filter dimensions for the global filter bar (#135).
//!
//! The filter bar sends `agent`, `model`, `provider`, `project`, `session`
//! to every GenAI endpoint. Each endpoint applies the subset of dimensions
//! it genuinely supports and echoes the applied set back as
//! `filters_applied` so the UI can grey out what it could not apply.
//! Unsupported filter params are ignored — never a 400.
//!
//! Dimension support by store:
//! - spans: all five (agent via name/scope family, model/provider via
//!   semconv key coalesces, project via `project.id`, session via
//!   `session.id`)
//! - logs: session only (`session.id` / `conversation.id`)
//! - metrics (opencode-labelled): session, project, model — opencode emits
//!   no metrics under any other family, so agent has no metric predicate

use serde::{Deserialize, Serialize};

use crate::semconv::{
    agent_names as an, coalesce_extract, MODEL_KEYS, SESSION_ID_KEY, SYSTEM_KEYS,
};

/// The five filter dimension names, in filter-bar order.
pub const FILTER_DIMENSIONS: [&str; 5] = ["agent", "model", "provider", "project", "session"];

/// Cross-cutting GenAI filter dimensions.
///
/// Every dimension is optional; an absent dimension applies no predicate.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GenAiFilters {
    /// Agent family: `claude`, `opencode`, or `codex`.
    pub agent: Option<String>,
    /// Model name, e.g. `claude-sonnet-4-6` or `aws/claude-sonnet-5`.
    pub model: Option<String>,
    /// Provider, e.g. `anthropic`, `openai`, `amazon`.
    pub provider: Option<String>,
    /// Project id (opencode spans/metrics only).
    pub project: Option<String>,
    /// Session id.
    pub session: Option<String>,
}

impl GenAiFilters {
    /// Names of the supplied dimensions restricted to `supported`, in
    /// filter-bar order. Used to build the `filters_applied` echo.
    pub fn applied(&self, supported: &[&str]) -> Vec<String> {
        FILTER_DIMENSIONS
            .iter()
            .filter(|name| supported.contains(name))
            .filter_map(|name| match *name {
                // Unknown families are ignored, mirroring span_scope.
                "agent" => self
                    .agent
                    .as_ref()
                    .and_then(|a| agent_span_predicate(a).map(|_| name.to_string())),
                "model" => self.model.as_ref().map(|_| name.to_string()),
                "provider" => self.provider.as_ref().map(|_| name.to_string()),
                "project" => self.project.as_ref().map(|_| name.to_string()),
                "session" => self.session.as_ref().map(|_| name.to_string()),
                _ => None,
            })
            .collect()
    }

    /// SQL predicate fragment + bind params for the `spans` table.
    ///
    /// Returns `None` when no *supported* dimension is supplied. An
    /// unrecognised agent value (not one of the known families) is treated
    /// as unsupported and ignored, matching the never-400 contract.
    pub fn span_scope(&self) -> Option<(String, Vec<String>)> {
        let mut parts: Vec<String> = Vec::new();
        let mut params: Vec<String> = Vec::new();

        if let Some(agent) = self.agent.as_deref().and_then(agent_span_predicate) {
            parts.push(agent);
        }
        if let Some(m) = &self.model {
            parts.push(format!(
                "({}) = ?",
                coalesce_extract("attributes", MODEL_KEYS)
            ));
            params.push(m.clone());
        }
        if let Some(p) = &self.provider {
            parts.push(format!(
                "({}) = ?",
                coalesce_extract("attributes", SYSTEM_KEYS)
            ));
            params.push(p.clone());
        }
        if let Some(pr) = &self.project {
            parts.push("json_extract(attributes, '$.\"project.id\"') = ?".to_string());
            params.push(pr.clone());
        }
        if let Some(s) = &self.session {
            parts.push(format!(
                "json_extract(attributes, '$.{}') = ?",
                SESSION_ID_KEY
            ));
            params.push(s.clone());
        }

        (!parts.is_empty()).then(|| (format!("({})", parts.join(" AND ")), params))
    }

    /// SQL predicate fragment + bind params for the `logs` table.
    ///
    /// Logs carry no model/provider/project labels; only session is
    /// supported, via `session.id` (claude/opencode) or `conversation.id`
    /// (codex, where the two are equal).
    pub fn log_scope(&self) -> Option<(String, Vec<String>)> {
        let s = self.session.as_ref()?;
        Some((
            format!(
                "(json_extract(attributes, '$.{}') = ? OR json_extract(attributes, '$.\"conversation.id\"') = ?)",
                SESSION_ID_KEY
            ),
            vec![s.clone(), s.clone()],
        ))
    }

    /// SQL predicate fragment + bind params for the `metrics` table.
    ///
    /// Opencode metrics carry `session.id`, `project.id` and `model` labels.
    /// No metric rows exist for other agent families, so agent is not a
    /// metric predicate (an opencode-only query returns empty for them).
    pub fn metric_scope(&self) -> Option<(String, Vec<String>)> {
        let mut parts: Vec<String> = Vec::new();
        let mut params: Vec<String> = Vec::new();

        if let Some(m) = &self.model {
            parts.push("json_extract(attributes, '$.\"model\"') = ?".to_string());
            params.push(m.clone());
        }
        if let Some(pr) = &self.project {
            parts.push("json_extract(attributes, '$.\"project.id\"') = ?".to_string());
            params.push(pr.clone());
        }
        if let Some(s) = &self.session {
            parts.push(format!(
                "json_extract(attributes, '$.{}') = ?",
                SESSION_ID_KEY
            ));
            params.push(s.clone());
        }

        (!parts.is_empty()).then(|| (format!("({})", parts.join(" AND ")), params))
    }
}

/// Agent-family predicate for the `spans` table, or `None` for an
/// unrecognised family (ignored, never a 400).
fn agent_span_predicate(agent: &str) -> Option<String> {
    let scope = "json_extract(attributes, '$.\"otel.scope.name\"')";
    Some(match agent {
        a if a == an::CLAUDE => {
            format!("(name LIKE 'claude_code.%' OR {scope} LIKE 'com.anthropic.claude_code.%')")
        },
        a if a == an::OPENCODE => format!("(name LIKE 'opencode.%' OR {scope} = 'com.opencode')"),
        a if a == an::CODEX => format!("({scope} = 'codex_cli_rs')"),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_filters_apply_nothing() {
        let f = GenAiFilters::default();
        assert!(f.span_scope().is_none());
        assert!(f.log_scope().is_none());
        assert!(f.metric_scope().is_none());
        assert!(f.applied(&FILTER_DIMENSIONS).is_empty());
    }

    #[test]
    fn span_scope_covers_all_five_dimensions() {
        let f = GenAiFilters {
            agent: Some("claude".into()),
            model: Some("m1".into()),
            provider: Some("anthropic".into()),
            project: Some("p1".into()),
            session: Some("s1".into()),
        };
        let (frag, params) = f.span_scope().unwrap();
        assert!(frag.contains("name LIKE 'claude_code.%'"), "agent: {frag}");
        assert!(frag.contains("= ?"), "equality binds: {frag}");
        assert_eq!(params, vec!["m1", "anthropic", "p1", "s1"]);
        assert_eq!(
            f.applied(&FILTER_DIMENSIONS),
            vec!["agent", "model", "provider", "project", "session"]
        );
    }

    #[test]
    fn unknown_agent_is_ignored_not_applied() {
        let f = GenAiFilters {
            agent: Some("hermes".into()),
            ..Default::default()
        };
        // Unknown family: span_scope has nothing to apply, nothing echoed.
        assert!(f.span_scope().is_none());
        assert!(f.applied(&FILTER_DIMENSIONS).is_empty());
    }

    #[test]
    fn echo_only_reports_supported_dimensions() {
        let f = GenAiFilters {
            agent: Some("codex".into()),
            model: Some("m1".into()),
            session: Some("s1".into()),
            ..Default::default()
        };
        assert_eq!(f.applied(&["model", "session"]), vec!["model", "session"]);
        // codex agent IS a known family, so a supporting endpoint echoes it
        let (frag, params) = f.span_scope().unwrap();
        assert!(frag.contains("codex_cli_rs"));
        assert_eq!(params, vec!["m1", "s1"]);
    }

    #[test]
    fn log_scope_supports_session_only() {
        let f = GenAiFilters {
            model: Some("m1".into()),
            session: Some("s1".into()),
            ..Default::default()
        };
        let (frag, params) = f.log_scope().unwrap();
        assert!(frag.contains("conversation.id"), "{frag}");
        assert_eq!(params, vec!["s1", "s1"]);

        let none: GenAiFilters = GenAiFilters {
            model: Some("m1".into()),
            ..Default::default()
        };
        assert!(none.log_scope().is_none());
    }

    #[test]
    fn metric_scope_supports_session_project_model() {
        let f = GenAiFilters {
            agent: Some("claude".into()),
            model: Some("m1".into()),
            project: Some("p1".into()),
            session: Some("s1".into()),
            ..Default::default()
        };
        let (frag, params) = f.metric_scope().unwrap();
        assert!(
            !frag.contains("claude"),
            "agent is not a metric predicate: {frag}"
        );
        assert_eq!(params, vec!["m1", "p1", "s1"]);
    }
}
