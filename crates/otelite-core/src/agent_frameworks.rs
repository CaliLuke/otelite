//! Agent-framework attribute recognizers.
//!
//! Some LLM-orchestration frameworks emit structured attributes that are more
//! meaningful when grouped than when listed alphabetically. This module holds
//! the vocabulary: for each framework, which keys belong to it and which keys
//! identify a span as "an agentic span from this framework".
//!
//! Both the web frontend and the CLI/TUI render attribute groups — they all
//! read these definitions. The web frontend fetches them via
//! `/api/genai/agent_framework_defs`; Rust callers import them directly.

use serde::Serialize;

/// Definition of one framework's attribute vocabulary.
///
/// - `prefixes` and `literal_keys` together determine whether a given
///   attribute key belongs to this framework's group.
/// - `marker_keys` is the (stricter) list of attributes whose presence
///   identifies the span as originating from this framework — used by UIs to
///   decide whether to render the framework's section at all.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AgentFrameworkRecognizer {
    pub name: &'static str,
    pub prefixes: &'static [&'static str],
    pub literal_keys: &'static [&'static str],
    pub marker_keys: &'static [&'static str],
}

impl AgentFrameworkRecognizer {
    /// Does this attribute key belong to this framework's group?
    pub fn matches_key(&self, key: &str) -> bool {
        self.prefixes.iter().any(|p| key.starts_with(p)) || self.literal_keys.contains(&key)
    }

    /// Does the span carry any of this framework's marker attributes?
    /// If no marker is present but a prefix match exists, that also counts —
    /// it means the instrumentation emits framework-specific attrs but not
    /// under our preferred marker names.
    pub fn is_present<'a, I: IntoIterator<Item = &'a str>>(&self, keys: I) -> bool {
        let mut any_prefix = false;
        for k in keys {
            if self.marker_keys.contains(&k) {
                return true;
            }
            if self.prefixes.iter().any(|p| k.starts_with(p)) {
                any_prefix = true;
            }
        }
        any_prefix
    }
}

/// All recognized agent frameworks, in display order.
pub const AGENT_FRAMEWORKS: &[AgentFrameworkRecognizer] = &[
    AgentFrameworkRecognizer {
        name: "CrewAI",
        prefixes: &["crew.", "task."],
        literal_keys: &["agent.role", "agent.goal", "agent.backstory", "agent.id"],
        marker_keys: &["crew.task", "crew.id", "agent.role", "agent.goal"],
    },
    AgentFrameworkRecognizer {
        name: "AutoGen",
        prefixes: &["autogen."],
        literal_keys: &[],
        marker_keys: &["autogen.agent_name", "autogen.role"],
    },
    AgentFrameworkRecognizer {
        name: "LangGraph",
        prefixes: &["graph.", "langgraph."],
        literal_keys: &[],
        marker_keys: &[
            "graph.node",
            "graph.state",
            "langgraph.node",
            "langgraph.step",
        ],
    },
];

/// Return every key that belongs to *any* recognized agent framework. Useful
/// for excluding these keys from generic attribute lists to avoid duplication.
pub fn is_any_agent_framework_key(key: &str) -> bool {
    AGENT_FRAMEWORKS.iter().any(|fw| fw.matches_key(key))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crewai_matches_prefixes_and_literals() {
        let crew = AGENT_FRAMEWORKS
            .iter()
            .find(|f| f.name == "CrewAI")
            .unwrap();
        assert!(crew.matches_key("crew.task"));
        assert!(crew.matches_key("crew.id"));
        assert!(crew.matches_key("task.name"));
        assert!(crew.matches_key("agent.role"));
        assert!(!crew.matches_key("autogen.x"));
        assert!(!crew.matches_key("gen_ai.system"));
    }

    #[test]
    fn is_present_requires_marker_or_prefix_match() {
        let crew = AGENT_FRAMEWORKS
            .iter()
            .find(|f| f.name == "CrewAI")
            .unwrap();
        assert!(crew.is_present(["crew.task"].iter().copied()));
        // Prefix-only match (no named marker) still counts.
        assert!(crew.is_present(["crew.custom_attr"].iter().copied()));
        // Unrelated keys don't trigger presence.
        assert!(!crew.is_present(["gen_ai.system", "model"].iter().copied()));
    }

    #[test]
    fn langgraph_marker_detection() {
        let lg = AGENT_FRAMEWORKS
            .iter()
            .find(|f| f.name == "LangGraph")
            .unwrap();
        assert!(lg.is_present(["graph.node"].iter().copied()));
        assert!(lg.is_present(["langgraph.step"].iter().copied()));
        assert!(!lg.is_present(["gen_ai.system"].iter().copied()));
    }

    #[test]
    fn is_any_agent_framework_key_covers_all_three() {
        assert!(is_any_agent_framework_key("crew.task"));
        assert!(is_any_agent_framework_key("autogen.agent_name"));
        assert!(is_any_agent_framework_key("graph.state"));
        assert!(is_any_agent_framework_key("agent.role"));
        assert!(!is_any_agent_framework_key("gen_ai.system"));
        assert!(!is_any_agent_framework_key("model"));
    }
}
