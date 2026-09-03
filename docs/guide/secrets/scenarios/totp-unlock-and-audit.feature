# language: en
Feature: A TOTP code proves a human approved the re-unlock
  As a developer who leaves an agent working against an unlocked vault
  I want a re-unlock to need something the agent cannot produce
  So that a long session does not silently become permanent access

  The guarantee rests on one fact: the shared secret lives in daemon
  memory and in a reserved vault slot, and neither is reachable from
  the agent-facing surface. Everything else here follows from that.

  Background:
    Given a vault with an authenticator enrolled
    And a daemon that was started as a service, not by the agent

  @covered-by:the_totp_secret_is_unreachable_from_the_wire
  Scenario: The agent cannot read the shared secret
    Given the vault is unlocked
    When the agent calls "secret.get" for the reserved TOTP path
    Then the daemon answers as though no such entry exists
    And the path is absent from "secret.list"
    And "secret.put" refuses to write it, so the agent cannot swap
    in a secret of its own

  @covered-by:the_totp_path_lives_and_dies_with_the_unlock
  Scenario: The secret does not outlive the unlock that produced it
    Given the daemon has been unlocked with the passphrase
    Then a TOTP re-unlock is available
    When the vault is locked
    Then the TOTP path is gone
    And a code cannot re-open a vault the user deliberately closed

  @covered-by:totp_unlock_without_a_resident_secret_is_explicit
  Scenario: A restarted daemon says the path is gone rather than failing quietly
    Given the daemon has restarted and has not been unlocked since
    When the agent attempts a TOTP unlock
    Then the refusal names the missing secret
    And it points at a passphrase unlock as the way forward

  @covered-by:the_same_step_cannot_be_used_twice
  Scenario: A code works once
    Given the agent has observed a valid code
    When it presents that code a second time inside the same window
    Then the daemon refuses it as replayed
    And the refusal is distinct from a wrong code, because waiting
    for the next code fixes one and not the other

  @covered-by:an_older_step_is_refused_after_a_newer_one
  Scenario: A captured earlier code does not work later
    Given a code for a later step has been accepted
    When a code for an earlier step is presented
    Then it is refused, even though it would otherwise be inside
    the clock-skew window

  @covered-by:too_many_wrong_codes_shut_the_path @covered-by:even_a_valid_code_is_refused_during_lockout
  Scenario: Guessing is throttled
    When five wrong codes are presented inside thirty seconds
    Then the TOTP path closes for sixty seconds
    And a correct code is refused during the lockout, or waiting
    for one would bypass the limit entirely

  @covered-by:attempts_against_an_absent_secret_do_not_trip_the_limiter
  Scenario: An agent cannot lock a door that was never open
    Given no TOTP secret is resident
    When the agent presents many wrong codes
    Then each is refused as unavailable
    And no lockout accumulates
    And the path works immediately once a secret becomes resident

  @covered-by:failures_outside_the_window_do_not_accumulate
  Scenario: Occasional mistakes do not add up
    Given a user mistypes one code per day
    Then no lockout is ever reached

  @covered-by:each_totp_denial_maps_to_its_own_code @covered-by:every_failure_reply_carries_remediation
  Scenario: Every refusal tells the agent what to do next
    When a TOTP unlock is refused
    Then the reason is one of unavailable, bad code, replayed or
    rate-limited
    And each carries its own remediation
    And an agent given a single undifferentiated "denied" would
    retry a value that can never work

  @covered-by:reading_a_secret_leaves_an_audit_record
  Scenario: Secret access leaves a trace
    Given the vault is unlocked
    When the agent reads a secret
    Then the audit log records the path, the actor and the time
    And the unlock itself is recorded too

  @covered-by:a_failed_lookup_is_not_recorded_as_a_read
  Scenario: A miss is not recorded as a read
    When the agent asks for a path that does not exist
    Then no read is recorded
    And the trail is not padded with accesses that never happened

  @covered-by:a_known_value_is_redacted_before_it_reaches_the_log
  Scenario: A secret value cannot reach the audit log
    Given a detail string happens to quote a provisioned secret
    When the record is written
    Then the value has been replaced by its path
    And the substitution is structural: the record type accepts only
    text the scrubber has produced

  @covered-by:values_too_short_to_scrub_are_counted_and_reported
  Scenario: A secret too short to redact is reported rather than hidden
    Given a provisioned value shorter than the scrubber's minimum
    When the audit trail is opened
    Then the count of unprotected values is reported
    And the user is told, because a leak nobody can see is worse
    than one they can

  @covered-by:a_record_moved_to_another_index_does_not_decrypt
  Scenario: A record cannot be moved within the log
    Given two records of equal length
    When their index entries and ciphertexts are swapped
    Then neither decrypts
    And the tampering is detected rather than silently accepted

  @covered-by:a_truncated_log_is_detected
  Scenario: Records cannot be lopped off the end
    When the last record is removed from the file
    Then reading the log reports truncation
    And this is caught by the header's committed count, which is
    what a plain append-only file has no way to notice

  @covered-by:the_log_is_encrypted_at_rest
  Scenario: The log does not leak what it records
    When records have been written
    Then no path, actor or timestamp is legible in the file
    And the sequence of accesses is as sensitive as the secrets
