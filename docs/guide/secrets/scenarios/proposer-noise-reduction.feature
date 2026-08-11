# language: en
Feature: Proposer drops noise but keeps real credentials
  As a developer running the setup-secrets wizard on a real project
  I want the proposer to skip configuration / connection / CI runner
  variables that look like credentials but are not
  So that the manifest review list is short enough to actually read
  and the genuine credentials are easy to spot in the diff

  # All scenarios use a hypothetical project with the env vars below.
  # Counts pin the cumulative effect of P1-P5 + S2 + bundled catalogs.

  Background:
    Given the bundled token catalogs are loaded (anthropic, gemini, gitlab, langfuse, ollama, slack, …)

  @covered-by:proposer_skips_configuration_suffixes
  Scenario Outline: Configuration / connection metadata is dropped (P1)
    When the proposer sees env-var "<env_var>"
    Then the proposal is "Skip" with reason matching "configuration / connection metadata"

    Examples:
      | env_var                  |
      | OPENAI_API_BASE          |
      | ANTHROPIC_MODEL          |
      | ANTHROPIC_CONCURRENCY    |
      | EMBEDDING_MODEL          |
      | LLM_MODEL                |
      | REDIS_HOST               |
      | REDIS_PORT               |
      | REDIS_DB                 |
      | LOG_LEVEL                |
      | HTTP_TIMEOUT             |
      | CACHE_DIR                |
      | S3_ENDPOINT              |

  @covered-by:proposer_skips_ci_runner_prefixes
  Scenario Outline: CI runner / build agent / cloud-platform variables are dropped (P2)
    When the proposer sees env-var "<env_var>"
    Then the proposal is "Skip" with reason matching "CI runner / build agent / cloud metadata"

    Examples:
      | env_var                       |
      | CI_COMMIT_BRANCH              |
      | CI_PIPELINE_ID                |
      | CI_RUNNER_ID                  |
      | BUILD_DATE                    |
      | GITHUB_RUN_ID                 |
      | GITHUB_REF_NAME               |
      | RUNNER_OS                     |
      | VERCEL_URL_INTERNAL           |
      | CI                            |

  @covered-by:proposer_skips_windows_machine_vars
  Scenario Outline: Windows machine vars are dropped (P3)
    When the proposer sees env-var "<env_var>"
    Then the proposal is "Skip" with reason matching "environment / machine variable"

    Examples:
      | env_var                  |
      | COMPUTERNAME             |
      | APPDATA                  |
      | LOCALAPPDATA             |
      | USERPROFILE              |
      | HOMEDRIVE                |
      | OS                       |
      | SYSTEMROOT               |
      | WINDIR                   |
      | PROGRAMFILES             |
      | TEMP                     |
      | PROCESSOR_ARCHITECTURE   |

  @covered-by:proposer_skips_posix_locale_and_xdg
  Scenario Outline: POSIX locale + agent metadata is dropped (P4)
    When the proposer sees env-var "<env_var>"
    Then the proposal is "Skip" with reason matching "CI runner / build agent / cloud metadata|environment / machine variable"

    Examples:
      | env_var          |
      | LC_ALL           |
      | LC_CTYPE         |
      | XDG_CONFIG_HOME  |
      | XDG_RUNTIME_DIR  |
      | GPG_TTY          |
      | SSH_AUTH_SOCK    |
      | EDITOR           |
      | PAGER            |

  @covered-by:proposer_skips_ambiguous_generic_segments @covered-by:proposer_keeps_ambiguous_when_provider_actually_known
  Scenario Outline: Ambiguous generic-segment names are skipped (P5)
    When the proposer sees env-var "<env_var>"
    Then the proposal is "Skip" with reason matching "ambiguous provider segment"

    Examples:
      | env_var              |
      | SERVICE_TOKEN        |
      | MEET_SERVICE_TOKEN   |
      | API_KEY              |
      | BOT_IMAGE            |
      | WORKER_TOKEN         |
      | CORE_SECRET          |
      | DB_PASSWORD          |

  @covered-by:proposer_picks_catalog_pattern_over_heuristic @covered-by:proposer_openai_api_key_known_provider @covered-by:proposer_catalog_pattern_supports_glob_wildcard
  Scenario Outline: Credentials route to canonical paths (S2 + bundled)
    When the proposer sees env-var "<env_var>"
    Then the proposal is "Path" with path "<path>" and provider_known = true

    Note: two mechanisms are at work here and the table does not
    distinguish them. Rows whose provider ships an `env_var_patterns`
    block come from the catalog; `OPENAI_API_KEY` does not — openai.json
    declares no patterns, so that row comes from the heuristic. The
    covering tests exercise the mechanisms against synthetic catalogs
    rather than the bundled ones, so these specific rows are illustrative
    of the shape, not pinned end-to-end.

    Examples:
      | env_var                | path                                    |
      | OPENAI_API_KEY         | team/openai/api-key                     |
      | ANTHROPIC_API_KEY      | team/anthropic/anthropic-api-key        |
      | GEMINI_API_KEY         | team/gemini/gemini-api-key              |
      | LANGFUSE_PUBLIC_KEY    | team/langfuse/langfuse-public-key       |
      | LANGFUSE_SECRET_KEY    | team/langfuse/langfuse-secret-key       |
      | OLLAMA_API_KEY         | team/ollama/ollama-api-key              |
      | CLICKUP_TOKEN          | team/clickup/clickup-api-key            |
      | JIRA_API_TOKEN         | team/jira/jira-api-token                |
      | SLACK_BOT_TOKEN        | team/slack/slack-bot-token              |

  @covered-by:proposer_catalog_pattern_honours_personal_scope
  Scenario Outline: E2E_* / personal-scope patterns map to personal/
    When the proposer sees env-var "<env_var>"
    Then the proposal is "Path" with path "<path>" and scope = "personal"

    Examples:
      | env_var                       | path                                |
      | E2E_OPENAI_API_KEY            | personal/openai/openai-api-key      |
      | E2E_GEMINI_API_KEY            | personal/gemini/gemini-api-key      |
      | E2E_ANTHROPIC_API_KEY         | personal/anthropic/anthropic-api-key|

  @not-covered:no-demo-project-fixture-for-the-pinned-counts
  Scenario: Cumulative noise reduction on the canonical demo project
    Given the meteora/devboy-env-1 fixture project (845 env-var references in 123 files)
    When the proposer is run with the bundled catalogs
    Then the proposed-path count is materially lower than the pre-P1 baseline
    (the exact figures quoted here were measured once by hand and no fixture
    reproduces them, which is why this scenario is marked @not-covered)
    And every real credential listed in the catalog patterns is mapped, not skipped
