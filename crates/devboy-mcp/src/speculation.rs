//! Paper 3 — speculative tool-call dispatcher.
//!
//! Given an `EnrichmentPlan` from the planner, spawns each prefetch
//! as a Tokio task, caps concurrency per `rate_limit_host` (so two
//! providers hitting the same domain share the budget), waits up to
//! `prefetch_timeout_ms` for results to land, and aborts everything
//! still pending on session shutdown.
//!
//! ## Design
//!
//! - **Dispatcher trait** — [`PrefetchDispatcher`] abstracts the
//!   actual `tools/call` path so tests can plug in a mock without
//!   pulling in MCP transport. The real impl wraps the server's
//!   handler and is wired in `SessionPipeline`.
//! - **Per-host concurrency cap** — a `Mutex<HashMap<host,
//!   in_flight>>` tracks in-flight prefetches per rate-limit host;
//!   the dispatcher refuses to schedule a call when the cap is hit.
//!   `None` host = unlimited (local tool).
//! - **Bounded synchronous wait** — [`SpeculationEngine::wait_within`]
//!   blocks at most `prefetch_timeout_ms` collecting results that
//!   landed in time; anything still pending keeps running in the
//!   background and lands later via the dedup cache.
//! - **Cascade cancellation** — [`SpeculationEngine::shutdown`] (also
//!   called from `Drop`) aborts every pending task. No orphan IO.
//!
//! Telemetry counters (`prefetch_dispatched`, `prefetch_won_race`,
//! `prefetch_wasted`) are updated by the caller; this module only
//! reports the outcomes.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::Mutex;
use tokio::task::JoinSet;
use tokio::time::timeout;

use devboy_format_pipeline::adaptive_config::EnrichmentConfig;
use devboy_format_pipeline::enrichment::PlannedCall;

/// Abstracts how a prefetch is actually executed. The real impl wraps
/// the MCP server's `tools/call` handler; the test impl returns a
/// canned body or an error after an optional sleep.
#[async_trait]
pub trait PrefetchDispatcher: Send + Sync {
    /// Execute `tool_name` with `args` out-of-band and return the
    /// response body (the same string the LLM would receive). Errors
    /// are logged at WARN by the engine and counted as wasted, never
    /// surfaced to the LLM stream.
    async fn dispatch(&self, tool_name: &str, args: Value) -> Result<String, PrefetchError>;
}

#[derive(Debug, thiserror::Error)]
pub enum PrefetchError {
    #[error("dispatcher rejected: {0}")]
    Rejected(String),
    #[error("dispatcher I/O: {0}")]
    Io(String),
    #[error("dispatcher timed out (host-level)")]
    /// HostTimeout.
    HostTimeout,
}

/// Outcome of a single prefetch task as observed by
/// [`SpeculationEngine::wait_within`].
#[derive(Debug)]
pub enum PrefetchOutcome {
    /// Prefetch landed within the timeout. The body is ready to write
    /// into the dedup cache. `predicted_cost_tokens` carries the
    /// planner's admit-time estimate so callers can pass it through
    /// to telemetry (`PipelineEvent.enricher_predicted_cost_tokens`).
    Settled {
        tool: String,
        args: Value,
        body: String,
        predicted_cost_tokens: u32,
    },
    /// Prefetch returned an error. Counted as wasted; logged at WARN.
    Failed {
        /// Tool name.
        tool: String,
        /// Underlying prefetch error.
        error: PrefetchError,
    },
    /// Prefetch was rate-limited at scheduling time — the engine
    /// never even spawned it. Used by callers to attribute the
    /// `prefetch_dispatched` gap.
    Skipped {
        /// Tool name.
        tool: String,
        /// Reason the dispatch was skipped.
        reason: SkipReason,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// `rate_limit_host` saturated (in-flight count == cap).
    HostSaturated,
    /// `enrichment.max_parallel_prefetches` reached for this turn.
    MaxParallelReached,
    /// Tool's `side_effect_class` (or `speculate=Some(false)`) blocks
    /// speculation — surfaced for telemetry but the planner already
    /// filters these out.
    NotSpeculatable,
}

/// One unit of work the engine decides about. Public so the host can
/// produce the list (combining `EnrichmentPlan.calls` with extracted
/// args from `projection`) before handing it to the engine.
#[derive(Debug, Clone)]
pub struct PrefetchRequest {
    pub call: PlannedCall,
    pub args: Value,
    /// Pre-computed rate-limit host for this call. `None` = uncapped.
    /// Built by the host from either the static
    /// `ToolValueModel.rate_limit_host` or the runtime
    /// `ToolEnricher::rate_limit_host(args)` — whichever resolved.
    pub rate_limit_host: Option<String>,
}

/// Per-host in-flight counter. Cheap clone (Arc).
#[derive(Default, Clone)]
pub struct HostBudget {
    counts: Arc<Mutex<HashMap<String, u32>>>,
}

impl HostBudget {
    /// New.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns `true` and increments the count if `host` has room
    /// under `cap`; returns `false` (no change) otherwise. `cap = 0`
    /// blocks all calls to this host (defensive default).
    pub async fn try_acquire(&self, host: &str, cap: u32) -> bool {
        if cap == 0 {
            return false;
        }
        let mut g = self.counts.lock().await;
        let entry = g.entry(host.to_string()).or_insert(0);
        if *entry >= cap {
            return false;
        }
        *entry = entry.saturating_add(1);
        true
    }

    /// Decrement the count for `host`. No-op if the count is already 0
    /// (mismatched release-without-acquire — defensive, can't go neg).
    pub async fn release(&self, host: &str) {
        let mut g = self.counts.lock().await;
        if let Some(entry) = g.get_mut(host) {
            *entry = entry.saturating_sub(1);
            if *entry == 0 {
                g.remove(host);
            }
        }
    }

    /// Snapshot of in-flight counts — for telemetry / tests.
    pub async fn snapshot(&self) -> HashMap<String, u32> {
        self.counts.lock().await.clone()
    }
}

/// Per-turn speculation engine. One instance per `SessionPipeline`;
/// holds the JoinSet and the host budget. Drop = shutdown.
pub struct SpeculationEngine {
    config: EnrichmentConfig,
    dispatcher: Arc<dyn PrefetchDispatcher>,
    budget: HostBudget,
    /// Pending tasks for the current turn. Cleared at the end of each
    /// `wait_within`; orphans get aborted by `shutdown` / `Drop`.
    join_set: JoinSet<TaskResult>,
    /// Default per-host concurrency cap when the call has a host.
    /// Picked at 4 — enough for top-3 prefetch fan-out plus one
    /// in-flight from the main flow without saturating typical APIs.
    per_host_cap: u32,
}

struct TaskResult {
    tool: String,
    args: Value,
    body: Result<String, PrefetchError>,
    predicted_cost_tokens: u32,
    /// Carried through for future per-host telemetry on the result
    /// path. Today, the budget is released inside the spawned task
    /// before this value is read by `wait_within` — so this field
    /// is only logged in WARN traces. Kept for symmetry with
    /// `PrefetchRequest.rate_limit_host`; downstream may surface it.
    #[allow(dead_code)]
    rate_limit_host: Option<String>,
}

impl SpeculationEngine {
    /// Build a fresh engine bound to a `dispatcher`. Settings come
    /// from `config.enrichment`.
    pub fn new(config: EnrichmentConfig, dispatcher: Arc<dyn PrefetchDispatcher>) -> Self {
        Self {
            config,
            dispatcher,
            budget: HostBudget::new(),
            join_set: JoinSet::new(),
            per_host_cap: 4,
        }
    }

    /// Build with an explicit per-host cap. Useful for tests that want
    /// `cap = 1` to force serialisation.
    pub fn with_per_host_cap(mut self, cap: u32) -> Self {
        self.per_host_cap = cap;
        self
    }

    /// `true` when `enrichment.enabled = false` — host should skip
    /// all dispatch and not even build a plan.
    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    /// Returns the configured wall-clock budget for one wait_within
    /// call.
    pub fn timeout(&self) -> Duration {
        Duration::from_millis(self.config.prefetch_timeout_ms.into())
    }

    /// Number of currently-spawned tasks (incl. pending ones from
    /// previous turns that have not been collected yet).
    pub fn pending(&self) -> usize {
        self.join_set.len()
    }

    /// Schedule `requests` honouring `max_parallel_prefetches` and
    /// per-host caps. Requests rejected by either gate are reported
    /// as `Skipped` outcomes and **not** spawned.
    ///
    /// Returns the immediately-known outcomes (skips). Settled /
    /// failed outcomes come back from [`Self::wait_within`].
    pub async fn dispatch(&mut self, requests: Vec<PrefetchRequest>) -> Vec<PrefetchOutcome> {
        let mut skips = Vec::new();
        let mut spawned = 0u32;
        let max = self.config.max_parallel_prefetches;
        for req in requests {
            if spawned >= max {
                skips.push(PrefetchOutcome::Skipped {
                    tool: req.call.tool.clone(),
                    reason: SkipReason::MaxParallelReached,
                });
                continue;
            }
            // Honour rate-limit host. Uncapped tools (None) fall
            // through; capped ones must acquire a slot first.
            if self.config.respect_rate_limits
                && let Some(host) = &req.rate_limit_host
                && !self.budget.try_acquire(host, self.per_host_cap).await
            {
                skips.push(PrefetchOutcome::Skipped {
                    tool: req.call.tool.clone(),
                    reason: SkipReason::HostSaturated,
                });
                continue;
            }

            let dispatcher = Arc::clone(&self.dispatcher);
            let tool = req.call.tool.clone();
            let args = req.args.clone();
            let host = req.rate_limit_host.clone();
            let predicted_cost_tokens = req.call.estimated_cost_tokens;
            let budget = self.budget.clone();
            let respects = self.config.respect_rate_limits;
            self.join_set.spawn(async move {
                let body = dispatcher.dispatch(&tool, args.clone()).await;
                // Release the per-host slot whether we succeeded or
                // failed — the call has stopped occupying the API.
                if respects && let Some(h) = &host {
                    budget.release(h).await;
                }
                TaskResult {
                    tool,
                    args,
                    body,
                    predicted_cost_tokens,
                    rate_limit_host: host,
                }
            });
            spawned += 1;
        }
        skips
    }

    /// Wait up to `prefetch_timeout_ms` collecting outcomes for tasks
    /// that complete in time. Tasks still pending stay in the
    /// JoinSet — their results arrive on the next `wait_within` (or
    /// get cancelled by `shutdown`).
    ///
    /// The timeout is a **global deadline** for the whole call, not
    /// a per-task budget — N slow prefetches finishing one-by-one
    /// just under the threshold can no longer stall the response
    /// for `N × prefetch_timeout_ms`.
    ///
    /// Returns `Settled` for every task that returned `Ok(body)`,
    /// `Failed` for every `Err(...)`. Skipped outcomes from the most
    /// recent `dispatch` call are not echoed here — the host already
    /// has them.
    pub async fn wait_within(&mut self) -> Vec<PrefetchOutcome> {
        let mut out = Vec::new();
        let deadline = tokio::time::Instant::now() + self.timeout();
        loop {
            if self.join_set.is_empty() {
                break;
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                tracing::debug!(
                    target: "devboy_mcp::speculation",
                    "prefetch_timeout_ms reached with {} tasks still pending",
                    self.join_set.len()
                );
                break;
            }
            match tokio::time::timeout_at(deadline, self.join_set.join_next()).await {
                Ok(Some(Ok(task_result))) => {
                    let predicted = task_result.predicted_cost_tokens;
                    out.push(match task_result.body {
                        Ok(body) => PrefetchOutcome::Settled {
                            tool: task_result.tool,
                            args: task_result.args,
                            body,
                            predicted_cost_tokens: predicted,
                        },
                        Err(error) => PrefetchOutcome::Failed {
                            tool: task_result.tool,
                            error,
                        },
                    });
                }
                Ok(Some(Err(join_err))) => {
                    tracing::warn!(
                        target: "devboy_mcp::speculation",
                        "prefetch task panicked or was cancelled: {join_err}"
                    );
                    out.push(PrefetchOutcome::Failed {
                        tool: "<unknown>".into(),
                        error: PrefetchError::Io(join_err.to_string()),
                    });
                }
                Ok(None) => break, // empty join_set
                Err(_elapsed) => {
                    // Global deadline expired. Remaining tasks stay in
                    // the JoinSet so a future `drain_pending()` (or
                    // another `wait_within` cycle) can still collect
                    // their results into the dedup cache.
                    tracing::debug!(
                        target: "devboy_mcp::speculation",
                        "prefetch_timeout_ms reached with {} tasks still pending",
                        self.join_set.len()
                    );
                    break;
                }
            }
        }
        out
    }

    /// Best-effort drain of completed tasks **without blocking**.
    ///
    /// Returns outcomes for every task that has already finished,
    /// leaves still-pending tasks alone. Lets the host call this on
    /// the next turn (or before each `dispatch`) to recover bodies
    /// that landed after the previous `wait_within` timed out, so
    /// they can still be written into the dedup cache instead of
    /// being silently lost on the next `shutdown`.
    pub async fn drain_pending(&mut self) -> Vec<PrefetchOutcome> {
        let mut out = Vec::new();
        loop {
            if self.join_set.is_empty() {
                break;
            }
            // 0-duration timeout = "non-blocking poll".
            match timeout(Duration::from_millis(0), self.join_set.join_next()).await {
                Ok(Some(Ok(task_result))) => {
                    let predicted = task_result.predicted_cost_tokens;
                    out.push(match task_result.body {
                        Ok(body) => PrefetchOutcome::Settled {
                            tool: task_result.tool,
                            args: task_result.args,
                            body,
                            predicted_cost_tokens: predicted,
                        },
                        Err(error) => PrefetchOutcome::Failed {
                            tool: task_result.tool,
                            error,
                        },
                    });
                }
                Ok(Some(Err(join_err))) => {
                    out.push(PrefetchOutcome::Failed {
                        tool: "<unknown>".into(),
                        error: PrefetchError::Io(join_err.to_string()),
                    });
                }
                Ok(None) | Err(_) => break,
            }
        }
        out
    }

    /// Abort every still-pending task. Idempotent.
    pub async fn shutdown(&mut self) {
        self.join_set.abort_all();
        // Drain so the abort_all signal is observed before the engine
        // is dropped — without this, ASAN-style runtimes could complain
        // about tasks holding outstanding waker references.
        while self.join_set.join_next().await.is_some() {}
    }
}

impl Drop for SpeculationEngine {
    fn drop(&mut self) {
        // `JoinSet::abort_all` is a sync call — safe in Drop. We do
        // not poll for completion here (sync context), but `Tokio`'s
        // task abort is delivered to each worker on its next yield.
        self.join_set.abort_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use devboy_format_pipeline::enrichment::PlannedCall;
    use std::sync::atomic::{AtomicU32, Ordering};

    struct MockDispatcher {
        delay_ms: u64,
        call_count: Arc<AtomicU32>,
        fail_for: Option<String>,
    }

    #[async_trait]
    impl PrefetchDispatcher for MockDispatcher {
        async fn dispatch(&self, tool: &str, args: Value) -> Result<String, PrefetchError> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(self.delay_ms)).await;
            if Some(tool.to_string()) == self.fail_for {
                return Err(PrefetchError::Io("simulated failure".into()));
            }
            Ok(format!("mock-body for {tool} args={args}"))
        }
    }

    fn req(tool: &str, host: Option<&str>) -> PrefetchRequest {
        PrefetchRequest {
            call: PlannedCall {
                tool: tool.into(),
                projection: None,
                probability: 1.0,
                estimated_cost_bytes: 1024,
                estimated_cost_tokens: 256,
                value_class: devboy_core::ValueClass::Critical,
            },
            args: serde_json::json!({"x": 1}),
            rate_limit_host: host.map(String::from),
        }
    }

    fn cfg(timeout_ms: u32, max_parallel: u32) -> EnrichmentConfig {
        EnrichmentConfig {
            enabled: true,
            max_parallel_prefetches: max_parallel,
            prefetch_budget_tokens: 8000,
            prefetch_timeout_ms: timeout_ms,
            respect_rate_limits: true,
        }
    }

    #[tokio::test]
    async fn settled_outcome_returned_when_within_budget() {
        let count = Arc::new(AtomicU32::new(0));
        let mut engine = SpeculationEngine::new(
            cfg(500, 5),
            Arc::new(MockDispatcher {
                delay_ms: 10,
                call_count: count.clone(),
                fail_for: None,
            }),
        );
        let skips = engine
            .dispatch(vec![req("Read", None), req("Read", None)])
            .await;
        assert!(skips.is_empty(), "no skips expected: {skips:?}");
        let outcomes = engine.wait_within().await;
        assert_eq!(outcomes.len(), 2);
        for o in outcomes {
            match o {
                PrefetchOutcome::Settled { body, .. } => assert!(body.contains("mock-body")),
                other => panic!("expected Settled, got {other:?}"),
            }
        }
        assert_eq!(count.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn timeout_leaves_slow_prefetches_pending() {
        let count = Arc::new(AtomicU32::new(0));
        let mut engine = SpeculationEngine::new(
            cfg(50, 5),
            Arc::new(MockDispatcher {
                delay_ms: 500,
                call_count: count.clone(),
                fail_for: None,
            }),
        );
        engine.dispatch(vec![req("SlowTool", None)]).await;
        let outcomes = engine.wait_within().await;
        // Wall-clock budget hit before the dispatcher returned.
        assert!(
            outcomes.is_empty(),
            "expected no settled within 50ms timeout"
        );
        assert_eq!(engine.pending(), 1, "task must still be in JoinSet");
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn max_parallel_skips_excess_requests() {
        let count = Arc::new(AtomicU32::new(0));
        let mut engine = SpeculationEngine::new(
            cfg(500, 2),
            Arc::new(MockDispatcher {
                delay_ms: 5,
                call_count: count.clone(),
                fail_for: None,
            }),
        );
        let skips = engine
            .dispatch(vec![
                req("A", None),
                req("B", None),
                req("C", None),
                req("D", None),
            ])
            .await;
        assert_eq!(skips.len(), 2, "C+D must skip — max_parallel=2");
        for s in &skips {
            assert!(matches!(
                s,
                PrefetchOutcome::Skipped {
                    reason: SkipReason::MaxParallelReached,
                    ..
                }
            ));
        }
        let settled = engine.wait_within().await;
        assert_eq!(settled.len(), 2);
    }

    #[tokio::test]
    async fn host_saturation_is_observed_across_dispatches() {
        let count = Arc::new(AtomicU32::new(0));
        let dispatcher = Arc::new(MockDispatcher {
            delay_ms: 100,
            call_count: count.clone(),
            fail_for: None,
        });
        // Cap = 1 in-flight per host, max_parallel large enough.
        let mut engine = SpeculationEngine::new(cfg(500, 10), dispatcher).with_per_host_cap(1);
        // First call grabs the slot.
        let skips1 = engine
            .dispatch(vec![req("ToolA", Some("api.github.com"))])
            .await;
        assert!(skips1.is_empty());
        // Second call to the same host while first is in-flight: SKIP.
        let skips2 = engine
            .dispatch(vec![req("ToolB", Some("api.github.com"))])
            .await;
        assert_eq!(skips2.len(), 1);
        assert!(matches!(
            skips2[0],
            PrefetchOutcome::Skipped {
                reason: SkipReason::HostSaturated,
                ..
            }
        ));
        // Drain the first one so the slot frees up.
        engine.wait_within().await;
        // Now the same host has room again.
        let skips3 = engine
            .dispatch(vec![req("ToolC", Some("api.github.com"))])
            .await;
        assert!(skips3.is_empty(), "after drain the slot must be free");
        engine.wait_within().await;
    }

    #[tokio::test]
    async fn different_hosts_share_no_budget() {
        let count = Arc::new(AtomicU32::new(0));
        let mut engine = SpeculationEngine::new(
            cfg(500, 10),
            Arc::new(MockDispatcher {
                delay_ms: 5,
                call_count: count.clone(),
                fail_for: None,
            }),
        )
        .with_per_host_cap(1);
        let skips = engine
            .dispatch(vec![
                req("A", Some("api.github.com")),
                req("B", Some("gitlab.example.com")),
                req("C", Some("api.openai.com")),
            ])
            .await;
        assert!(skips.is_empty(), "different hosts must each get a slot");
        let settled = engine.wait_within().await;
        assert_eq!(settled.len(), 3);
    }

    #[tokio::test]
    async fn dispatcher_failure_surfaces_as_failed_outcome() {
        let count = Arc::new(AtomicU32::new(0));
        let mut engine = SpeculationEngine::new(
            cfg(500, 5),
            Arc::new(MockDispatcher {
                delay_ms: 5,
                call_count: count.clone(),
                fail_for: Some("Bad".into()),
            }),
        );
        engine
            .dispatch(vec![req("Bad", None), req("Good", None)])
            .await;
        let outcomes = engine.wait_within().await;
        assert_eq!(outcomes.len(), 2);
        let failed = outcomes
            .iter()
            .find(|o| matches!(o, PrefetchOutcome::Failed { tool, .. } if tool == "Bad"));
        assert!(failed.is_some(), "expected Failed for Bad");
    }

    #[tokio::test]
    async fn shutdown_aborts_pending_tasks() {
        let count = Arc::new(AtomicU32::new(0));
        let mut engine = SpeculationEngine::new(
            cfg(50, 5),
            Arc::new(MockDispatcher {
                delay_ms: 10_000,
                call_count: count.clone(),
                fail_for: None,
            }),
        );
        engine.dispatch(vec![req("LongRunning", None)]).await;
        // Don't wait for them — go straight to shutdown.
        engine.shutdown().await;
        assert_eq!(engine.pending(), 0, "shutdown must drain JoinSet");
    }

    #[tokio::test]
    async fn host_budget_release_after_failure() {
        let count = Arc::new(AtomicU32::new(0));
        let mut engine = SpeculationEngine::new(
            cfg(500, 5),
            Arc::new(MockDispatcher {
                delay_ms: 5,
                call_count: count.clone(),
                fail_for: Some("Failing".into()),
            }),
        )
        .with_per_host_cap(1);
        engine
            .dispatch(vec![req("Failing", Some("host.example.org"))])
            .await;
        engine.wait_within().await;
        // After failure, the slot must have been released.
        let snap = engine.budget.snapshot().await;
        assert!(
            !snap.contains_key("host.example.org")
                || snap.get("host.example.org").copied() == Some(0),
            "host budget must release on failure: {snap:?}"
        );
    }

    #[tokio::test]
    async fn stress_50_requests_3_hosts_cap_2_per_host() {
        // QA: realistic load — 50 prefetches fan out across 3 hosts
        // (api.github.com, api.openai.com, gitlab.com), per-host cap 2
        // and max_parallel 6. Expect:
        //   - settled count ≤ max_parallel × ceil(host_groups / cap)
        //   - skip count = 50 − settled
        //   - no panics, no orphan tasks at the end.
        let count = Arc::new(AtomicU32::new(0));
        let mut engine = SpeculationEngine::new(
            cfg(2_000, 6),
            Arc::new(MockDispatcher {
                delay_ms: 5,
                call_count: count.clone(),
                fail_for: None,
            }),
        )
        .with_per_host_cap(2);
        let hosts = ["api.github.com", "api.openai.com", "gitlab.com"];
        let mut requests = Vec::new();
        for i in 0..50 {
            requests.push(req("ToolX", Some(hosts[i % hosts.len()])));
        }
        let skips = engine.dispatch(requests).await;
        // First batch: only 6 fit (max_parallel), rest skip on max_parallel
        // — host saturation kicks in only after some have completed.
        assert!(
            skips.len() >= 44,
            "expected ≥ 44 skipped (cap 6 + per-host limits), got {}",
            skips.len()
        );
        // Settled count = up to 6 (max_parallel cap)
        let settled = engine.wait_within().await;
        let settled_ok = settled
            .iter()
            .filter(|o| matches!(o, PrefetchOutcome::Settled { .. }))
            .count();
        assert!(
            settled_ok <= 6,
            "settled must respect max_parallel=6, got {settled_ok}"
        );
        engine.shutdown().await;
        // No orphans.
        assert_eq!(engine.pending(), 0);
    }

    #[tokio::test]
    async fn rate_limit_disabled_in_config_lets_everything_through() {
        let count = Arc::new(AtomicU32::new(0));
        let mut cfg_no_rl = cfg(500, 10);
        cfg_no_rl.respect_rate_limits = false;
        let mut engine = SpeculationEngine::new(
            cfg_no_rl,
            Arc::new(MockDispatcher {
                delay_ms: 5,
                call_count: count.clone(),
                fail_for: None,
            }),
        )
        .with_per_host_cap(1);
        // Three calls to the same host with cap=1 — but respect=false
        // means the cap is not enforced.
        let skips = engine
            .dispatch(vec![
                req("A", Some("api.github.com")),
                req("B", Some("api.github.com")),
                req("C", Some("api.github.com")),
            ])
            .await;
        assert!(skips.is_empty());
        let settled = engine.wait_within().await;
        assert_eq!(settled.len(), 3);
    }
}
