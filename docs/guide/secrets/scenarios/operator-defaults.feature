# language: en
Feature: An operator sets the posture a fresh install starts from
  As someone rolling devboy out across a team
  I want the config server my people point at to decide how
  secrets are handled by default
  And I want that decision written into their config file rather
  than re-imposed over the network on every command
  So that a fleet starts consistent without anyone losing control
  of their own machine

  Background:
    Given devboy is being set up against a remote config URL

  @covered-by:an_operator_can_pin_the_profile_and_the_window
  Scenario: The operator pins the unlock profile and window
    Given the remote config carries a secrets section
    When I run "devboy init" against it
    Then the profile and the unlock window land in my local
      config
    And the command prints what it applied

  @covered-by:an_operator_can_turn_the_keychain_back_on_for_their_fleet
  Scenario: A fleet that wants the OS keychain can have it
    Given the remote config enables the OS keychain
    When I run "devboy init" against it
    Then the keychain is enabled in my local config
    And I can still turn it off myself afterwards

  @covered-by:the_runtime_merge_does_not_carry_the_secrets_section
  Scenario: The posture is not renegotiated on every command
    Given my config was set up from a remote config
    And the remote config later changes its secrets section
    When devboy runs any other command
    Then my secrets posture is unchanged
    Because a security setting I cannot see in a file I own is
      not a setting I control

  @covered-by:a_keyfile_path_from_the_remote_side_is_dropped
  Scenario: The remote side cannot pick which file is key material
    Given the remote config sets a keyfile path
    When I run "devboy init" against it
    Then the keyfile path is ignored
    And my keyfile stays at its local default

  @covered-by:migration_complete_from_the_remote_side_is_dropped
  Scenario: The remote side cannot declare my migration finished
    Given secrets of mine still live in the OS keychain
    And the remote config claims migration is complete
    When I run "devboy init" against it
    Then the claim is ignored
    And the read-only legacy fallback stays on

  @covered-by:a_window_above_its_own_ceiling_is_refused
  Scenario: A posture that cannot mean what it says is refused
    Given the remote config sets an unlock window above its own
      ceiling
    When I run "devboy init" against it
    Then the command says which two values disagree
    And it does not quietly clamp one of them

  @covered-by:a_remote_posture_cannot_reach_the_keyfile_path_or_the_migration_flag
  Scenario: The fields that are never accepted stay at local defaults
    Given a remote config that sets every field it can
    When the posture is turned into my local config
    Then the keyfile path and the migration flag are untouched
