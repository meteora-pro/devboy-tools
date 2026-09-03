# language: en
Feature: Moving secrets out of the OS keychain
  As someone upgrading from a release where the OS keychain was
  the credential store
  I want "devboy secrets migrate" to move my secrets into the
  store that reads still consult
  And I want it to refuse rather than guess whenever moving one
  would cost me the other copy
  So that an upgrade never trades a working secret for a tidy
  path

  Background:
    Given the OS keychain still holds secrets written by an
      earlier release
    And the keychain is no longer part of the credential chain

  @covered-by:execute_plan_moves_the_value_from_the_legacy_store_to_the_destination
  Scenario: A migrated secret lands where reads will look for it
    Given the keychain holds "github/token"
    And the local vault is unlocked
    When I run "devboy secrets migrate github/token"
    Then the value is written to the local vault at the
      suggested path
    And the keychain no longer holds "github/token"
    And nothing was written back into the keychain

  @covered-by:a_failing_destination_leaves_the_legacy_entry_alone
  Scenario: A destination that cannot accept the write costs nothing
    Given the keychain holds "github/token"
    And the local vault is locked
    When I run "devboy secrets migrate github/token"
    Then the command fails and says the write did not happen
    And the keychain still holds "github/token"

  @covered-by:a_second_run_over_the_same_value_writes_nothing_and_still_clears_the_legacy_entry
  Scenario: Running the same migration twice is a no-op
    Given "github/token" was already migrated to the vault
    And the keychain copy was kept
    When I run "devboy secrets migrate github/token" again
    Then the report says the secret was already migrated
    And the vault value is left exactly as it was
    And the redundant keychain copy is removed

  @covered-by:a_different_secret_at_the_target_is_neither_overwritten_nor_costs_the_legacy_copy
  Scenario: An unrelated secret already sitting on the target path
    Given the vault holds a different secret at the suggested
      path
    And the keychain holds "github/token"
    When I run "devboy secrets migrate github/token"
    Then the migration is skipped and says why
    And the secret already in the vault is untouched
    And the keychain copy is kept, because it is the only one
    And the index is not pointed at the occupied path

  @covered-by:a_value_written_before_the_upgrade_still_resolves
  Scenario: An upgrade does not silently lose every secret I own
    Given I upgraded from a release where the keychain was the
      credential store
    And I have not migrated anything yet
    When a secret is resolved
    Then the value still comes back, from the read-only legacy
      fallback
    And a warning names the release the fallback goes away in

  @covered-by:the_same_key_warns_once_and_a_second_key_warns_again
  Scenario: The warning is one line per secret, not one per read
    Given two secrets resolve through the legacy fallback
    And one of them is read several times in a single command
    When the command finishes
    Then each secret warned exactly once
    And neither warning was repeated per read

  @covered-by:writes_are_refused_and_say_where_to_put_it_instead
  Scenario: The fallback never becomes a place to put new secrets
    Given the legacy fallback is active
    When something tries to write a secret through it
    Then the write is refused
    And the refusal names somewhere the value can actually go

  @covered-by:a_partly_migrated_keychain_still_has_entries_remaining
  Scenario: A half-finished migration keeps the fallback on
    Given two secrets are in the keychain
    And I migrate only one of them
    Then the fallback stays on
    And the report says how many entries are left

  @covered-by:an_empty_keychain_has_nothing_remaining
  Scenario: Finishing the migration switches the fallback off
    Given every legacy entry has been migrated
    When the last one is moved
    Then migration_complete is recorded
    And the credential chain stops consulting the keychain

  @covered-by:detects_present_legacy_entries_and_emits_warning
  Scenario: Doctor says what is standing on the fallback
    Given secrets resolve only through the legacy fallback
    When I run "devboy doctor"
    Then it reports how many secrets depend on it
    And it names the release it is removed in
