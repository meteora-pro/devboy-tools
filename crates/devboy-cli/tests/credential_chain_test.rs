//! Integration tests for credential resolution chain.
//!
//! These tests verify the credential resolution order:
//! 1. Environment variables (DEVBOY_* prefix, then unprefixed)
//! 2. Keychain (when available)
//!
//! Note: Keychain tests are skipped in CI environments where keychain is unavailable.
//!
//! IMPORTANT: Each test uses unique env var names to avoid interference when
//! tests run in parallel. The pattern is `TEST_{TEST_NAME}_{PROVIDER}_TOKEN`.

use devboy_storage::{ChainStore, CredentialStore, EnvVarStore, MemoryStore};
use std::env;

/// Helper to clean up env vars after test
struct EnvGuard {
    vars: Vec<String>,
}

impl EnvGuard {
    fn new() -> Self {
        Self { vars: Vec::new() }
    }

    fn set(&mut self, key: &str, value: &str) {
        env::set_var(key, value);
        self.vars.push(key.to_string());
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for var in &self.vars {
            env::remove_var(var);
        }
    }
}

// =============================================================================
// EnvVarStore Integration Tests
// =============================================================================

#[test]
fn test_env_var_store_prefixed_token() {
    let mut guard = EnvGuard::new();
    // Use unique key to avoid parallel test interference
    guard.set(
        "DEVBOY_TEST_PREFIXED_MYSERVICE_TOKEN",
        "test_prefixed_value",
    );

    let store = EnvVarStore::new();
    let result = store.get("test.prefixed.myservice.token").unwrap();

    assert_eq!(result, Some("test_prefixed_value".to_string()));
}

#[test]
fn test_env_var_store_unprefixed_token() {
    let mut guard = EnvGuard::new();
    // Use unique key to avoid parallel test interference
    guard.set("TEST_UNPREFIXED_MYSERVICE_TOKEN", "test_unprefixed_value");

    let store = EnvVarStore::new();
    let result = store.get("test.unprefixed.myservice.token").unwrap();

    assert_eq!(result, Some("test_unprefixed_value".to_string()));
}

#[test]
fn test_env_var_store_gitlab_token() {
    let mut guard = EnvGuard::new();
    guard.set("DEVBOY_TEST_GITLAB_INTEG_TOKEN", "glpat_test");

    let store = EnvVarStore::new();
    let result = store.get("test.gitlab.integ.token").unwrap();

    assert_eq!(result, Some("glpat_test".to_string()));
}

#[test]
fn test_env_var_store_clickup_token() {
    let mut guard = EnvGuard::new();
    guard.set("DEVBOY_TEST_CLICKUP_INTEG_TOKEN", "pk_test");

    let store = EnvVarStore::new();
    let result = store.get("test.clickup.integ.token").unwrap();

    assert_eq!(result, Some("pk_test".to_string()));
}

#[test]
fn test_env_var_store_jira_token() {
    let mut guard = EnvGuard::new();
    guard.set("DEVBOY_TEST_JIRA_INTEG_TOKEN", "jira_api_token");

    let store = EnvVarStore::new();
    let result = store.get("test.jira.integ.token").unwrap();

    assert_eq!(result, Some("jira_api_token".to_string()));
}

#[test]
fn test_env_var_store_context_scoped_token() {
    let mut guard = EnvGuard::new();
    guard.set(
        "DEVBOY_CONTEXTS_TESTDASHBOARD_TESTPROVIDER_TOKEN",
        "ghp_dashboard",
    );

    let store = EnvVarStore::new();
    let result = store
        .get("contexts.testdashboard.testprovider.token")
        .unwrap();

    assert_eq!(result, Some("ghp_dashboard".to_string()));
}

#[test]
fn test_env_var_store_prefixed_has_priority_over_unprefixed() {
    let mut guard = EnvGuard::new();
    // Use unique key to avoid parallel test interference
    guard.set("DEVBOY_TEST_PRIORITY_SERVICE_TOKEN", "prefixed_value");
    guard.set("TEST_PRIORITY_SERVICE_TOKEN", "unprefixed_value");

    let store = EnvVarStore::new();
    let result = store.get("test.priority.service.token").unwrap();

    // Prefixed should win
    assert_eq!(result, Some("prefixed_value".to_string()));
}

#[test]
fn test_env_var_store_fallback_disabled() {
    let mut guard = EnvGuard::new();
    // Use unique key to avoid parallel test interference
    guard.set("TEST_FALLBACK_DISABLED_SERVICE_TOKEN", "unprefixed_value");

    let store = EnvVarStore::new().without_fallback();
    let result = store.get("test.fallback.disabled.service.token").unwrap();

    // Should NOT find unprefixed when fallback is disabled
    assert_eq!(result, None);
}

// =============================================================================
// ChainStore Integration Tests
// =============================================================================

#[test]
fn test_chain_store_env_var_priority_over_memory() {
    let mut guard = EnvGuard::new();
    guard.set("DEVBOY_CHAIN_INTEG_TOKEN", "from_env");

    // Memory store has different value for same key
    let memory = MemoryStore::with_credentials([(
        "chain.integ.token".to_string(),
        "from_memory".to_string(),
    )]);

    let chain = ChainStore::new(vec![Box::new(EnvVarStore::new()), Box::new(memory)]);

    let result = chain.get("chain.integ.token").unwrap();

    // Env var should take priority
    assert_eq!(result, Some("from_env".to_string()));
}

#[test]
fn test_chain_store_fallback_to_memory() {
    // No env var set, should fall back to memory
    let memory = MemoryStore::with_credentials([(
        "chain.fallback.integ.token".to_string(),
        "from_memory".to_string(),
    )]);

    let chain = ChainStore::new(vec![Box::new(EnvVarStore::new()), Box::new(memory)]);

    let result = chain.get("chain.fallback.integ.token").unwrap();

    assert_eq!(result, Some("from_memory".to_string()));
}

#[test]
fn test_chain_store_not_found_in_any() {
    let chain = ChainStore::new(vec![
        Box::new(EnvVarStore::new()),
        Box::new(MemoryStore::new()),
    ]);

    let result = chain.get("nonexistent.key.integration").unwrap();

    assert_eq!(result, None);
}

#[test]
fn test_chain_store_write_to_memory() {
    // Chain with env (read-only) + memory (writable)
    let chain = ChainStore::new(vec![
        Box::new(EnvVarStore::new()),
        Box::new(MemoryStore::new()),
    ]);

    // Should write to memory store
    chain.store("test.write.key", "test_value").unwrap();

    // Should be able to read it back
    let result = chain.get("test.write.key").unwrap();
    assert_eq!(result, Some("test_value".to_string()));
}

#[test]
fn test_chain_store_delete_from_memory() {
    let memory =
        MemoryStore::with_credentials([("delete.test.key".to_string(), "value".to_string())]);

    let chain = ChainStore::new(vec![Box::new(EnvVarStore::new()), Box::new(memory)]);

    // Verify it exists
    assert_eq!(
        chain.get("delete.test.key").unwrap(),
        Some("value".to_string())
    );

    // Delete
    chain.delete("delete.test.key").unwrap();

    // Should be gone
    assert_eq!(chain.get("delete.test.key").unwrap(), None);
}

#[test]
fn test_chain_store_ci_mode_no_keychain() {
    let mut guard = EnvGuard::new();
    guard.set("DEVBOY_CI_MODE_TOKEN", "ci_value");

    // CI chain: env vars + memory (no keychain)
    let chain = ChainStore::ci_chain();

    let result = chain.get("ci.mode.token").unwrap();
    assert_eq!(result, Some("ci_value".to_string()));

    // Can also write
    chain.store("ci.write.key", "written").unwrap();
    assert_eq!(
        chain.get("ci.write.key").unwrap(),
        Some("written".to_string())
    );
}

// =============================================================================
// Default Chain Integration Tests
// =============================================================================

#[test]
fn test_default_chain_env_var_works() {
    let mut guard = EnvGuard::new();
    guard.set("DEVBOY_DEFAULT_CHAIN_TEST", "env_value");

    let chain = ChainStore::default_chain();
    let result = chain.get("default.chain.test").unwrap();

    assert_eq!(result, Some("env_value".to_string()));
}

#[test]
fn test_default_chain_is_available() {
    let chain = ChainStore::default_chain();

    // Default chain should always be available (env vars are always available)
    assert!(chain.is_available());
}

// =============================================================================
// Real-world Scenario Tests (using unique env vars to avoid interference)
// =============================================================================

#[test]
fn test_scenario_unprefixed_fallback() {
    // Simulate GitHub Actions environment with unique key
    let mut guard = EnvGuard::new();
    guard.set("SCENARIO_GHA_TOKEN", "ghs_workflow_token");

    let chain = ChainStore::default_chain();

    // Should find unprefixed token (fallback)
    let result = chain.get("scenario.gha.token").unwrap();
    assert_eq!(result, Some("ghs_workflow_token".to_string()));
}

#[test]
fn test_scenario_prefixed_priority() {
    // Simulate Docker environment with explicit DEVBOY_ vars
    let mut guard = EnvGuard::new();
    guard.set("DEVBOY_SCENARIO_DOCKER_TOKEN", "docker_secret");

    let chain = ChainStore::default_chain();

    let result = chain.get("scenario.docker.token").unwrap();
    assert_eq!(result, Some("docker_secret".to_string()));
}

#[test]
fn test_multiple_contexts_scenario() {
    // Simulate multiple contexts with different tokens (using unique keys)
    let mut guard = EnvGuard::new();
    guard.set("DEVBOY_CONTEXTS_TESTPROD_TESTGH_TOKEN", "ghp_prod");
    guard.set("DEVBOY_CONTEXTS_TESTDEV_TESTGH_TOKEN", "ghp_dev");
    guard.set("DEVBOY_TESTGH_TOKEN", "ghp_default");

    let chain = ChainStore::default_chain();

    // Each context should get its own token
    assert_eq!(
        chain.get("contexts.testprod.testgh.token").unwrap(),
        Some("ghp_prod".to_string())
    );
    assert_eq!(
        chain.get("contexts.testdev.testgh.token").unwrap(),
        Some("ghp_dev".to_string())
    );
    // Default/global token
    assert_eq!(
        chain.get("testgh.token").unwrap(),
        Some("ghp_default".to_string())
    );
}

#[test]
fn test_proxy_server_token_scenario() {
    // Simulate proxy MCP server tokens (using unique key)
    let mut guard = EnvGuard::new();
    guard.set("DEVBOY_TEST_PROXY_CLOUD_TOKEN", "proxy_auth_token");

    let chain = ChainStore::default_chain();

    // Proxy token with dashes (converted to underscores)
    let result = chain.get("test-proxy-cloud.token").unwrap();
    assert_eq!(result, Some("proxy_auth_token".to_string()));
}
