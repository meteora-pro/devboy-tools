# language: en
Feature: A daemon with no screen of its own asks on the one it was lent
  As an architect specifying the ADR-024 §7 prompt channel
  I want a properly-installed daemon to be able to ask a human for the
  passphrase
  So that the configuration the ADR recommends is one a person can
  actually use, rather than one that only works with the passphrase in
  an environment variable

  Background:
    Given the daemon has been reparented to init, as §7 requires
    And a reparented process therefore has no controlling terminal
    And the passphrase must still never enter the client's memory

  @covered-by:a_lent_terminal_unlocks_through_the_dispatcher
  Scenario: The caller lends its terminal and a human unlocks the vault
    Given the caller is running in a terminal
    When it asks the daemon to unlock and names that terminal
    Then the prompt appears on the caller's screen
    And the passphrase typed there opens the vault
    And the passphrase never crossed the socket

  @covered-by:the_daemons_own_terminal_is_preferred_over_a_lent_one
  Scenario: A daemon that does have its own terminal uses it
    Given the daemon was started from a terminal
    When a caller offers one as well
    Then the daemon asks on its own
    And the audit entry records the channel as its own

  @covered-by:a_lent_path_that_is_not_a_terminal_is_refused
  Scenario: A path that is not a terminal is refused
    Given the daemon has no terminal of its own
    When a caller names a path that is not a terminal
    Then the daemon refuses rather than writing the prompt into it
    And the message says which of the two channels failed

  @covered-by:a_path_outside_dev_is_refused_before_anything_opens_it
  Scenario: A path outside /dev is refused before it is opened
    Given the daemon would open the named path for reading and writing
    When a caller names a path outside /dev
    Then the daemon refuses before opening anything
    And the message explains that the prompt would have been written
    into that file

  @covered-by:no_terminal_anywhere_says_so_and_names_the_way_out
  Scenario: Neither side has a terminal
    Given the daemon has none
    And the caller offered none
    Then the refusal is distinct from "what you offered is unusable"
    And it names both ways forward — running the unlock from a
    terminal, or the environment variable for an unattended start

  @covered-by:echo_is_restored_on_the_lent_terminal_afterwards
  Scenario: The lent terminal is left as it was found
    Given reading a passphrase requires turning terminal echo off
    When the daemon has finished reading, whether it succeeded or not
    Then echo is on again
    And the user is not left in a shell that shows nothing they type
