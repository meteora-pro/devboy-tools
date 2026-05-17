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

  Scenario: Env passphrase opens the vault with no prompt
    Given "DEVBOY_VAULT_PASSPHRASE" is set in the environment
    When I launch "devboy secrets ui --gui"
    Then the backend resolves straight to unlocked local-vault
    And no unlock modal is shown
    And the top-bar banner reads "Backend: local-vault — file ..."

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

  Scenario: A wrong passphrase keeps the modal open with a red error
    Given the unlock modal is open for an existing vault
    When I type an incorrect passphrase and click "Unlock"
    Then the modal stays open
    And a red "✗ wrong passphrase" line appears below the input
    And the inventory stays gated behind the modal

  Scenario: The keychain escape hatch skips the vault for the session
    Given the unlock modal is open for an existing vault
    When I click "Use keychain instead"
    Then the modal closes
    And the backend switches to the OS keychain for this session
    And the inventory reloads against the keychain

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

  Scenario: Create flow rejects a mismatched confirmation
    Given the "Create encrypted local vault" modal is open
    When I enter a passphrase and a confirmation that do not match
    And I click "Create vault"
    Then the modal stays open
    And it shows "passphrase and confirmation do not match"
    And no vault file is created

  Scenario: Live backend switching from the top bar
    Given the backend is unlocked local-vault
    When I click "Lock vault" in the top bar
    Then the held passphrase is dropped
    And the unlock modal returns
    When the unlock modal is open and I click "Switch to keychain" is unavailable
    Then the only escape is "Use keychain instead" inside the modal

  Scenario: The agent never sees the passphrase
    Given the unlock modal is open
    When I type a passphrase
    Then the passphrase is held in a SecretString, zeroized when the modal closes
    And no MCP tool response and no log line carries the passphrase
