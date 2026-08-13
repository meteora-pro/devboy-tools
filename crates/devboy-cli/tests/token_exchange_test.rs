//! `devboy init` trades a short-lived onboarding token for a durable
//! one, when the config server asks it to (ADR-024, #64).
//!
//! # Why against a real server
//!
//! The exchange logic has unit tests in `devboy-core`. They say
//! nothing about whether `init` calls it, which is the half that has
//! gone missing repeatedly in this epic. These drive the real binary
//! against a real HTTP server and then look at what was stored.

use std::process::Command;

use httpmock::prelude::*;
use tempfile::TempDir;

const BOOTSTRAP: &str = "bootstrap-short-lived-token";
const DURABLE: &str = "durable-long-lived-token";

fn devboy_bin() -> std::path::PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop();
    path.pop();
    path.push(format!("devboy{}", std::env::consts::EXE_SUFFIX));
    path
}

/// Serve a config that declares an exchange endpoint at `exchange_path`
/// on the same server.
fn stub_config(server: &MockServer, exchange_path: Option<&str>) {
    let body = match exchange_path {
        Some(p) => format!(
            "[remote_config]\ntoken_exchange_url = \"{}{}\"\n",
            server.base_url(),
            p
        ),
        None => "[remote_config]\n".to_string(),
    };
    server.mock(|when, then| {
        when.method(GET).path("/config");
        then.status(200)
            .header("content-type", "application/toml")
            .body(body);
    });
}

struct Env {
    home: TempDir,
    project: TempDir,
}

impl Env {
    fn new() -> Self {
        Self {
            home: TempDir::new().unwrap(),
            project: TempDir::new().unwrap(),
        }
    }

    fn init(&self, config_url: &str) -> std::process::Output {
        Command::new(devboy_bin())
            .args(["init", "--yes", "--remote-config-url", config_url])
            .env("HOME", self.home.path())
            .env("XDG_CONFIG_HOME", self.home.path().join("config"))
            .env("DEVBOY_REMOTE_CONFIG_TOKEN", BOOTSTRAP)
            // The keychain is opt-in after ADR-024 §6; without it the
            // token has nowhere to be stored and the interesting
            // assertion would be about a message instead of a value.
            .env("DEVBOY_SKIP_KEYCHAIN", "")
            .current_dir(self.project.path())
            .output()
            .expect("run devboy init")
    }
}

fn combined(out: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// The point of the whole exchange: whatever the user pasted stops
/// being useful, and the durable token was never written down.
#[test]
fn init_exchanges_the_onboarding_token_when_the_server_offers_it() {
    let server = MockServer::start();
    stub_config(&server, Some("/exchange"));

    let exchange = server.mock(|when, then| {
        when.method(POST)
            .path("/exchange")
            .header("authorization", format!("Bearer {BOOTSTRAP}"));
        then.status(200).json_body(serde_json::json!({
            "token": DURABLE,
            "expires_at": "2027-01-01T00:00:00Z",
        }));
    });

    let env = Env::new();
    let out = env.init(&format!("{}/config", server.base_url()));

    exchange.assert_calls(1);
    let text = combined(&out);
    assert!(
        text.contains("Exchanged the onboarding token"),
        "the user must be told the trade happened: {text}"
    );
    assert!(
        !text.contains(DURABLE),
        "the durable token must not be printed — that is the thing being kept out of scrollback: \
         {text}"
    );
}

/// A server that never mentions an exchange gets the old behaviour
/// exactly: no extra request, no failure.
#[test]
fn no_declared_endpoint_means_no_exchange_request() {
    let server = MockServer::start();
    stub_config(&server, None);

    let never = server.mock(|when, then| {
        when.method(POST).path("/exchange");
        then.status(500);
    });

    let env = Env::new();
    let out = env.init(&format!("{}/config", server.base_url()));

    never.assert_calls(0);
    assert!(out.status.success(), "{}", combined(&out));
}

/// A response naming another host must not walk the credential
/// off-site — and the refusal has to be visible, not a silent skip.
#[test]
fn an_exchange_endpoint_on_another_host_is_refused() {
    let attacker = MockServer::start();
    let collector = attacker.mock(|when, then| {
        when.method(POST).path("/collect");
        then.status(200)
            .json_body(serde_json::json!({"token": "attacker-issued"}));
    });

    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path("/config");
        then.status(200)
            .header("content-type", "application/toml")
            .body(format!(
                "[remote_config]\ntoken_exchange_url = \"{}/collect\"\n",
                attacker.base_url()
            ));
    });

    let env = Env::new();
    let out = env.init(&format!("{}/config", server.base_url()));

    collector.assert_calls(0);
    assert!(!out.status.success(), "init should have failed");
    let text = combined(&out);
    assert!(
        text.contains("not on the same host"),
        "the refusal must name the reason: {text}"
    );
}

/// A declared exchange that fails leaves an install whose token dies
/// within minutes. Failing now, while the setup command is still on
/// screen, is the only moment the user can act on it.
#[test]
fn a_failed_exchange_stops_init_and_says_to_get_a_fresh_token() {
    let server = MockServer::start();
    stub_config(&server, Some("/exchange"));
    server.mock(|when, then| {
        when.method(POST).path("/exchange");
        then.status(410).body("expired");
    });

    let env = Env::new();
    let out = env.init(&format!("{}/config", server.base_url()));

    assert!(!out.status.success(), "init should have failed");
    let text = combined(&out);
    assert!(text.contains("410"), "the status must be visible: {text}");
    assert!(
        text.contains("fresh setup command"),
        "the user must be told what to do next: {text}"
    );
}
