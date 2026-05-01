//! OpenAI Codex CLI detector.
//!
//! Codex stores prompts cross-session in a single rolling file:
//! `~/.codex/history.jsonl` — `{"session_id":"019a…","ts":1763549853,"text":"…"}`.
//!
//! Sessions = number of unique `session_id` values.
//! last_used = max `ts` (epoch seconds).

use std::collections::HashSet;
use std::io::{BufRead, BufReader};
use std::path::Path;

use super::{AgentDetector, AgentSnapshot, InstallStatus};

const ID: &str = "codex";
const DISPLAY_NAME: &str = "Codex CLI";
const MAX_LINES: usize = 200_000;

pub struct CodexDetector;

impl AgentDetector for CodexDetector {
    fn id(&self) -> &'static str { ID }
    fn display_name(&self) -> &'static str { DISPLAY_NAME }

    fn detect(&self, home: &Path) -> AgentSnapshot {
        let codex_dir = home.join(".codex");
        let history = codex_dir.join("history.jsonl");
        let paths_checked = vec![codex_dir.clone(), history.clone()];

        let dir_present = codex_dir.is_dir();
        let binary_present = which::which("codex").is_ok();

        let status = if dir_present || binary_present { InstallStatus::Yes } else { InstallStatus::No };
        if status == InstallStatus::No {
            return empty(paths_checked);
        }

        let (sessions, last_used) = parse_history(&history);

        AgentSnapshot {
            id: ID,
            display_name: DISPLAY_NAME,
            status,
            sessions: (sessions > 0).then_some(sessions),
            last_used,
            score: 0.0,
            paths_checked,
        }
    }
}

fn parse_history(history: &Path) -> (u64, Option<chrono::DateTime<chrono::Utc>>) {
    let Ok(file) = std::fs::File::open(history) else { return (0, None); };
    let reader = BufReader::new(file);
    let mut sessions: HashSet<String> = HashSet::new();
    let mut last_ts: i64 = 0;
    for line in reader.lines().map_while(Result::ok).take(MAX_LINES) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
        if let Some(sid) = value.get("session_id").and_then(|v| v.as_str()) {
            sessions.insert(sid.to_string());
        }
        if let Some(ts) = value.get("ts").and_then(|v| v.as_i64())
            && ts > last_ts
        {
            last_ts = ts;
        }
    }
    let last_used = (last_ts > 0).then(|| chrono::DateTime::<chrono::Utc>::from_timestamp(last_ts, 0).unwrap_or_default());
    (sessions.len() as u64, last_used)
}

fn empty(paths_checked: Vec<std::path::PathBuf>) -> AgentSnapshot {
    AgentSnapshot {
        id: ID,
        display_name: DISPLAY_NAME,
        status: InstallStatus::No,
        sessions: None,
        last_used: None,
        score: 0.0,
        paths_checked,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn counts_unique_session_ids() {
        let home = tempdir().unwrap();
        let codex = home.path().join(".codex");
        fs::create_dir_all(&codex).unwrap();
        let history = codex.join("history.jsonl");
        let body = "\
{\"session_id\":\"AAA\",\"ts\":1700000000,\"text\":\"hi\"}
{\"session_id\":\"AAA\",\"ts\":1700000010,\"text\":\"again\"}
{\"session_id\":\"BBB\",\"ts\":1700000020,\"text\":\"third\"}
{\"session_id\":\"CCC\",\"ts\":1700000030,\"text\":\"fourth\"}
";
        fs::write(&history, body).unwrap();
        let snap = CodexDetector.detect(home.path());
        assert_eq!(snap.status, InstallStatus::Yes);
        assert_eq!(snap.sessions, Some(3));
        assert_eq!(snap.last_used.unwrap().timestamp(), 1700000030);
    }
}
