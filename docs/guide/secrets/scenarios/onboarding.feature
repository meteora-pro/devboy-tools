# language: en
Feature: First-run onboarding wizard picks where secrets live
  As a developer launching devboy's secrets UI for the first time
  I want a wizard that lets me choose — and combine — backends
  (OS keychain, encrypted local vault, HashiCorp Cloud Vault)
  So that I do not have to hand-write sources.toml or export
  environment variables before the UI is useful

  Background:
    Given devboy 0.29.0 is installed
    And there is no "sources.toml"
    And there is no local-vault ".dvb" file
    And "DEVBOY_VAULT_PASSPHRASE" is NOT set

  @covered-by:fresh_state_has_keychain_preselected_as_primary @covered-by:render_keychain_only_does_not_panic
  Scenario: First launch shows the onboarding wizard
    When I launch "devboy secrets ui --gui"
    Then a modal onboarding wizard opens over a dimmed window
    And the title is "Set up devboy secrets"
    And the "OS keychain" provider is pre-ticked and is the primary
    And "Encrypted local vault" and "HashiCorp Cloud Vault" are unticked

  @covered-by:to_sources_toml_keychain_only @covered-by:validate_passes_for_keychain_only
  Scenario: Keychain-only — the zero-config path
    Given the onboarding wizard is open
    When I leave only "OS keychain" ticked and click "Finish setup"
    Then "sources.toml" is written with a single keychain "[[source]]"
    And "[default].source" is "default-keychain"
    And the backend resolves to the OS keychain
    And no vault file is created

  @covered-by:to_sources_toml_all_three_with_http_vault_primary @covered-by:create_vault_mints_a_file_and_returns_a_recovery_phrase @covered-by:http_vault_read_access_refuses_a_write_pointing_at_the_config
  Scenario: Combining all three backends
    Given the onboarding wizard is open
    When I tick "Encrypted local vault" and enter a matching passphrase pair
    And I tick "HashiCorp Cloud Vault" and enter the address, namespace, mount and token
    And I pick "HashiCorp Cloud Vault" as the primary backend
    And I click "Finish setup"
    Then "sources.toml" contains three "[[source]]" entries — keychain, local-vault, vault
    And the HCP Vault entry carries addr / namespace / mount / token / access
    And "[default].source" is "hcp-vault"
    And the local-vault ".dvb" file is created and its recovery phrase is shown once
    And the backend resolves to the HCP Vault (read-only until P15 write support lands)

  @covered-by:source_access_read_parses_and_does_not_leak_into_settings @covered-by:http_vault_read_access_refuses_a_write_pointing_at_the_config
  Scenario: HCP Vault access mode is read-only by default-safe choice
    Given the onboarding wizard is open with "HashiCorp Cloud Vault" ticked
    Then the Access radio offers "read-only" and "read-write"
    And "read-write" is annotated that write-back ships in P15
    When I select "read-only" and finish
    Then the vault "[[source]]" carries 'access = "read"'
    And the UI refuses any write to that source with a clear read-only message

  @covered-by:validate_rejects_zero_providers @covered-by:validate_rejects_local_vault_passphrase_mismatch
  Scenario: Validation blocks an incomplete submission
    Given the onboarding wizard is open
    When I untick every provider and click "Finish setup"
    Then the wizard stays open
    And it shows "pick at least one backend"
    When I tick "Encrypted local vault" with a passphrase that does not match its confirmation
    And I click "Finish setup"
    Then it shows "passphrase and confirmation do not match"
    And no "sources.toml" is written

  @not-covered:skip-flag-never-inspected
  Scenario: Skip goes straight to the keychain
    Given the onboarding wizard is open
    When I click "Skip — use keychain"
    Then no "sources.toml" is written
    And the backend resolves to the OS keychain for this session

  @not-covered:is-first-run-untestable-reads-env-and-dirs-directly
  Scenario: The wizard does not re-appear once onboarded
    Given a previous run wrote "sources.toml"
    When I launch "devboy secrets ui --gui"
    Then the onboarding wizard does NOT open
    And the UI uses the configured backend (the unlock modal handles a locked vault)

  @covered-by:to_sources_toml_all_three_with_http_vault_primary
  Scenario: Deferred — multi-source routing
    # The wizard writes every selected backend into sources.toml,
    # but the UI drives exactly ONE (the primary) until the P11
    # router orchestration lands. Likewise the HCP Vault source
    # is read-only because the SecretSource write surface is P15.
    Given the onboarding wizard wrote three "[[source]]" entries
    Then only the primary backend is active in the UI today
    And the other entries are recorded, ready for P11 multi-source routing
