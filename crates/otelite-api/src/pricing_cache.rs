//! Server-side cache of the LiteLLM pricing database.
//!
//! On server startup we spawn a background task that fetches the LiteLLM JSON
//! from GitHub and swaps it into an `ArcSwap`-style `RwLock`. If the fetch
//! fails (offline, rate-limit, upstream 5xx) the cache stays at whatever it
//! was — `PricingDatabase::empty()` initially, meaning the Claude fallback
//! table in `otelite-core::pricing` handles every lookup until the first
//! successful refresh.
//!
//! The background task then sleeps for [`REFRESH_INTERVAL`] and tries again.
//! Failures are logged but never fatal.

use otelite_core::pricing::{PricingDatabase, LITELLM_RAW_URL};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// Refresh the pricing database once per day. LiteLLM updates the JSON when new
/// models are added or prices change, typically weekly, so daily is ample.
pub const REFRESH_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// Initial fetch timeout — if GitHub is slow we'd rather start the server with
/// an empty cache and refresh in the background than block startup.
pub const INITIAL_FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// Ongoing refresh timeout can be more generous.
pub const REFRESH_FETCH_TIMEOUT: Duration = Duration::from_secs(30);

/// Thread-safe handle to the current pricing database.
#[derive(Clone)]
pub struct PricingCache {
    inner: Arc<RwLock<PricingState>>,
}

#[derive(Clone)]
pub struct PricingState {
    pub db: PricingDatabase,
    /// Unix millis of the last successful LiteLLM fetch, or None if never.
    pub last_fetched_unix_ms: Option<i64>,
    /// Unix millis of the last failed LiteLLM fetch (for surfacing staleness).
    pub last_failed_unix_ms: Option<i64>,
}

impl PricingCache {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(PricingState {
                db: PricingDatabase::empty(),
                last_fetched_unix_ms: None,
                last_failed_unix_ms: None,
            })),
        }
    }

    pub async fn snapshot(&self) -> PricingState {
        self.inner.read().await.clone()
    }

    /// Fetch LiteLLM JSON and swap into the cache. Returns Ok even if we had
    /// to fall back to the empty DB — the only error is a panicked lock.
    async fn refresh(&self, timeout: Duration) {
        let client = match reqwest::Client::builder().timeout(timeout).build() {
            Ok(c) => c,
            Err(e) => {
                warn!("pricing: failed to build HTTP client: {e}");
                return;
            },
        };
        match client.get(LITELLM_RAW_URL).send().await {
            Ok(resp) if resp.status().is_success() => {
                let body = match resp.text().await {
                    Ok(b) => b,
                    Err(e) => {
                        warn!("pricing: failed to read LiteLLM response: {e}");
                        self.record_failure().await;
                        return;
                    },
                };
                match PricingDatabase::from_litellm_json(&body) {
                    Ok(db) => {
                        let count = db.len();
                        let mut state = self.inner.write().await;
                        state.db = db;
                        state.last_fetched_unix_ms = Some(now_unix_ms());
                        info!("pricing: loaded {count} LiteLLM entries");
                    },
                    Err(e) => {
                        warn!("pricing: failed to parse LiteLLM JSON: {e}");
                        self.record_failure().await;
                    },
                }
            },
            Ok(resp) => {
                warn!("pricing: LiteLLM fetch returned HTTP {}", resp.status());
                self.record_failure().await;
            },
            Err(e) => {
                debug!("pricing: LiteLLM fetch failed: {e}");
                self.record_failure().await;
            },
        }
    }

    async fn record_failure(&self) {
        let mut state = self.inner.write().await;
        state.last_failed_unix_ms = Some(now_unix_ms());
    }

    /// Spawn a background task that does the initial fetch and then refreshes
    /// on a timer. Returns a clone of the cache for the caller to retain.
    pub fn spawn_refresher(self) -> Self {
        let task_cache = self.clone();
        tokio::spawn(async move {
            task_cache.refresh(INITIAL_FETCH_TIMEOUT).await;
            let mut ticker = tokio::time::interval(REFRESH_INTERVAL);
            // The interval's first tick fires immediately — skip past it since
            // we just fetched.
            ticker.tick().await;
            loop {
                ticker.tick().await;
                task_cache.refresh(REFRESH_FETCH_TIMEOUT).await;
            }
        });
        self
    }
}

impl Default for PricingCache {
    fn default() -> Self {
        Self::new()
    }
}

fn now_unix_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
