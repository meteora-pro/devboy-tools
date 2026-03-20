//! Integration tests for credential resolution chain.
//!
//! These tests verify the credential resolution order:
//! 1. Environment variables (DEVBOY_* prefix, then unprefixed)
//! 2. Keychain (when available)
//!
//! Note: Keychain tests are skipped in CI environments where keychain is unavailable.

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
fn test_env_var_store_github_token_prefixed() {
    let mut guard = EnvGuard::new();
    guard.set("DEVBOY_GITHUB_TOKEN", "ghp_test_prefixed");

    let store = EnvVarStore::new();
    let result = store.get("github.token").unwrap();

    assert_eq!(result, Some("ghp_test_prefixed".to_string()));
}

#[test]
fn test_env_var_store_github_token_unprefixed() {
    let mut guard = EnvGuard::new();
    guard.set("GITHUB_TOKEN", "ghp_test_unprefixed");

    let store = EnvVarStore::new();
    let result = store.get("github.token").unwrap();

    assert_eq!(result, Some("ghp_test_unprefixed".to_string()));
}

#[test]
fn test_env_var_store_gitlab_token() {
    let mut guard = EnvGuard::new();
    guard.set("DEVBOY_GITLAB_TOKEN", "glpat_test");

    let store = EnvVarStore::new();
    let result = store.get("gitlab.token").unwrap();

    assert_eq!(result, Some("glpat_test".to_string()));
}

#[test]
fn test_env_var_store_clickup_token() {
    let mut guard = EnvGuard::new();
    guard.set("DEVBOY_CLICKUP_TOKEN", "pk_test");

    let store = EnvVarStore::new();
    let result = store.get("clickup.token").unwrap();

    assert_eq!(result, Some("pk_test".to_string()));
}

#[test]
fn test_env_var_store_jira_token() {
    let mut guard = EnvGuard::new();
    guard.set("DEVBOY_JIRA_TOKEN", "jira_api_token");

    let store = EnvVarStore::new();
    let result = store.get("jira.token").unwrap();

    assert_eq!(result, Some("jira_api_token".to_string()));
}

#[test]
fn test_env_var_store_context_scoped_token() {
    let mut guard = EnvGuard::new();
    guard.set("DEVBOY_CONTEXTS_DASHBOARD_GITHUB_TOKEN", "ghp_dashboard");

    let store = EnvVarStore::new();
    let result = store.get("contexts.dashboard.github.token").unwrap();

    assert_eq!(result, Some("ghp_dashboard".to_string()));
}

#[test]
fn test_env_var_store_prefixed_has_priority_over_unprefixed() {
    let mut guard = EnvGuard::new();
    guard.set("DEVBOY_GITHUB_TOKEN", "prefixed_value");
    guard.set("GITHUB_TOKEN", "unprefixed_value");

    let store = EnvVarStore::new();
    let result = store.get("github.token").unwrap();

    // Prefixed should win
    assert_eq!(result, Some("prefixed_value".to_string()));
}

#[test]
fn test_env_var_store_fallback_disabled() {
    let mut guard = EnvGuard::new();
    guard.set("GITHUB_TOKEN", "unprefixed_value");

    let store = EnvVarStore::new().without_fallback();
    let result = store.get("github.token").unwrap();

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

#[test]
fn test_default_chain_is_writable() {
    let chain = ChainStore::default_chain();

    // Default chain includes keychain which is writable
    // (on macOS/Windows/Linux with secret service)
    // This may fail in some CI environments, which is expected
    let writable = chain.is_writable();

    // Just log the result, don't assert since it depends on environment
    println!("Default chain is_writable: {}", writable);
}

// =============================================================================
// Real-world Scenario Tests
// =============================================================================

#[test]
fn test_github_actions_scenario() {
    // Simulate GitHub Actions environment
    let mut guard = EnvGuard::new();
    guard.set("GITHUB_TOKEN", "ghs_workflow_token");

    let chain = ChainStore::default_chain();

    // Should find GITHUB_TOKEN (unprefixed fallback)
    let result = chain.get("github.token").unwrap();
    assert_eq!(result, Some("ghs_workflow_token".to_string()));
}

#[test]
fn test_gitlab_ci_scenario() {
    // Simulate GitLab CI environment
    let mut guard = EnvGuard::new();
    guard.set("GITLAB_TOKEN", "glpat-ci-job-token");

    let chain = ChainStore::default_chain();

    // Should find GITLAB_TOKEN (unprefixed fallback)
    let result = chain.get("gitlab.token").unwrap();
    assert_eq!(result, Some("glpat-ci-job-token".to_string()));
}

#[test]
fn test_docker_environment_scenario() {
    // Simulate Docker environment with explicitly set DEVBOY_ vars
    let mut guard = EnvGuard::new();
    guard.set("DEVBOY_GITHUB_TOKEN", "ghp_docker_secret");
    guard.set("DEVBOY_GITLAB_TOKEN", "glpat_docker_secret");

    let chain = ChainStore::default_chain();

    assert_eq!(
        chain.get("github.token").unwrap(),
        Some("ghp_docker_secret".to_string())
    );
    assert_eq!(
        chain.get("gitlab.token").unwrap(),
        Some("glpat_docker_secret".to_string())
    );
}

#[test]
fn test_multiple_contexts_scenario() {
    // Simulate multiple contexts with different tokens
    let mut guard = EnvGuard::new();
    guard.set("DEVBOY_CONTEXTS_PROD_GITHUB_TOKEN", "ghp_prod");
    guard.set("DEVBOY_CONTEXTS_DEV_GITHUB_TOKEN", "ghp_dev");
    guard.set("DEVBOY_GITHUB_TOKEN", "ghp_default");

    let chain = ChainStore::default_chain();

    // Each context should get its own token
    assert_eq!(
        chain.get("contexts.prod.github.token").unwrap(),
        Some("ghp_prod".to_string())
    );
    assert_eq!(
        chain.get("contexts.dev.github.token").unwrap(),
        Some("ghp_dev".to_string())
    );
    // Default/global token
    assert_eq!(
        chain.get("github.token").unwrap(),
        Some("ghp_default".to_string())
    );
}

#[test]
fn test_proxy_server_token_scenario() {
    // Simulate proxy MCP server tokens
    let mut guard = EnvGuard::new();
    guard.set("DEVBOY_DEVBOY_CLOUD_TOKEN", "proxy_auth_token");

    let chain = ChainStore::default_chain();

    // Proxy token with custom name
    let result = chain.get("devboy-cloud.token").unwrap();
    assert_eq!(result, Some("proxy_auth_token".to_string()));
}
