//! Telemetry data types for OTLP signals

pub mod formatting;
pub mod genai;
pub mod log;
pub mod metric;
pub mod resource;
pub mod trace;

pub use formatting::{format_attribute_preview, format_attribute_value};
pub use genai::{classify_ttft_value, extract_ttft_secs, GenAiSpanInfo, TtftValueQuality};
pub use log::LogRecord;
pub use metric::Metric;
pub use resource::Resource;
pub use trace::{Span, Trace};
