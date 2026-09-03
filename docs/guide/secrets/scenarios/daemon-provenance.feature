# language: en
Feature: The daemon refuses to run where the agent could read its memory
  As an architect specifying the ADR-024 §7 trust boundary
  I want the daemon to check its own ancestry at startup
  And to refuse rather than warn when the check fails
  So that "the agent cannot read the vault key" is a property of
  how the process was started, not a hope about how it is used

  Background:
    Given kernel.yama.ptrace_scope is 1, so a process may only trace
    its own descendants
    And the daemon holds the unlocked vault key in memory

  @covered-by:check_b_refuses_a_session_parented_daemon
  Scenario: A daemon started by its caller refuses to run
    Given a client spawns the daemon as a direct child
    When the daemon runs its startup check
    Then it exits rather than serving
    And the message explains that the caller could trace it
    And it names the platform command that starts it properly

  @covered-by:check_b_accepts_an_init_reparented_daemon
  Scenario: A daemon reparented to init starts normally
    Given the daemon has been double-forked and adopted by init
    When it runs its startup check
    Then it serves requests
    And no warning is emitted

  @covered-by:double_forking_past_check_a_also_severs_the_ptrace_relationship
  Scenario: Passing the check is not a formality
    Given a caller double-forks the daemon to get past the ancestry check
    When the caller then attempts to trace the daemon
    Then the trace is refused by the kernel
    And the check and the protection agree, rather than the check
    being a box the caller can tick

  @covered-by:check_c_is_advisory_only
  Scenario: Holding a terminal is reported but never fatal
    Given the daemon has a controlling terminal
    When it runs its startup check
    Then it may still serve
    And the terminal is reported as a warning rather than a refusal

  @covered-by:the_override_never_upgrades_what_the_daemon_claims
  Scenario: The escape hatch does not launder the trust level
    Given "DEVBOY_INSECURE_ALLOW_UNTRUSTED_DAEMON" is set
    And the daemon was started by its caller
    When it runs its startup check
    Then it serves, because tests need to
    But the reported trust level is still agent-parented
    And the TOTP path stays unavailable, because a code proves
    nothing when the secret can be read from memory

  @covered-by:the_override_warning_is_not_a_one_time_notice
  Scenario: The override keeps saying so
    Given the daemon is running under the insecure override
    When it reports its state more than once
    Then the warning appears every time
    And it does not decay into something a reader stops noticing
