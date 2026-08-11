# language: en
Feature: Unlocking and creating the local vault from the UI
  As a developer who keeps secrets in an encrypted local vault
  I want the secrets UI to prompt me for the passphrase
  instead of forcing me to export it as an environment
  variable before launch
  So that the vault is convenient to use without weakening it

  Background:
    Given devboy 0.29.0 is installed
    And the default vault path is "<config>/devboy-tools/secrets/local-vault.dvb"

  @covered-by:is_locked_only_true_for_the_locked_variant @covered-by:labels_distinguish_the_three_backend_states
  Scenario: Env passphrase opens the vault with no prompt
    Given "DEVBOY_VAULT_PASSPHRASE" is set in the environment
    When I launch "devboy secrets ui --gui"
    Then the backend resolves straight to unlocked local-vault
    And no unlock modal is shown
    And the top-bar banner reads "Backend: local-vault — file ..."

  @covered-by:unlock_opens_a_vault_created_with_the_same_passphrase @covered-by:effective_title_falls_back_to_mode_when_not_overridden @covered-by:fresh_state_starts_idle_masked_focused_on_passphrase
  Scenario: An existing vault file with no env passphrase prompts to unlock
    Given a "local-vault.dvb" file exists at the default path
    And "DEVBOY_VAULT_PASSPHRASE" is NOT set
    When I launch "devboy secrets ui --gui"
    Then a modal unlock prompt opens over a dimmed inventory
    And the modal title is "Unlock local vault"
    And the passphrase input is password-masked with an eye-toggle
    When I type the correct passphrase and click "Unlock"
    Then the vault opens
    And the modal closes
    And the inventory reloads showing real provisioned / missing status

  @covered-by:unlock_rejects_a_wrong_passphrase @covered-by:a_wrong_passphrase_never_opens_the_vault @covered-by:apply_status_drives_the_lifecycle
  Scenario: A wrong passphrase keeps the modal open with a red error
    Given the unlock modal is open for an existing vault
    When I type an incorrect passphrase and click "Unlock"
    Then the modal stays open
    And a red "✗ wrong passphrase" line appears below the input
    And the inventory stays gated behind the modal

  @not-covered:use-keychain-flag-never-inspected-by-a-test
  Scenario: The keychain escape hatch skips the vault for the session
    Given the unlock modal is open for an existing vault
    When I click "Use keychain instead"
    Then the modal closes
    And the backend switches to the OS keychain for this session
    And the inventory reloads against the keychain

  @covered-by:create_vault_mints_a_file_and_returns_a_recovery_phrase @covered-by:create_with_recovery_returns_phrase
  Scenario: First run with no vault file offers a create flow
    Given no "local-vault.dvb" file exists
    And "DEVBOY_VAULT_PASSPHRASE" is NOT set
    When I launch "devboy secrets ui --gui"
    Then the backend defaults to the OS keychain
    And the top bar shows a "Switch to encrypted vault" button
    When I click "Switch to encrypted vault"
    Then a "Create encrypted local vault" modal opens
    And it has a passphrase input AND a confirm input
    When I enter matching passphrases and click "Create vault"
    Then the vault file is created at the default path
    And the recovery phrase is shown once, behind an "I've saved this phrase" gate
    And the backend transitions to unlocked local-vault

  @covered-by:validate_rejects_a_create_confirm_mismatch
  Scenario: Create flow rejects a mismatched confirmation
    Given the "Create encrypted local vault" modal is open
    When I enter a passphrase and a confirmation that do not match
    And I click "Create vault"
    Then the modal stays open
    And it shows "passphrase and confirmation do not match"
    And no vault file is created

  @covered-by:lock_drops_the_passphrase_back_to_locked @covered-by:is_locked_only_true_for_the_locked_variant
  Scenario: Live backend switching from the top bar
    Given the backend is unlocked local-vault
    When I click "Lock vault" in the top bar
    Then the held passphrase is dropped
    And the unlock modal returns
    When the unlock modal is open and I click "Switch to keychain" is unavailable
    Then the only escape is "Use keychain instead" inside the modal

  @not-covered:no-render-scan-for-the-passphrase-in-the-unlock-modal
  Scenario: The agent never sees the passphrase
    Given the unlock modal is open
    When I type a passphrase
    Then the passphrase is held in a SecretString, zeroized when the modal closes
    And no MCP tool response and no log line carries the passphrase
