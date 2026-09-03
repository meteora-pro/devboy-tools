# language: en
Feature: CI runs on environment variables alone
  As a pipeline that has no keychain, no daemon and no human
  I want every secret to resolve from the environment
  And I want a missing one to fail loudly rather than hang
  So that a build that cannot get a secret says so in seconds
  instead of timing out on a prompt nobody can answer

  Background:
    Given devboy is installed in a container with no OS keychain
    And no secret daemon is running

  @covered-by:explicit_ci_flag_selects_env_only_mode
  Scenario: DEVBOY_CI selects env-only mode and says so
    Given "DEVBOY_CI" is set to "1"
    When I run "devboy doctor --checks secrets-mode"
    Then the report names the mode as "env-only"
    And it lists the chain as environment variables

  @covered-by:heuristic_ci_variables_do_not_flip_the_mode
  Scenario: An unrelated CI variable does not change the security posture
    Given "CI" is set to "true" by some other tool
    And neither "DEVBOY_CI" nor "--ci" was given
    When I run "devboy doctor --checks secrets-mode"
    Then the mode is still "env-default"
    And the report raises a notice that a CI heuristic was seen
    But the posture is unchanged

  @covered-by:the_default_chain_excludes_the_keychain
  Scenario: The OS keychain is absent from the default chain
    Given no CI signal is present
    When I run "devboy doctor --checks secrets-mode"
    Then the mode is "env-default"
    And the report names the chain it will actually use

  @covered-by:legacy_env_names_still_resolve_in_ci_mode
  Scenario Outline: A pipeline written before ADR-021 keeps working
    Given "DEVBOY_CI" is set to "1"
    And "<variable>" is set to a GitLab token
    When I run "devboy doctor --checks gitlab-token"
    Then the check does not report the token as missing
    And the token value never appears in the output

    Examples:
      | variable            |
      | DEVBOY_GITLAB_TOKEN |
      | GITLAB_TOKEN        |

  @covered-by:common_commands_complete_without_prompting_in_ci_mode
  Scenario: Nothing blocks on a prompt
    Given "DEVBOY_CI" is set to "1"
    And stdin is closed
    When I run "devboy doctor", "devboy secrets list" and "devboy config get"
    Then each command terminates
    And none of them asks for a passphrase

  @covered-by:a_missing_secret_names_the_variables_that_would_satisfy_it
  Scenario: A missing secret names the variable that would supply it
    Given "DEVBOY_CI" is set to "1"
    And nothing supplies "team/gitlab/token"
    When I run "devboy secrets describe team/gitlab/token"
    Then the command fails
    And the error lists the environment variables that would satisfy the path
    And the names are copy-pasteable into a CI configuration

  @covered-by:configuration_round_trips_without_interaction
  Scenario: A CI image can configure the framework non-interactively
    Given "DEVBOY_CI" is set to "1"
    When I run "devboy config set secrets.profile strict"
    And I run "devboy config get secrets.profile"
    Then the value comes back as "strict"
    And neither command required a terminal

  @covered-by:the_legacy_skip_keychain_switch_still_selects_env_only
  Scenario: The pre-ADR-024 switch still works
    Given "DEVBOY_SKIP_KEYCHAIN" is set to "1"
    When I run "devboy doctor --checks secrets-mode"
    Then the chain is environment variables only
