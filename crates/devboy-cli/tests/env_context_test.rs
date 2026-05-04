//! Integration tests for env-only context configuration.
//!
//! These tests verify that contexts can be defined entirely through
//! environment variables without any config file.
//!
//! Uses `temp_env` for safe env var manipulation (thread-safe, automatic cleanup).

use devboy_storage::{ChainStore, CredentialStore, EnvVarStore};
use secrecy::{ExposeSecret, SecretString};

/// Compare an `Option<SecretString>` from a credential store against an
/// expected plaintext. Pre-existing tests were written against `Option<String>`
/// before the SecretString migration; the helper keeps the assertions
/// readable without leaking the inner value elsewhere.
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
// Context Token Resolution Tests
// =============================================================================

#[test]
fn test_context_github_token_resolution() {
    temp_env::with_var(
        "DEVBOY_CONTEXTS_INTEG_PROD_GITHUB_TOKEN",
        Some("ghp_prod_token"),
        || {
            let store = EnvVarStore::new();
            let result = store.get("contexts.integ-prod.github.token").unwrap();
            assert_secret_eq(result, Some("ghp_prod_token"));
        },
    );
}

#[test]
fn test_context_gitlab_token_resolution() {
    temp_env::with_var(
        "DEVBOY_CONTEXTS_INTEG_STAGING_GITLAB_TOKEN",
        Some("glpat_staging"),
        || {
            let store = EnvVarStore::new();
            let result = store.get("contexts.integ-staging.gitlab.token").unwrap();
            assert_secret_eq(result, Some("glpat_staging"));
        },
    );
}

#[test]
fn test_context_clickup_token_resolution() {
    temp_env::with_var(
        "DEVBOY_CONTEXTS_INTEG_TASKS_CLICKUP_TOKEN",
        Some("pk_tasks_token"),
        || {
            let store = EnvVarStore::new();
            let result = store.get("contexts.integ-tasks.clickup.token").unwrap();
            assert_secret_eq(result, Some("pk_tasks_token"));
        },
    );
}

#[test]
fn test_context_jira_token_resolution() {
    temp_env::with_var(
        "DEVBOY_CONTEXTS_INTEG_JIRA_JIRA_TOKEN",
        Some("jira_token_123"),
        || {
            let store = EnvVarStore::new();
            let result = store.get("contexts.integ-jira.jira.token").unwrap();
            assert_secret_eq(result, Some("jira_token_123"));
        },
    );
}

// =============================================================================
// Multi-part Context Name Tests
// =============================================================================

#[test]
fn test_context_with_underscores_in_name() {
    temp_env::with_var(
        "DEVBOY_CONTEXTS_MY_COOL_PROJECT_GITHUB_TOKEN",
        Some("ghp_cool_project"),
        || {
            let store = EnvVarStore::new();
            let result = store.get("contexts.my-cool-project.github.token").unwrap();
            assert_secret_eq(result, Some("ghp_cool_project"));
        },
    );
}

#[test]
fn test_context_single_word_name() {
    temp_env::with_var(
        "DEVBOY_CONTEXTS_PROD_GITHUB_TOKEN",
        Some("ghp_single_word"),
        || {
            let store = EnvVarStore::new();
            let result = store.get("contexts.prod.github.token").unwrap();
            assert_secret_eq(result, Some("ghp_single_word"));
        },
    );
}

// =============================================================================
// Token Priority Tests
// =============================================================================

#[test]
fn test_context_specific_token_over_global() {
    temp_env::with_vars(
        [
            (
                "DEVBOY_CONTEXTS_INTEG_PRIO_GITHUB_TOKEN",
                Some("ghp_context_specific"),
            ),
            ("DEVBOY_GITHUB_TOKEN", Some("ghp_global")),
        ],
        || {
            let chain = ChainStore::ci_chain();
            let context_result = chain.get("contexts.integ-prio.github.token").unwrap();
            assert_secret_eq(context_result, Some("ghp_context_specific"));
            let global_result = chain.get("github.token").unwrap();
            assert_secret_eq(global_result, Some("ghp_global"));
        },
    );
}

#[test]
fn test_fallback_to_global_token_when_no_context_specific() {
    temp_env::with_var(
        "DEVBOY_INTEG_FALLBACK_GITHUB_TOKEN",
        Some("ghp_global_only"),
        || {
            let chain = ChainStore::ci_chain();
            let context_result = chain
                .get("contexts.nonexistent.integ-fallback.github.token")
                .unwrap();
            assert_secret_eq(context_result, None);
            let global_result = chain.get("integ-fallback.github.token").unwrap();
            assert_secret_eq(global_result, Some("ghp_global_only"));
        },
    );
}

// =============================================================================
// Multiple Providers in Same Context Tests
// =============================================================================

#[test]
fn test_multiple_providers_in_context() {
    temp_env::with_vars(
        [
            (
                "DEVBOY_CONTEXTS_INTEG_MULTI_GITHUB_TOKEN",
                Some("ghp_multi_context"),
            ),
            (
                "DEVBOY_CONTEXTS_INTEG_MULTI_GITLAB_TOKEN",
                Some("glpat_multi_context"),
            ),
            (
                "DEVBOY_CONTEXTS_INTEG_MULTI_CLICKUP_TOKEN",
                Some("pk_multi_context"),
            ),
        ],
        || {
            let store = EnvVarStore::new();
            assert_secret_eq(
                store.get("contexts.integ-multi.github.token").unwrap(),
                Some("ghp_multi_context"),
            );
            assert_secret_eq(
                store.get("contexts.integ-multi.gitlab.token").unwrap(),
                Some("glpat_multi_context"),
            );
            assert_secret_eq(
                store.get("contexts.integ-multi.clickup.token").unwrap(),
                Some("pk_multi_context"),
            );
        },
    );
}

// =============================================================================
// Multiple Contexts Tests
// =============================================================================

#[test]
fn test_multiple_contexts_isolation() {
    temp_env::with_vars(
        [
            (
                "DEVBOY_CONTEXTS_INTEG_CTX_A_GITHUB_TOKEN",
                Some("ghp_context_a"),
            ),
            (
                "DEVBOY_CONTEXTS_INTEG_CTX_B_GITHUB_TOKEN",
                Some("ghp_context_b"),
            ),
            (
                "DEVBOY_CONTEXTS_INTEG_CTX_C_GITHUB_TOKEN",
                Some("ghp_context_c"),
            ),
        ],
        || {
            let store = EnvVarStore::new();
            assert_secret_eq(
                store.get("contexts.integ-ctx-a.github.token").unwrap(),
                Some("ghp_context_a"),
            );
            assert_secret_eq(
                store.get("contexts.integ-ctx-b.github.token").unwrap(),
                Some("ghp_context_b"),
            );
            assert_secret_eq(
                store.get("contexts.integ-ctx-c.github.token").unwrap(),
                Some("ghp_context_c"),
            );
        },
    );
}

// =============================================================================
// CI Chain Tests (env-only mode)
// =============================================================================

#[test]
fn test_ci_chain_context_tokens() {
    temp_env::with_var(
        "DEVBOY_CONTEXTS_INTEG_CI_GITHUB_TOKEN",
        Some("ghp_ci_context"),
        || {
            let chain = ChainStore::ci_chain();
            let result = chain.get("contexts.integ-ci.github.token").unwrap();
            assert_secret_eq(result, Some("ghp_ci_context"));
        },
    );
}

#[test]
fn test_ci_chain_write_to_memory() {
    let chain = ChainStore::ci_chain();
    chain
        .store("ci.context.test.key", &secret("test_value"))
        .expect("Should be able to write in CI chain");
    let result = chain.get("ci.context.test.key").unwrap();
    assert_secret_eq(result, Some("test_value"));
}

// =============================================================================
// Edge Cases
// =============================================================================

#[test]
fn test_double_underscore_in_env_var() {
    temp_env::with_var(
        "DEVBOY_CONTEXTS__GITHUB_TOKEN",
        Some("double_underscore_value"),
        || {
            let store = EnvVarStore::new();
            let result = store.get("contexts..github.token").unwrap();
            assert_secret_eq(result, Some("double_underscore_value"));
        },
    );
}

#[test]
fn test_provider_name_in_context_name() {
    temp_env::with_var(
        "DEVBOY_CONTEXTS_MY_GITHUB_APP_GITHUB_TOKEN",
        Some("ghp_github_in_name"),
        || {
            let store = EnvVarStore::new();
            let result = store.get("contexts.my-github-app.github.token").unwrap();
            assert_secret_eq(result, Some("ghp_github_in_name"));
        },
    );
}

#[test]
fn test_nonexistent_context_returns_none() {
    let store = EnvVarStore::new();
    let result = store
        .get("contexts.completely-nonexistent-ctx-12345.github.token")
        .unwrap();
    assert_secret_eq(result, None);
}

// =============================================================================
// Real-world Scenario Tests
// =============================================================================

#[test]
fn test_scenario_docker_deployment() {
    temp_env::with_vars(
        [
            (
                "DEVBOY_CONTEXTS_INTEG_DOCKER_GITHUB_TOKEN",
                Some("ghp_docker"),
            ),
            (
                "DEVBOY_CONTEXTS_INTEG_DOCKER_GITLAB_TOKEN",
                Some("glpat_docker"),
            ),
        ],
        || {
            let chain = ChainStore::ci_chain();
            assert_secret_eq(
                chain.get("contexts.integ-docker.github.token").unwrap(),
                Some("ghp_docker"),
            );
            assert_secret_eq(
                chain.get("contexts.integ-docker.gitlab.token").unwrap(),
                Some("glpat_docker"),
            );
        },
    );
}

#[test]
fn test_scenario_kubernetes_secrets() {
    temp_env::with_vars(
        [
            (
                "DEVBOY_CONTEXTS_INTEG_K8S_PROD_GITHUB_TOKEN",
                Some("ghp_k8s"),
            ),
            (
                "DEVBOY_CONTEXTS_INTEG_K8S_STAGING_GITHUB_TOKEN",
                Some("ghp_k8s_staging"),
            ),
        ],
        || {
            let chain = ChainStore::ci_chain();
            assert_secret_eq(
                chain.get("contexts.integ-k8s-prod.github.token").unwrap(),
                Some("ghp_k8s"),
            );
            assert_secret_eq(
                chain
                    .get("contexts.integ-k8s-staging.github.token")
                    .unwrap(),
                Some("ghp_k8s_staging"),
            );
        },
    );
}

#[test]
fn test_scenario_github_actions_matrix() {
    temp_env::with_vars(
        [
            (
                "DEVBOY_CONTEXTS_INTEG_GHA_NODE16_GITHUB_TOKEN",
                Some("ghp_node16"),
            ),
            (
                "DEVBOY_CONTEXTS_INTEG_GHA_NODE18_GITHUB_TOKEN",
                Some("ghp_node18"),
            ),
            (
                "DEVBOY_CONTEXTS_INTEG_GHA_NODE20_GITHUB_TOKEN",
                Some("ghp_node20"),
            ),
        ],
        || {
            let chain = ChainStore::ci_chain();
            assert_secret_eq(
                chain.get("contexts.integ-gha-node16.github.token").unwrap(),
                Some("ghp_node16"),
            );
            assert_secret_eq(
                chain.get("contexts.integ-gha-node18.github.token").unwrap(),
                Some("ghp_node18"),
            );
            assert_secret_eq(
                chain.get("contexts.integ-gha-node20.github.token").unwrap(),
                Some("ghp_node20"),
            );
        },
    );
}
