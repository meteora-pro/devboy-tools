//! Session traces for the self-feedback loop (ADR-015).
//!
//! A session is the execution of one skill (or any caller that opts in
//! through the `devboy trace` CLI). Events land as JSON Lines at
//! `<target>/.devboy/sessions/<YYYY-MM-DD>/<skill>/<session_id>/trace.jsonl`,
//! and a sibling `meta.json` in the same per-session directory carries
//! session-level metadata. The `<session_id>` segment keeps concurrent
//! or repeated invocations of the same skill on the same day isolated
//! from each other (ADR-015 requires self-contained sessions).
//!
//! The [`SessionTracer`] writer is intentionally small — it serialises
//! one event per line with no framing, no network I/O, and no reliance
//! on the host logging stack. The companion [`devboy trace`] CLI
//! subcommand family in `devboy-cli` lets shell-based skills write into
//! the same format.
//!
//! The redaction pass in [`redact::sanitize`] strips values that match
//! known credential shapes and values of environment variables named
//! `*_TOKEN` / `*_SECRET` / `*_KEY` / `*_PASSWORD` / `*_PASSPHRASE` /
//! `AUTHORIZATION` / `COOKIE` before anything is written to disk.

use std::fs::{File, OpenOptions, create_dir_all};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::{Result, SkillError};

pub mod redact;

// ---------------------------------------------------------------------------
// Phase + Outcome enums
// ---------------------------------------------------------------------------

/// Event phases. Kept small; readers must silently ignore unknown values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// First event of the session, marking the start of execution.
    /// The tracer synthesises a `start` record with an empty payload;
    /// any input / cwd / version metadata is caller-defined — pass it
    /// via a regular `event` call after `begin` if you want it in the
    /// stream, or record it in `meta.json` via a later `end` call.
    Start,
    /// Skill-level reasoning outcome.
    Decision,
    /// Immediately before invoking a tool.
    ToolCall,
    /// Immediately after a tool returns.
    ToolResult,
    /// A verification check ran (tests / clippy / dry-run etc.).
    Verify,
    /// Non-trivial file produced by the skill.
    Artifact,
    /// Free-form human-readable log entry.
    Note,
    /// Last event of the session.
    End,
}

/// Session outcome — recorded on `End` in both the event stream and the
/// per-session `meta.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Skill completed and achieved its objective.
    Success,
    /// Skill ran to completion but the objective was not met.
    Failure,
    /// Skill stopped before completing (cancelled, interrupted).
    Aborted,
}

// ---------------------------------------------------------------------------
// Target resolution
// ---------------------------------------------------------------------------

/// Where to write session traces.
#[derive(Debug, Clone)]
pub enum TraceTarget {
    /// Repo-local at `<repo>/.devboy/sessions/`. Default.
    RepoLocal,
    /// Per-user at `~/.devboy/sessions/`.
    Global,
    /// Explicit directory — used by tests.
    Custom(PathBuf),
}

impl TraceTarget {
    /// Resolve the `sessions/` root directory for this target.
    pub fn sessions_root(&self) -> Result<PathBuf> {
        match self {
            Self::RepoLocal => {
                let cwd = std::env::current_dir().map_err(|source| SkillError::Io {
                    path: PathBuf::from("."),
                    source,
                })?;
                let repo = locate_repo_root(&cwd).ok_or_else(|| SkillError::Io {
                    path: cwd,
                    source: std::io::Error::other(
                        "no git repository / .devboy.toml at the current path (pass --global to write traces under ~/.devboy/sessions/)",
                    ),
                })?;
                Ok(repo.join(".devboy").join("sessions"))
            }
            Self::Global => {
                let home = home_dir()?;
                Ok(home.join(".devboy").join("sessions"))
            }
            Self::Custom(p) => Ok(p.clone()),
        }
    }
}

fn locate_repo_root(start: &Path) -> Option<PathBuf> {
    let mut cur = start;
    loop {
        if cur.join(".git").exists() || cur.join(".devboy.toml").exists() {
            return Some(cur.to_path_buf());
        }
        match cur.parent() {
            Some(p) => cur = p,
            None => return None,
        }
    }
}

fn home_dir() -> Result<PathBuf> {
    if let Some(p) = std::env::var_os("DEVBOY_HOME_OVERRIDE")
        && !p.is_empty()
    {
        return Ok(PathBuf::from(p));
    }
    dirs::home_dir().ok_or_else(|| SkillError::Io {
        path: PathBuf::from("~"),
        source: std::io::Error::other("home directory is not set"),
    })
}

// ---------------------------------------------------------------------------
// SessionTracer
// ---------------------------------------------------------------------------

/// Appends events to a session's `trace.jsonl`.
///
/// A new instance is built via [`SessionTracer::begin`], events are
/// appended with [`SessionTracer::event`], and the session is finalised
/// with [`SessionTracer::end`]. Dropping a tracer without calling `end`
/// leaves the `trace.jsonl` intact but does not write `meta.json` — the
/// stream still round-trips, just without the convenience summary.
pub struct SessionTracer {
    session_id: String,
    skill: String,
    session_dir: PathBuf,
    trace_path: PathBuf,
    meta_path: PathBuf,
    trace_file: Mutex<File>,
    started_at: DateTime<Utc>,
    tool_calls: std::sync::atomic::AtomicU64,
    errors: std::sync::atomic::AtomicU64,
    /// Env-var snapshot captured at `begin`, reused for every
    /// `write_event` so the redactor does not rescan `std::env::vars()`
    /// on each record (can be tens of thousands of events in a long
    /// session).
    redactor: redact::Redactor,
}

impl SessionTracer {
    /// Start a new session. Creates a unique directory
    /// `<root>/<date>/<skill>/<session_id>/` and opens `trace.jsonl`
    /// for append. The per-session directory ensures that concurrent
    /// or repeated invocations of the same skill on the same day do
    /// not share any files (ADR-015 requires self-contained sessions).
    pub fn begin(skill: &str, target: &TraceTarget) -> Result<Self> {
        let root = target.sessions_root()?;
        let started_at = Utc::now();
        let date = started_at.format("%Y-%m-%d").to_string();
        let session_id = new_session_id();
        let session_dir = root.join(&date).join(skill).join(&session_id);
        create_dir_all(&session_dir).map_err(|source| SkillError::Io {
            path: session_dir.clone(),
            source,
        })?;

        let trace_path = session_dir.join("trace.jsonl");
        let meta_path = session_dir.join("meta.json");
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&trace_path)
            .map_err(|source| SkillError::Io {
                path: trace_path.clone(),
                source,
            })?;

        let tracer = Self {
            session_id,
            skill: skill.to_string(),
            session_dir,
            trace_path,
            meta_path,
            trace_file: Mutex::new(file),
            started_at,
            tool_calls: std::sync::atomic::AtomicU64::new(0),
            errors: std::sync::atomic::AtomicU64::new(0),
            redactor: redact::Redactor::snapshot(),
        };

        // Emit the `start` event synthesised from the call arguments;
        // callers can decorate it further via a regular `event` call.
        tracer.write_event(Phase::Start, json!({}))?;
        Ok(tracer)
    }

    /// Append one event to the trace. Payload is redacted before
    /// writing.
    pub fn event(&self, phase: Phase, payload: Value) -> Result<()> {
        if phase == Phase::ToolCall {
            self.tool_calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        if phase == Phase::ToolResult
            && payload
                .get("ok")
                .and_then(|v| v.as_bool())
                .is_some_and(|ok| !ok)
        {
            self.errors
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        self.write_event(phase, payload)
    }

    /// Record the final `end` event and write the per-session
    /// `meta.json`. Consumes the tracer.
    pub fn end(self, outcome: Outcome, summary: &str) -> Result<()> {
        let ended_at = Utc::now();
        self.write_event(
            Phase::End,
            json!({ "outcome": outcome, "summary": summary }),
        )?;

        let meta = SessionMeta {
            session_id: self.session_id.clone(),
            skill: self.skill.clone(),
            skill_version: None,
            devboy_version: env!("CARGO_PKG_VERSION").to_string(),
            started_at: self.started_at,
            ended_at: Some(ended_at),
            outcome: Some(outcome),
            input_summary: None,
            tool_calls: self.tool_calls.load(std::sync::atomic::Ordering::Relaxed),
            errors: self.errors.load(std::sync::atomic::Ordering::Relaxed),
            summary: Some(summary.to_string()),
        };
        let bytes = serde_json::to_vec_pretty(&meta).map_err(|source| SkillError::SerdeJson {
            operation: "serialise session meta",
            path: self.meta_path.clone(),
            source,
        })?;
        std::fs::write(&self.meta_path, bytes).map_err(|source| SkillError::Io {
            path: self.meta_path.clone(),
            source,
        })
    }

    /// The session directory on disk.
    pub fn session_dir(&self) -> &Path {
        &self.session_dir
    }

    /// The `trace.jsonl` path.
    pub fn trace_path(&self) -> &Path {
        &self.trace_path
    }

    /// The session id (ULID, or a randomly-generated fallback when the
    /// `trace` Cargo feature is disabled).
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    fn write_event(&self, phase: Phase, payload: Value) -> Result<()> {
        let redacted = self.redactor.sanitize(payload);
        let record = TraceRecord {
            ts: Utc::now(),
            skill: self.skill.clone(),
            session_id: self.session_id.clone(),
            phase,
            payload: redacted,
        };
        let line = serde_json::to_string(&record).map_err(|source| SkillError::SerdeJson {
            operation: "serialise trace record",
            path: self.trace_path.clone(),
            source,
        })?;

        let mut guard = self.trace_file.lock().map_err(|_| SkillError::Io {
            path: self.trace_path.clone(),
            source: std::io::Error::other("trace mutex poisoned"),
        })?;
        guard
            .write_all(line.as_bytes())
            .and_then(|()| guard.write_all(b"\n"))
            .map_err(|source| SkillError::Io {
                path: self.trace_path.clone(),
                source,
            })
    }
}

// ---------------------------------------------------------------------------
// On-disk formats
// ---------------------------------------------------------------------------

/// One line of `trace.jsonl`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceRecord {
    /// Timestamp of the event.
    pub ts: DateTime<Utc>,
    /// Skill name — duplicated on every event so that readers that
    /// merge traces from different sessions can still attribute each
    /// line.
    pub skill: String,
    /// Session id (ULID).
    pub session_id: String,
    /// Event phase.
    pub phase: Phase,
    /// Event payload. Redacted before writing.
    pub payload: Value,
}

/// `meta.json` — written at session end and optionally touched during
/// long sessions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMeta {
    /// Session id matching every record in `trace.jsonl`.
    pub session_id: String,
    pub skill: String,
    /// Skill version at run time, if the caller provided it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skill_version: Option<u32>,
    /// devboy-tools version that emitted the session.
    pub devboy_version: String,
    /// Session start timestamp.
    pub started_at: DateTime<Utc>,
    /// Session end timestamp (missing on in-flight sessions).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<DateTime<Utc>>,
    /// Outcome, if the session ended cleanly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<Outcome>,
    /// One-line input summary recorded at start, if available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_summary: Option<String>,
    /// Number of tool calls observed.
    pub tool_calls: u64,
    /// Number of tool calls that reported `ok: false`.
    pub errors: u64,
    /// Closing summary string (mirrors the `end` event payload).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

// ---------------------------------------------------------------------------
// Stateless helpers (used by `devboy trace` CLI subcommands)
// ---------------------------------------------------------------------------

/// Create the session directory, write the opening `start` event, and
/// return the session id + session directory. The returned path is the
/// value the user passed through `TraceTarget` with the `<date>/<skill>
/// /<session_id>` suffix appended — it is not canonicalised, so a
/// relative `TraceTarget::Custom(".traces")` produces a relative path
/// and an absolute target produces an absolute path. Callers pass both
/// values back into [`append_event`] and [`finalise_session`] on
/// subsequent CLI invocations.
///
/// The per-session directory level is required so that concurrent or
/// repeated invocations of the same skill on the same day never share
/// `trace.jsonl` or `meta.json` (ADR-015).
pub fn create_session(skill: &str, target: &TraceTarget) -> Result<(String, PathBuf)> {
    let root = target.sessions_root()?;
    let started_at = Utc::now();
    let date = started_at.format("%Y-%m-%d").to_string();
    let session_id = new_session_id();
    let session_dir = root.join(&date).join(skill).join(&session_id);
    create_dir_all(&session_dir).map_err(|source| SkillError::Io {
        path: session_dir.clone(),
        source,
    })?;
    append_event_inner(
        &session_dir,
        &session_id,
        skill,
        Phase::Start,
        Value::Object(Default::default()),
    )?;
    // Write a skeletal meta.json so long-running sessions are
    // introspectable before `end` runs.
    let skeleton = SessionMeta {
        session_id: session_id.clone(),
        skill: skill.to_string(),
        skill_version: None,
        devboy_version: env!("CARGO_PKG_VERSION").to_string(),
        started_at,
        ended_at: None,
        outcome: None,
        input_summary: None,
        tool_calls: 0,
        errors: 0,
        summary: None,
    };
    write_meta_file(&session_dir.join("meta.json"), &skeleton)?;
    Ok((session_id, session_dir))
}

/// Append one event to an existing session's `trace.jsonl`.
pub fn append_event(
    session_dir: &Path,
    session_id: &str,
    skill: &str,
    phase: Phase,
    payload: Value,
) -> Result<()> {
    append_event_inner(session_dir, session_id, skill, phase, payload)
}

fn append_event_inner(
    session_dir: &Path,
    session_id: &str,
    skill: &str,
    phase: Phase,
    payload: Value,
) -> Result<()> {
    let redacted = redact::sanitize(payload);
    let record = TraceRecord {
        ts: Utc::now(),
        skill: skill.to_string(),
        session_id: session_id.to_string(),
        phase,
        payload: redacted,
    };
    let line = serde_json::to_string(&record).map_err(|source| SkillError::SerdeJson {
        operation: "serialise trace record",
        path: session_dir.join("trace.jsonl"),
        source,
    })?;

    let trace_path = session_dir.join("trace.jsonl");
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&trace_path)
        .map_err(|source| SkillError::Io {
            path: trace_path.clone(),
            source,
        })?;
    file.write_all(line.as_bytes())
        .and_then(|()| file.write_all(b"\n"))
        .map_err(|source| SkillError::Io {
            path: trace_path.clone(),
            source,
        })
}

/// Write the final `end` event and refresh `meta.json` with the
/// outcome + aggregated counts derived from the existing trace.
pub fn finalise_session(
    session_dir: &Path,
    session_id: &str,
    skill: &str,
    outcome: Outcome,
    summary: &str,
) -> Result<()> {
    append_event_inner(
        session_dir,
        session_id,
        skill,
        Phase::End,
        json!({ "outcome": outcome, "summary": summary }),
    )?;

    let trace_path = session_dir.join("trace.jsonl");
    let (tool_calls, errors, started_at) = scan_counts(&trace_path)?;

    let meta = SessionMeta {
        session_id: session_id.to_string(),
        skill: skill.to_string(),
        skill_version: None,
        devboy_version: env!("CARGO_PKG_VERSION").to_string(),
        started_at,
        ended_at: Some(Utc::now()),
        outcome: Some(outcome),
        input_summary: None,
        tool_calls,
        errors,
        summary: Some(summary.to_string()),
    };
    write_meta_file(&session_dir.join("meta.json"), &meta)
}

fn write_meta_file(path: &Path, meta: &SessionMeta) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(meta).map_err(|source| SkillError::SerdeJson {
        operation: "serialise session meta",
        path: path.to_path_buf(),
        source,
    })?;
    std::fs::write(path, bytes).map_err(|source| SkillError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn scan_counts(trace_path: &Path) -> Result<(u64, u64, DateTime<Utc>)> {
    use std::io::{BufRead, BufReader};

    // Stream the file line-by-line rather than reading the whole
    // `trace.jsonl` into memory. ADR-015 explicitly flags that long
    // sessions can produce very large traces; keeping the memory
    // footprint bounded matters.
    let file = std::fs::File::open(trace_path).map_err(|source| SkillError::Io {
        path: trace_path.to_path_buf(),
        source,
    })?;
    let reader = BufReader::new(file);

    let mut tool_calls = 0u64;
    let mut errors = 0u64;
    let mut started_at: Option<DateTime<Utc>> = None;
    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(source) => {
                return Err(SkillError::Io {
                    path: trace_path.to_path_buf(),
                    source,
                });
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let record: TraceRecord = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(_) => continue,
        };
        if started_at.is_none() && record.phase == Phase::Start {
            started_at = Some(record.ts);
        }
        if record.phase == Phase::ToolCall {
            tool_calls += 1;
        }
        if record.phase == Phase::ToolResult
            && record
                .payload
                .get("ok")
                .and_then(|v| v.as_bool())
                .is_some_and(|ok| !ok)
        {
            errors += 1;
        }
    }
    Ok((tool_calls, errors, started_at.unwrap_or_else(Utc::now)))
}

// ---------------------------------------------------------------------------
// Session id
// ---------------------------------------------------------------------------

#[cfg(feature = "trace")]
fn new_session_id() -> String {
    ulid::Ulid::new().to_string()
}

#[cfg(not(feature = "trace"))]
fn new_session_id() -> String {
    // Deterministic-enough fallback when the `trace` feature is off —
    // time-prefixed so logs stay sortable.
    format!(
        "{}-{:x}",
        Utc::now().format("%Y%m%d%H%M%S%f"),
        std::process::id()
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn events_in(path: &Path) -> Vec<TraceRecord> {
        let text = std::fs::read_to_string(path).unwrap();
        text.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[test]
    fn session_round_trip() {
        let dir = tempdir().unwrap();
        let target = TraceTarget::Custom(dir.path().to_path_buf());

        let tracer = SessionTracer::begin("setup", &target).unwrap();
        let trace_path = tracer.trace_path().to_path_buf();
        let meta_path = tracer.session_dir().join("meta.json");

        tracer
            .event(
                Phase::Decision,
                json!({ "question": "provider?", "decision": "github" }),
            )
            .unwrap();
        tracer
            .event(
                Phase::ToolCall,
                json!({ "tool": "get_issues", "args": { "limit": 3 } }),
            )
            .unwrap();
        tracer
            .event(
                Phase::ToolResult,
                json!({ "tool": "get_issues", "ok": true, "duration_ms": 42 }),
            )
            .unwrap();
        tracer.end(Outcome::Success, "configured github").unwrap();

        let events = events_in(&trace_path);
        // start + decision + tool_call + tool_result + end = 5
        assert_eq!(events.len(), 5);
        assert_eq!(events[0].phase, Phase::Start);
        assert_eq!(events.last().unwrap().phase, Phase::End);
        assert!(events.iter().all(|e| e.skill == "setup"));
        assert!(events.iter().all(|e| e.session_id == events[0].session_id));

        let meta_bytes = std::fs::read(&meta_path).unwrap();
        let meta: SessionMeta = serde_json::from_slice(&meta_bytes).unwrap();
        assert_eq!(meta.skill, "setup");
        assert_eq!(meta.outcome, Some(Outcome::Success));
        assert_eq!(meta.tool_calls, 1);
        assert_eq!(meta.errors, 0);
    }

    #[test]
    fn failed_tool_result_is_counted_as_error() {
        let dir = tempdir().unwrap();
        let target = TraceTarget::Custom(dir.path().to_path_buf());
        let tracer = SessionTracer::begin("devboy-test", &target).unwrap();
        tracer
            .event(Phase::ToolCall, json!({ "tool": "get_issues" }))
            .unwrap();
        tracer
            .event(
                Phase::ToolResult,
                json!({ "tool": "get_issues", "ok": false, "error": "401 Unauthorized" }),
            )
            .unwrap();
        let meta_path = tracer.session_dir().join("meta.json");
        tracer.end(Outcome::Failure, "401").unwrap();

        let meta: SessionMeta = serde_json::from_slice(&std::fs::read(meta_path).unwrap()).unwrap();
        assert_eq!(meta.tool_calls, 1);
        assert_eq!(meta.errors, 1);
        assert_eq!(meta.outcome, Some(Outcome::Failure));
    }

    #[test]
    fn events_are_redacted_before_writing() {
        // Share the env-serialisation lock with `redact::tests`; without
        // it a concurrent `DEVBOY_TRACE_REDACTION=off` in a sibling test
        // can disable redaction for the window this test runs and let
        // the raw token reach disk (observed as a hard failure on
        // ubuntu-24.04-arm in CI). Strictly stronger than a bare
        // `temp_env::with_var` reset because the mutex serialises against
        // every redact-tests sibling that legitimately toggles the var.
        super::redact::test_support::with_clean_env(|| {
            let dir = tempdir().unwrap();
            let target = TraceTarget::Custom(dir.path().to_path_buf());
            let tracer = SessionTracer::begin("devboy-test", &target).unwrap();
            let trace_path = tracer.trace_path().to_path_buf();
            tracer
                .event(
                    Phase::ToolCall,
                    json!({
                        "tool": "create_issue",
                        "args": { "token": "ghp_012345678901234567890123456789012345" }
                    }),
                )
                .unwrap();
            tracer.end(Outcome::Success, "").unwrap();

            let text = std::fs::read_to_string(&trace_path).unwrap();
            assert!(
                !text.contains("ghp_0123456789"),
                "trace contained raw GitHub token: {text}"
            );
            assert!(
                text.contains("<redacted"),
                "trace did not include redaction marker: {text}"
            );
        });
    }

    #[test]
    fn global_target_respects_home_override() {
        let home = tempdir().unwrap();
        let home_path = home.path().to_path_buf();
        temp_env::with_var("DEVBOY_HOME_OVERRIDE", Some(home.path()), || {
            let root = TraceTarget::Global.sessions_root().unwrap();
            assert!(root.starts_with(&home_path));
        });
    }

    #[test]
    fn custom_target_writes_exactly_where_asked() {
        let dir = tempdir().unwrap();
        let target = TraceTarget::Custom(dir.path().to_path_buf());
        let tracer = SessionTracer::begin("x", &target).unwrap();
        let trace_path = tracer.trace_path().to_path_buf();
        assert!(trace_path.starts_with(dir.path()));
        tracer.end(Outcome::Success, "").unwrap();
    }
}
