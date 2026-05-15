use otelite_core::api::{
    CacheHitRateByModel, ConversationDepthStats, ErrorTypeBreakdown, LatencyStats, ModelDriftPair,
    TokenUsageResponse, ToolUsage, TruncationRateByModel,
};

/// State for the Usage analytics view.
#[derive(Debug, Default)]
pub struct UsageState {
    pub token_usage: Option<TokenUsageResponse>,
    pub latency_stats: Vec<LatencyStats>,
    pub truncation_rate: Vec<TruncationRateByModel>,
    pub cache_hit_rate: Vec<CacheHitRateByModel>,
    pub conversation_depth: Option<ConversationDepthStats>,
    pub tool_usage: Vec<ToolUsage>,
    pub error_types: Vec<ErrorTypeBreakdown>,
    pub model_drift: Vec<ModelDriftPair>,
    pub error: Option<String>,
    pub is_loading: bool,
}

impl UsageState {
    pub fn set_error(&mut self, msg: String) {
        self.error = Some(msg);
        self.is_loading = false;
    }

    pub fn clear_error(&mut self) {
        self.error = None;
    }
}
