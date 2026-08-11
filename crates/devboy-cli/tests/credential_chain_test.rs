//! Integration tests for credential resolution chain.
//!
//! These tests verify the credential resolution order:
//! 1. Environment variables (DEVBOY_* prefix, then unprefixed)
//! 2. Keychain (when available)
//!
//! Note: Keychain tests are skipped in CI environments where keychain is unavailable.
//!
//! Uses `temp_env` for safe env var manipulation (thread-safe, automatic cleanup).

use devboy_storage::{ChainStore, CredentialStore, EnvVarStore, MemoryStore};
use secrecy::{ExposeSecret, SecretString};

/// Compare an `Option<SecretString>` from a credential store against an
/// expected plaintext — the integration tests pre-date `SecretString` and
/// were written as `assert_eq!(result, Some("…".to_string()))`. The wrapper
/// keeps the test bodies legible without reaching into the secret elsewhere.
fn assert_secret_eq(actual: Option<SecretString>, expected: Option<&str>) {
    match (actual, expected) {
        (Some(secret), Some(want)) => {
            assert_eq!(
                secret.expose_secret(),
                want,
                "secret value did not match expected plaintext"
            );
        }
        (None, None) => {}
        (got, want) => panic!(
            "expected Option<SecretString> presence={:?}, got presence={}",
            want.is_some(),
            got.is_some()
        ),
    }
}

fn secret(s: &str) -> SecretString {
    SecretString::from(s.to_string())
}

// =============================================================================
// EnvVarStore Integration Tests
// =============================================================================

#[test]
fn test_env_var_store_prefixed_token() {
    temp_env::with_var(
        "DEVBOY_TEST_PREFIXED_MYSERVICE_TOKEN",
        Some("test_prefixed_value"),
        || {
            let store = EnvVarStore::new();
            let result = store.get("test.prefixed.myservice.token").unwrap();
            assert_secret_eq(result, Some("test_prefixed_value"));
        },
    );
}

#[test]
fn test_env_var_store_unprefixed_token() {
    temp_env::with_var(
        "TEST_UNPREFIXED_MYSERVICE_TOKEN",
        Some("test_unprefixed_value"),
        || {
            let store = EnvVarStore::new();
            let result = store.get("test.unprefixed.myservice.token").unwrap();
            assert_secret_eq(result, Some("test_unprefixed_value"));
        },
    );
}

#[test]
fn test_env_var_store_gitlab_token() {
    temp_env::with_var("DEVBOY_TEST_GITLAB_INTEG_TOKEN", Some("glpat_test"), || {
        let store = EnvVarStore::new();
        let result = store.get("test.gitlab.integ.token").unwrap();
        assert_secret_eq(result, Some("glpat_test"));
    });
}

#[test]
fn test_env_var_store_clickup_token() {
    temp_env::with_var("DEVBOY_TEST_CLICKUP_INTEG_TOKEN", Some("pk_test"), || {
        let store = EnvVarStore::new();
        let result = store.get("test.clickup.integ.token").unwrap();
        assert_secret_eq(result, Some("pk_test"));
    });
}

#[test]
fn test_env_var_store_jira_token() {
    temp_env::with_var(
        "DEVBOY_TEST_JIRA_INTEG_TOKEN",
        Some("jira_api_token"),
        || {
            let store = EnvVarStore::new();
            let result = store.get("test.jira.integ.token").unwrap();
            assert_secret_eq(result, Some("jira_api_token"));
        },
    );
}

#[test]
fn test_env_var_store_context_scoped_token() {
    temp_env::with_var(
        "DEVBOY_CONTEXTS_TESTDASHBOARD_TESTPROVIDER_TOKEN",
        Some("ghp_dashboard"),
        || {
            let store = EnvVarStore::new();
            let result = store
                .get("contexts.testdashboard.testprovider.token")
                .unwrap();
            assert_secret_eq(result, Some("ghp_dashboard"));
        },
    );
}

#[test]
fn test_env_var_store_prefixed_has_priority_over_unprefixed() {
    temp_env::with_vars(
        [
            ("DEVBOY_TEST_PRIORITY_SERVICE_TOKEN", Some("prefixed_value")),
            ("TEST_PRIORITY_SERVICE_TOKEN", Some("unprefixed_value")),
        ],
        || {
            let store = EnvVarStore::new();
            let result = store.get("test.priority.service.token").unwrap();
            assert_secret_eq(result, Some("prefixed_value"));
        },
    );
}

#[test]
fn test_env_var_store_fallback_disabled() {
    temp_env::with_var(
        "TEST_FALLBACK_DISABLED_SERVICE_TOKEN",
        Some("unprefixed_value"),
        || {
            let store = EnvVarStore::new().without_fallback();
            let result = store.get("test.fallback.disabled.service.token").unwrap();
            assert_secret_eq(result, None);
        },
    );
}

// =============================================================================
// ChainStore Integration Tests
// =============================================================================

#[test]
fn test_chain_store_env_var_priority_over_memory() {
    temp_env::with_var("DEVBOY_CHAIN_INTEG_TOKEN", Some("from_env"), || {
        let memory = MemoryStore::with_credentials([(
            "chain.integ.token".to_string(),
            "from_memory".to_string(),
        )]);
        let chain = ChainStore::new(vec![Box::new(EnvVarStore::new()), Box::new(memory)]);
        let result = chain.get("chain.integ.token").unwrap();
        assert_secret_eq(result, Some("from_env"));
    });
}

#[test]
fn test_chain_store_fallback_to_memory() {
    let memory = MemoryStore::with_credentials([(
        "chain.fallback.integ.token".to_string(),
        "from_memory".to_string(),
    )]);
    let chain = ChainStore::new(vec![Box::new(EnvVarStore::new()), Box::new(memory)]);
    let result = chain.get("chain.fallback.integ.token").unwrap();
    assert_secret_eq(result, Some("from_memory"));
}

#[test]
fn test_chain_store_not_found_in_any() {
    let chain = ChainStore::new(vec![
        Box::new(EnvVarStore::new()),
        Box::new(MemoryStore::new()),
    ]);
    let result = chain.get("nonexistent.key.integration").unwrap();
    assert_secret_eq(result, None);
}

#[test]
fn test_chain_store_write_to_memory() {
    let chain = ChainStore::new(vec![
        Box::new(EnvVarStore::new()),
        Box::new(MemoryStore::new()),
    ]);
    chain
        .store("test.write.key", &secret("test_value"))
        .unwrap();
    let result = chain.get("test.write.key").unwrap();
    assert_secret_eq(result, Some("test_value"));
}

#[test]
fn test_chain_store_delete_from_memory() {
    let memory =
        MemoryStore::with_credentials([("delete.test.key".to_string(), "value".to_string())]);
    let chain = ChainStore::new(vec![Box::new(EnvVarStore::new()), Box::new(memory)]);
    assert_secret_eq(chain.get("delete.test.key").unwrap(), Some("value"));
    chain.delete("delete.test.key").unwrap();
    assert_secret_eq(chain.get("delete.test.key").unwrap(), None);
}

#[test]
fn test_chain_store_ci_mode_reads_from_env() {
    temp_env::with_var("DEVBOY_CI_MODE_TOKEN", Some("ci_value"), || {
        let chain = ChainStore::ci_chain();
        let result = chain.get("ci.mode.token").unwrap();
        assert_secret_eq(result, Some("ci_value"));
    });
}

/// ADR-024 §6: a write in CI must fail loudly.
///
/// The CI chain used to pair the env store with `MemoryStore`, so
/// a write appeared to succeed and then vanished at process exit
/// — silent data loss, fine as a test shim and wrong as CI
/// behaviour. It now has no writable member at all.
#[test]
fn test_chain_store_ci_mode_refuses_writes() {
    let chain = ChainStore::ci_chain();

    let err = chain
        .store("ci.write.key", &secret("written"))
        .expect_err("writing in CI mode must fail rather than silently vanish");

    let message = err.to_string();
    assert!(
        message.contains("writable"),
        "error should explain that nothing in the chain accepts writes, got: {message}"
    );

    // And the value really is not readable afterwards.
    assert_secret_eq(chain.get("ci.write.key").unwrap(), None);
}

/// ADR-024 §6: the OS keychain is no longer part of the default
/// chain. If this ever flips back, the default posture changed.
#[test]
fn test_default_chain_excludes_keychain() {
    assert_eq!(
        ChainStore::default_chain().len(),
        1,
        "default chain should be environment-only after ADR-024 §6"
    );
    assert_eq!(
        ChainStore::with_keychain().len(),
        2,
        "opting in should add exactly the keychain store"
    );
}

// =============================================================================
// Default Chain Integration Tests
// =============================================================================

#[test]
fn test_default_chain_env_var_works() {
    temp_env::with_var("DEVBOY_DEFAULT_CHAIN_TEST", Some("env_value"), || {
        let chain = ChainStore::default_chain();
        let result = chain.get("default.chain.test").unwrap();
        assert_secret_eq(result, Some("env_value"));
    });
}

#[test]
fn test_default_chain_is_available() {
    let chain = ChainStore::default_chain();
    assert!(chain.is_available());
}

// =============================================================================
// Real-world Scenario Tests
// =============================================================================

#[test]
fn test_scenario_unprefixed_fallback() {
    temp_env::with_var("SCENARIO_GHA_TOKEN", Some("ghs_workflow_token"), || {
        let chain = ChainStore::default_chain();
        let result = chain.get("scenario.gha.token").unwrap();
        assert_secret_eq(result, Some("ghs_workflow_token"));
    });
}

#[test]
fn test_scenario_prefixed_priority() {
    temp_env::with_var(
        "DEVBOY_SCENARIO_DOCKER_TOKEN",
        Some("docker_secret"),
        || {
            let chain = ChainStore::default_chain();
            let result = chain.get("scenario.docker.token").unwrap();
            assert_secret_eq(result, Some("docker_secret"));
        },
    );
}

#[test]
fn test_multiple_contexts_scenario() {
    temp_env::with_vars(
        [
            ("DEVBOY_CONTEXTS_TESTPROD_TESTGH_TOKEN", Some("ghp_prod")),
            ("DEVBOY_CONTEXTS_TESTDEV_TESTGH_TOKEN", Some("ghp_dev")),
            ("DEVBOY_TESTGH_TOKEN", Some("ghp_default")),
        ],
        || {
            let chain = ChainStore::default_chain();
            assert_secret_eq(
                chain.get("contexts.testprod.testgh.token").unwrap(),
                Some("ghp_prod"),
            );
            assert_secret_eq(
                chain.get("contexts.testdev.testgh.token").unwrap(),
                Some("ghp_dev"),
            );
            assert_secret_eq(chain.get("testgh.token").unwrap(), Some("ghp_default"));
        },
    );
}

#[test]
fn test_proxy_server_token_scenario() {
    temp_env::with_var(
        "DEVBOY_TEST_PROXY_CLOUD_TOKEN",
        Some("proxy_auth_token"),
        || {
            let chain = ChainStore::default_chain();
            let result = chain.get("test-proxy-cloud.token").unwrap();
            assert_secret_eq(result, Some("proxy_auth_token"));
        },
    );
}
