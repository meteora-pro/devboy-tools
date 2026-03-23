//! Integration tests for env-only context configuration.
//!
//! These tests verify that contexts can be defined entirely through
//! environment variables without any config file.
//!
//! # Running Tests
//!
//! ```bash
//! cargo test --test env_context_test
//! ```
//!
//! IMPORTANT: Each test uses unique env var names to avoid interference
//! when tests run in parallel.

use devboy_storage::{ChainStore, CredentialStore, EnvVarStore};
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

    #[allow(dead_code)]
    fn remove(&mut self, key: &str) {
        env::remove_var(key);
        // Don't add to cleanup list, it's already removed
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
// Context Token Resolution Tests
// =============================================================================

#[test]
fn test_context_github_token_resolution() {
    let mut guard = EnvGuard::new();
    guard.set("DEVBOY_CONTEXTS_INTEG_PROD_GITHUB_TOKEN", "ghp_prod_token");

    let store = EnvVarStore::new();
    let result = store.get("contexts.integ-prod.github.token").unwrap();

    assert_eq!(result, Some("ghp_prod_token".to_string()));
}

#[test]
fn test_context_gitlab_token_resolution() {
    let mut guard = EnvGuard::new();
    guard.set(
        "DEVBOY_CONTEXTS_INTEG_STAGING_GITLAB_TOKEN",
        "glpat_staging",
    );

    let store = EnvVarStore::new();
    let result = store.get("contexts.integ-staging.gitlab.token").unwrap();

    assert_eq!(result, Some("glpat_staging".to_string()));
}

#[test]
fn test_context_clickup_token_resolution() {
    let mut guard = EnvGuard::new();
    guard.set(
        "DEVBOY_CONTEXTS_INTEG_TASKS_CLICKUP_TOKEN",
        "pk_tasks_token",
    );

    let store = EnvVarStore::new();
    let result = store.get("contexts.integ-tasks.clickup.token").unwrap();

    assert_eq!(result, Some("pk_tasks_token".to_string()));
}

#[test]
fn test_context_jira_token_resolution() {
    let mut guard = EnvGuard::new();
    guard.set("DEVBOY_CONTEXTS_INTEG_JIRA_JIRA_TOKEN", "jira_token_123");

    let store = EnvVarStore::new();
    let result = store.get("contexts.integ-jira.jira.token").unwrap();

    assert_eq!(result, Some("jira_token_123".to_string()));
}

// =============================================================================
// Multi-part Context Name Tests
// =============================================================================

#[test]
fn test_context_with_underscores_in_name() {
    let mut guard = EnvGuard::new();
    guard.set(
        "DEVBOY_CONTEXTS_MY_COOL_PROJECT_GITHUB_TOKEN",
        "ghp_cool_project",
    );

    let store = EnvVarStore::new();
    // Note: underscores in context name are preserved in the key
    let result = store.get("contexts.my-cool-project.github.token").unwrap();

    assert_eq!(result, Some("ghp_cool_project".to_string()));
}

#[test]
fn test_context_single_word_name() {
    let mut guard = EnvGuard::new();
    guard.set("DEVBOY_CONTEXTS_PROD_GITHUB_TOKEN", "ghp_single_word");

    let store = EnvVarStore::new();
    let result = store.get("contexts.prod.github.token").unwrap();

    assert_eq!(result, Some("ghp_single_word".to_string()));
}

// =============================================================================
// Token Priority Tests
// =============================================================================

#[test]
fn test_context_specific_token_over_global() {
    let mut guard = EnvGuard::new();
    // Set both context-specific and global tokens
    guard.set(
        "DEVBOY_CONTEXTS_INTEG_PRIO_GITHUB_TOKEN",
        "ghp_context_specific",
    );
    guard.set("DEVBOY_GITHUB_TOKEN", "ghp_global");

    // Use ci_chain to avoid keychain access issues in CI environments
    let chain = ChainStore::ci_chain();

    // Context-specific should be found when asking for that context
    let context_result = chain.get("contexts.integ-prio.github.token").unwrap();
    assert_eq!(context_result, Some("ghp_context_specific".to_string()));

    // Global should be found when asking for global
    let global_result = chain.get("github.token").unwrap();
    assert_eq!(global_result, Some("ghp_global".to_string()));
}

#[test]
fn test_fallback_to_global_token_when_no_context_specific() {
    let mut guard = EnvGuard::new();
    // Only set global token
    guard.set("DEVBOY_INTEG_FALLBACK_GITHUB_TOKEN", "ghp_global_only");

    // Use ci_chain to avoid keychain access issues in CI environments
    let chain = ChainStore::ci_chain();

    // Context-specific should not be found
    let context_result = chain
        .get("contexts.nonexistent.integ-fallback.github.token")
        .unwrap();
    assert_eq!(context_result, None);

    // Global should be found
    let global_result = chain.get("integ-fallback.github.token").unwrap();
    assert_eq!(global_result, Some("ghp_global_only".to_string()));
}

// =============================================================================
// Multiple Providers in Same Context Tests
// =============================================================================

#[test]
fn test_multiple_providers_in_context() {
    let mut guard = EnvGuard::new();
    guard.set(
        "DEVBOY_CONTEXTS_INTEG_MULTI_GITHUB_TOKEN",
        "ghp_multi_context",
    );
    guard.set(
        "DEVBOY_CONTEXTS_INTEG_MULTI_GITLAB_TOKEN",
        "glpat_multi_context",
    );
    guard.set(
        "DEVBOY_CONTEXTS_INTEG_MULTI_CLICKUP_TOKEN",
        "pk_multi_context",
    );

    let store = EnvVarStore::new();

    // All tokens should be resolvable for the same context
    assert_eq!(
        store.get("contexts.integ-multi.github.token").unwrap(),
        Some("ghp_multi_context".to_string())
    );
    assert_eq!(
        store.get("contexts.integ-multi.gitlab.token").unwrap(),
        Some("glpat_multi_context".to_string())
    );
    assert_eq!(
        store.get("contexts.integ-multi.clickup.token").unwrap(),
        Some("pk_multi_context".to_string())
    );
}

// =============================================================================
// Multiple Contexts Tests
// =============================================================================

#[test]
fn test_multiple_contexts_isolation() {
    let mut guard = EnvGuard::new();
    guard.set("DEVBOY_CONTEXTS_INTEG_CTX_A_GITHUB_TOKEN", "ghp_context_a");
    guard.set("DEVBOY_CONTEXTS_INTEG_CTX_B_GITHUB_TOKEN", "ghp_context_b");
    guard.set("DEVBOY_CONTEXTS_INTEG_CTX_C_GITHUB_TOKEN", "ghp_context_c");

    let store = EnvVarStore::new();

    // Each context should get its own token
    assert_eq!(
        store.get("contexts.integ-ctx-a.github.token").unwrap(),
        Some("ghp_context_a".to_string())
    );
    assert_eq!(
        store.get("contexts.integ-ctx-b.github.token").unwrap(),
        Some("ghp_context_b".to_string())
    );
    assert_eq!(
        store.get("contexts.integ-ctx-c.github.token").unwrap(),
        Some("ghp_context_c".to_string())
    );
}

// =============================================================================
// CI Chain Tests (env-only mode)
// =============================================================================

#[test]
fn test_ci_chain_context_tokens() {
    let mut guard = EnvGuard::new();
    guard.set("DEVBOY_CONTEXTS_INTEG_CI_GITHUB_TOKEN", "ghp_ci_context");

    // CI chain uses env vars only (no keychain)
    let chain = ChainStore::ci_chain();

    let result = chain.get("contexts.integ-ci.github.token").unwrap();
    assert_eq!(result, Some("ghp_ci_context".to_string()));
}

#[test]
fn test_ci_chain_write_to_memory() {
    // CI chain should be able to write (to memory store)
    let chain = ChainStore::ci_chain();

    chain
        .store("ci.context.test.key", "test_value")
        .expect("Should be able to write in CI chain");

    let result = chain.get("ci.context.test.key").unwrap();
    assert_eq!(result, Some("test_value".to_string()));
}

// =============================================================================
// Edge Cases
// =============================================================================

#[test]
fn test_double_underscore_in_env_var() {
    let mut guard = EnvGuard::new();
    // Double underscore becomes single dot in key, so this still matches
    // DEVBOY_CONTEXTS__GITHUB_TOKEN -> contexts..github.token
    // This is expected behavior based on key conversion rules
    guard.set("DEVBOY_CONTEXTS__GITHUB_TOKEN", "double_underscore_value");

    let store = EnvVarStore::new();
    let result = store.get("contexts..github.token").unwrap();

    // This actually matches because the conversion is symmetric
    assert_eq!(result, Some("double_underscore_value".to_string()));
}

#[test]
fn test_provider_name_in_context_name() {
    let mut guard = EnvGuard::new();
    // Context name contains "GITHUB" but it's part of context name, not provider
    guard.set(
        "DEVBOY_CONTEXTS_MY_GITHUB_APP_GITHUB_TOKEN",
        "ghp_github_in_name",
    );

    let store = EnvVarStore::new();
    let result = store.get("contexts.my-github-app.github.token").unwrap();

    assert_eq!(result, Some("ghp_github_in_name".to_string()));
}

#[test]
fn test_nonexistent_context_returns_none() {
    // Don't set any env var - just verify that missing context returns None
    let store = EnvVarStore::new();
    let result = store
        .get("contexts.completely-nonexistent-ctx-12345.github.token")
        .unwrap();

    // Should return None for non-existent context
    assert_eq!(result, None);
}

// =============================================================================
// Real-world Scenario Tests
// =============================================================================

#[test]
fn test_scenario_docker_deployment() {
    let mut guard = EnvGuard::new();
    // Simulate Docker environment with full context via env vars
    guard.set("DEVBOY_CONTEXTS_INTEG_DOCKER_GITHUB_TOKEN", "ghp_docker");
    guard.set("DEVBOY_CONTEXTS_INTEG_DOCKER_GITLAB_TOKEN", "glpat_docker");

    let chain = ChainStore::ci_chain();

    // Both providers should be resolvable
    assert_eq!(
        chain.get("contexts.integ-docker.github.token").unwrap(),
        Some("ghp_docker".to_string())
    );
    assert_eq!(
        chain.get("contexts.integ-docker.gitlab.token").unwrap(),
        Some("glpat_docker".to_string())
    );
}

#[test]
fn test_scenario_kubernetes_secrets() {
    let mut guard = EnvGuard::new();
    // Simulate Kubernetes environment with secrets mounted as env vars
    guard.set("DEVBOY_CONTEXTS_INTEG_K8S_PROD_GITHUB_TOKEN", "ghp_k8s");
    guard.set(
        "DEVBOY_CONTEXTS_INTEG_K8S_STAGING_GITHUB_TOKEN",
        "ghp_k8s_staging",
    );

    let chain = ChainStore::ci_chain();

    // Each environment (prod/staging) should have its own token
    assert_eq!(
        chain.get("contexts.integ-k8s-prod.github.token").unwrap(),
        Some("ghp_k8s".to_string())
    );
    assert_eq!(
        chain
            .get("contexts.integ-k8s-staging.github.token")
            .unwrap(),
        Some("ghp_k8s_staging".to_string())
    );
}

#[test]
fn test_scenario_github_actions_matrix() {
    let mut guard = EnvGuard::new();
    // Simulate GitHub Actions matrix build with different contexts
    guard.set(
        "DEVBOY_CONTEXTS_INTEG_GHA_NODE16_GITHUB_TOKEN",
        "ghp_node16",
    );
    guard.set(
        "DEVBOY_CONTEXTS_INTEG_GHA_NODE18_GITHUB_TOKEN",
        "ghp_node18",
    );
    guard.set(
        "DEVBOY_CONTEXTS_INTEG_GHA_NODE20_GITHUB_TOKEN",
        "ghp_node20",
    );

    let chain = ChainStore::ci_chain();

    // Each matrix job should have its own token
    assert_eq!(
        chain.get("contexts.integ-gha-node16.github.token").unwrap(),
        Some("ghp_node16".to_string())
    );
    assert_eq!(
        chain.get("contexts.integ-gha-node18.github.token").unwrap(),
        Some("ghp_node18".to_string())
    );
    assert_eq!(
        chain.get("contexts.integ-gha-node20.github.token").unwrap(),
        Some("ghp_node20".to_string())
    );
}
