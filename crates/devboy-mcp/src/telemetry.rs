//! Telemetry pipeline — records every tool invocation and ships batches to a
//! configurable HTTP endpoint, even when the call was executed locally.
//!
//! # Why on the proxy side?
//!
//! With transparent routing, some tool calls never reach an upstream MCP server.
//! Operators who aggregate usage elsewhere (usage dashboards, billing, audit) still
//! want those invocations to show up somewhere, so the proxy forwards a minimal event
//! payload (no tool arguments, no responses) to the endpoint configured under
//! `[proxy.telemetry].endpoint`. When no endpoint is configured, the pipeline still
//! buffers events locally but never attempts an upload.
//!
//! # Back pressure
//!
//! A single bounded in-memory queue with drop-oldest semantics. We deliberately avoid
//! SQLite here — the queue exists to ride out short connectivity hiccups, not to persist
//! across restarts. If the operator wants durable telemetry they can run the proxy in a
//! supervised process with a writable working directory and a `JSONL` sink (future
//! extension).
//!
//! # Scheduling
//!
//! The uploader flushes whenever either:
//! - the buffer reaches `batch_size`, or
//! - `batch_interval_secs` have elapsed since the last flush attempt.
//!
//! When the endpoint is unset the pipeline still records events (helpful for CLI debug
//! commands) but never attempts an upload.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use devboy_core::config::ProxyTelemetryConfig;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, Notify};
use tokio::task::JoinHandle;
use tracing::{debug, warn};

/// Status of a single tool invocation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TelemetryStatus {
    Success,
    Error,
}

/// Minimal event payload — intentionally excludes tool arguments and responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryEvent {
    /// Unprefixed tool name (`get_issues`).
    pub tool: String,
    /// Routing decision label (`strategy_remote`, `override_rule`, …).
    pub routing_decision: String,
    /// Optional detail for `override_rule` (the pattern that matched).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routing_detail: Option<String>,
    /// Upstream prefix when the call went remote (`cloud`); `None` for local.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream: Option<String>,
    pub status: TelemetryStatus,
    /// Wall-clock latency observed by the proxy.
    pub latency_ms: u64,
    /// Unix epoch seconds at the time the decision was made.
    pub timestamp_secs: u64,
    /// Optional fallback marker — true if the primary executor failed and we retried.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub was_fallback: bool,
}

impl TelemetryEvent {
    /// Now.
    pub fn now(tool: impl Into<String>, routing_decision: impl Into<String>) -> Self {
        Self {
            tool: tool.into(),
            routing_decision: routing_decision.into(),
            routing_detail: None,
            upstream: None,
            status: TelemetryStatus::Success,
            latency_ms: 0,
            timestamp_secs: unix_now(),
            was_fallback: false,
        }
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Authentication credentials used when uploading batches.
#[derive(Clone, Default)]
pub struct TelemetryAuth {
    pub bearer_token: Option<secrecy::SecretString>,
}

/// Request body shape expected by the backend (documented for the future endpoint).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryBatch {
    pub events: Vec<TelemetryEvent>,
}

/// Shared event buffer. `record()` is cheap and lock-bound; clones are light (Arc).
///
/// Internally carries a `Notify` that the buffer signals when the queue length crosses
/// the `flush_threshold` — the uploader waits on this alongside its periodic ticker so
/// that "batch_size reached" triggers an immediate flush without waiting for the next
/// interval tick.
#[derive(Clone)]
pub struct TelemetryBuffer {
    inner: Arc<Mutex<VecDeque<TelemetryEvent>>>,
    capacity: usize,
    /// Threshold at which `record()` wakes the flusher. Set by the pipeline owner; a
    /// freshly constructed buffer uses `capacity` (effectively "never size-triggered").
    flush_threshold: Arc<std::sync::atomic::AtomicUsize>,
    /// Signal for the background flush task to wake immediately.
    size_trigger: Arc<Notify>,
}

impl TelemetryBuffer {
    /// New.
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(VecDeque::with_capacity(capacity.min(1024)))),
            capacity,
            flush_threshold: Arc::new(std::sync::atomic::AtomicUsize::new(capacity)),
            size_trigger: Arc::new(Notify::new()),
        }
    }

    /// Set the queue length at which `record()` wakes the flusher. Called by the
    /// pipeline once `batch_size` is known.
    pub fn set_flush_threshold(&self, threshold: usize) {
        self.flush_threshold
            .store(threshold.max(1), std::sync::atomic::Ordering::Relaxed);
    }

    /// Notifier the background flusher waits on for size-triggered flushes.
    pub fn size_trigger(&self) -> Arc<Notify> {
        self.size_trigger.clone()
    }

    /// Push an event. When the queue is full, drops the oldest event to make room.
    /// Wakes the background flusher as soon as the queue length crosses
    /// [`Self::set_flush_threshold`].
    pub async fn record(&self, event: TelemetryEvent) {
        let (len_after, threshold) = {
            let mut guard = self.inner.lock().await;
            while guard.len() >= self.capacity {
                let dropped = guard.pop_front();
                debug!(
                    dropped = ?dropped.as_ref().map(|e| e.tool.as_str()),
                    "telemetry buffer full, dropping oldest event"
                );
            }
            guard.push_back(event);
            (
                guard.len(),
                self.flush_threshold
                    .load(std::sync::atomic::Ordering::Relaxed),
            )
        };
        if len_after >= threshold {
            self.size_trigger.notify_one();
        }
    }

    /// Drain up to `max` events without releasing the lock between pops.
    pub async fn drain(&self, max: usize) -> Vec<TelemetryEvent> {
        let mut guard = self.inner.lock().await;
        let n = guard.len().min(max);
        guard.drain(..n).collect()
    }

    /// Return events to the front of the queue (after a failed upload).
    pub async fn requeue_front(&self, events: Vec<TelemetryEvent>) {
        let mut guard = self.inner.lock().await;
        for event in events.into_iter().rev() {
            if guard.len() >= self.capacity {
                // Full — drop the last event (tail) to keep the retried batch prioritized.
                guard.pop_back();
            }
            guard.push_front(event);
        }
    }

    pub async fn len(&self) -> usize {
        self.inner.lock().await.len()
    }

    pub async fn is_empty(&self) -> bool {
        self.inner.lock().await.is_empty()
    }
}

/// Uploader — owns the HTTP client and knows how to ship a batch to `endpoint`.
#[derive(Clone)]
pub struct TelemetryUploader {
    endpoint: String,
    auth: TelemetryAuth,
    http: reqwest::Client,
}

impl TelemetryUploader {
    /// New.
    pub fn new(endpoint: String, auth: TelemetryAuth) -> devboy_core::Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(|e| devboy_core::Error::Http(format!("telemetry client build: {}", e)))?;
        Ok(Self {
            endpoint,
            auth,
            http,
        })
    }

    /// POST a batch; returns Ok when the server accepted it (2xx).
    pub async fn upload(&self, batch: &TelemetryBatch) -> devboy_core::Result<()> {
        let mut req = self
            .http
            .post(&self.endpoint)
            .header("content-type", "application/json");
        if let Some(token) = &self.auth.bearer_token {
            use secrecy::ExposeSecret;
            req = req.header("authorization", format!("Bearer {}", token.expose_secret()));
        }
        let resp = req.json(batch).send().await.map_err(|e| {
            devboy_core::Error::Http(format!("telemetry upload to {}: {}", self.endpoint, e))
        })?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(devboy_core::Error::Http(format!(
                "telemetry upload rejected: HTTP {} — {}",
                status, body
            )));
        }
        Ok(())
    }
}

/// The full pipeline — owns the buffer, an uploader, and a background flush task.
///
/// Callers get a cheap `TelemetryBuffer` clone via [`TelemetryPipeline::buffer`] so hot
/// paths do not need to pay for the uploader machinery.
pub struct TelemetryPipeline {
    buffer: TelemetryBuffer,
    config: ProxyTelemetryConfig,
    uploader: Option<TelemetryUploader>,
    task: Option<JoinHandle<()>>,
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
}

impl TelemetryPipeline {
    /// New.
    pub fn new(config: ProxyTelemetryConfig) -> Self {
        let capacity = config.offline_queue_max.max(16);
        let buffer = TelemetryBuffer::new(capacity);
        Self {
            buffer,
            config,
            uploader: None,
            task: None,
            shutdown_tx: None,
        }
    }

    /// Buffer.
    pub fn buffer(&self) -> TelemetryBuffer {
        self.buffer.clone()
    }

    /// Config.
    pub fn config(&self) -> &ProxyTelemetryConfig {
        &self.config
    }

    /// Wire up the uploader and start the flush task. No-op when telemetry is disabled
    /// or when `endpoint` is not set.
    pub fn start(&mut self, auth: TelemetryAuth) -> devboy_core::Result<()> {
        if !self.config.enabled {
            debug!("telemetry disabled in config; skipping uploader");
            return Ok(());
        }
        let Some(endpoint) = self.config.endpoint.clone() else {
            debug!("telemetry endpoint unset; events buffered but not uploaded");
            return Ok(());
        };

        let uploader = TelemetryUploader::new(endpoint, auth)?;
        self.uploader = Some(uploader.clone());

        let buffer = self.buffer.clone();
        let batch_size = self.config.batch_size.max(1);
        let interval = Duration::from_secs(self.config.batch_interval_secs.max(1));
        // Arm the buffer so `record()` pokes the flusher the moment the queue reaches
        // `batch_size` — satisfies the "batch_size reached OR interval elapsed"
        // contract without waiting up to `batch_interval_secs` every time.
        buffer.set_flush_threshold(batch_size);
        let size_trigger = buffer.size_trigger();
        let (tx, mut rx) = tokio::sync::oneshot::channel();
        self.shutdown_tx = Some(tx);

        let task = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _ = &mut rx => {
                        // Best-effort flush on shutdown.
                        let events = buffer.drain(usize::MAX).await;
                        if !events.is_empty() {
                            let _ = uploader.upload(&TelemetryBatch { events }).await;
                        }
                        break;
                    }
                    _ = ticker.tick() => {
                        flush_once(&buffer, &uploader, batch_size).await;
                    }
                    _ = size_trigger.notified() => {
                        flush_once(&buffer, &uploader, batch_size).await;
                    }
                }
            }
        });

        self.task = Some(task);
        Ok(())
    }

    /// Force an immediate flush. Returns the number of events uploaded.
    pub async fn flush(&self) -> devboy_core::Result<usize> {
        let Some(uploader) = &self.uploader else {
            return Ok(0);
        };
        let events = self.buffer.drain(usize::MAX).await;
        let n = events.len();
        if !events.is_empty()
            && let Err(e) = uploader
                .upload(&TelemetryBatch {
                    events: events.clone(),
                })
                .await
        {
            // Requeue on failure to preserve at-least-once semantics.
            self.buffer.requeue_front(events).await;
            return Err(e);
        }
        Ok(n)
    }

    /// Gracefully stop the background task, doing one final flush attempt.
    pub async fn shutdown(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(handle) = self.task.take() {
            let _ = handle.await;
        }
    }
}

async fn flush_once(buffer: &TelemetryBuffer, uploader: &TelemetryUploader, batch_size: usize) {
    loop {
        let events = buffer.drain(batch_size).await;
        if events.is_empty() {
            return;
        }
        match uploader
            .upload(&TelemetryBatch {
                events: events.clone(),
            })
            .await
        {
            Ok(_) => {
                debug!(count = events.len(), "telemetry batch uploaded");
                if events.len() < batch_size {
                    return;
                }
            }
            Err(e) => {
                warn!(error = %e, "telemetry upload failed, retrying later");
                buffer.requeue_front(events).await;
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::prelude::*;

    fn sample_event(tool: &str) -> TelemetryEvent {
        let mut ev = TelemetryEvent::now(tool, "strategy_remote");
        ev.latency_ms = 42;
        ev
    }

    #[tokio::test]
    async fn test_buffer_record_and_drain() {
        let buf = TelemetryBuffer::new(10);
        buf.record(sample_event("a")).await;
        buf.record(sample_event("b")).await;
        assert_eq!(buf.len().await, 2);

        let drained = buf.drain(1).await;
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].tool, "a");
        assert_eq!(buf.len().await, 1);
    }

    #[tokio::test]
    async fn test_buffer_drop_oldest_when_full() {
        let buf = TelemetryBuffer::new(2);
        buf.record(sample_event("a")).await;
        buf.record(sample_event("b")).await;
        buf.record(sample_event("c")).await;

        let drained = buf.drain(10).await;
        assert_eq!(drained.len(), 2);
        assert_eq!(drained[0].tool, "b");
        assert_eq!(drained[1].tool, "c");
    }

    #[tokio::test]
    async fn test_size_trigger_fires_when_threshold_reached() {
        let buf = TelemetryBuffer::new(10);
        buf.set_flush_threshold(3);
        let notify = buf.size_trigger();

        // Two events stay below the threshold — notify must not fire yet.
        buf.record(sample_event("a")).await;
        buf.record(sample_event("b")).await;
        let early = tokio::time::timeout(Duration::from_millis(50), notify.notified()).await;
        assert!(
            early.is_err(),
            "size_trigger should not fire below threshold"
        );

        // Third event lands on the threshold — notify must fire.
        buf.record(sample_event("c")).await;
        let fired = tokio::time::timeout(Duration::from_millis(100), notify.notified()).await;
        assert!(
            fired.is_ok(),
            "size_trigger must fire when queue reaches threshold"
        );
    }

    #[tokio::test]
    async fn test_buffer_requeue_front() {
        let buf = TelemetryBuffer::new(5);
        buf.record(sample_event("new")).await;
        buf.requeue_front(vec![sample_event("old1"), sample_event("old2")])
            .await;
        let drained = buf.drain(10).await;
        assert_eq!(
            drained.iter().map(|e| e.tool.as_str()).collect::<Vec<_>>(),
            vec!["old1", "old2", "new"]
        );
    }

    #[tokio::test]
    async fn test_uploader_sends_bearer_header_and_payload() {
        let server = MockServer::start_async().await;
        let mock = server
            .mock_async(|when, then| {
                when.method(POST)
                    .path("/api/telemetry/tool-invocations")
                    .header("authorization", "Bearer my-token")
                    .body_includes(r#""tool":"get_issues""#);
                then.status(202).body("");
            })
            .await;

        let uploader = TelemetryUploader::new(
            format!("{}/api/telemetry/tool-invocations", server.base_url()),
            TelemetryAuth {
                bearer_token: Some("my-token".into()),
            },
        )
        .unwrap();

        let batch = TelemetryBatch {
            events: vec![sample_event("get_issues")],
        };
        uploader.upload(&batch).await.unwrap();
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_uploader_reports_error_on_5xx() {
        let server = MockServer::start_async().await;
        server
            .mock_async(|when, then| {
                when.method(POST);
                then.status(500).body("boom");
            })
            .await;

        let uploader = TelemetryUploader::new(
            format!("{}/tele", server.base_url()),
            TelemetryAuth::default(),
        )
        .unwrap();

        let err = uploader
            .upload(&TelemetryBatch {
                events: vec![sample_event("x")],
            })
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("500"));
        assert!(msg.contains("boom"));
    }

    #[tokio::test]
    async fn test_pipeline_flush_uploads_all_and_returns_count() {
        let server = MockServer::start_async().await;
        server
            .mock_async(|when, then| {
                when.method(POST);
                then.status(200).body("");
            })
            .await;

        let cfg = ProxyTelemetryConfig {
            endpoint: Some(format!("{}/t", server.base_url())),
            ..Default::default()
        };

        let mut pipeline = TelemetryPipeline::new(cfg);
        pipeline.start(TelemetryAuth::default()).unwrap();
        pipeline.buffer().record(sample_event("a")).await;
        pipeline.buffer().record(sample_event("b")).await;

        let n = pipeline.flush().await.unwrap();
        assert_eq!(n, 2);
        pipeline.shutdown().await;
    }

    #[tokio::test]
    async fn test_pipeline_disabled_is_noop() {
        let cfg = ProxyTelemetryConfig {
            enabled: false,
            ..Default::default()
        };
        let mut pipeline = TelemetryPipeline::new(cfg);
        pipeline.start(TelemetryAuth::default()).unwrap();
        pipeline.buffer().record(sample_event("x")).await;

        // Flush is a no-op — uploader not wired.
        let n = pipeline.flush().await.unwrap();
        assert_eq!(n, 0);
        pipeline.shutdown().await;
    }

    #[tokio::test]
    async fn test_pipeline_without_endpoint_still_buffers_but_does_not_upload() {
        let cfg = ProxyTelemetryConfig {
            enabled: true,
            endpoint: None,
            ..Default::default()
        };
        let mut pipeline = TelemetryPipeline::new(cfg);
        pipeline.start(TelemetryAuth::default()).unwrap();
        pipeline.buffer().record(sample_event("x")).await;
        assert_eq!(pipeline.buffer().len().await, 1);
        pipeline.shutdown().await;
    }

    #[tokio::test]
    async fn test_flush_requeues_on_failure() {
        let server = MockServer::start_async().await;
        server
            .mock_async(|when, then| {
                when.method(POST);
                then.status(500).body("");
            })
            .await;

        let cfg = ProxyTelemetryConfig {
            endpoint: Some(format!("{}/t", server.base_url())),
            ..Default::default()
        };

        let mut pipeline = TelemetryPipeline::new(cfg);
        pipeline.start(TelemetryAuth::default()).unwrap();
        pipeline.buffer().record(sample_event("a")).await;

        assert!(pipeline.flush().await.is_err());
        assert_eq!(pipeline.buffer().len().await, 1);
        pipeline.shutdown().await;
    }
}
