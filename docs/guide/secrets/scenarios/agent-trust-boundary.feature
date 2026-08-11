# language: en
Feature: Agent surface never leaks secret values
  As an architect specifying the MCP-side contract for AI agents
  I want a typed boundary that physically prevents an agent reply from
  carrying a secret value
  So that "agent never sees the value" is a property of the surface,
  not a runtime guard or an honour-system convention

  Background:
    Given the devboy MCP server is running with the secrets_* tool family registered
    And a project manifest declares "team/jira/api-key" as required

  @covered-by:list_response_does_not_carry_value_field @covered-by:list_surfaces_policy_in_each_row
  Scenario: secrets_list returns metadata only
    When the agent calls "tools/call secrets_list"
    Then the reply contains entries with fields: path, status, expires_at, source_name, capabilities_hint, approve_on_use?
    And NO entry contains a "value" field
    And NO entry contains anything that would deserialise as a SecretString

  @covered-by:describe_response_does_not_carry_value_field @covered-by:describe_returns_full_metadata_for_known_path
  Scenario: secrets_describe returns metadata only
    When the agent calls "tools/call secrets_describe path: \"team/jira/api-key\""
    Then the reply contains description / retrieval_url / rotation_method / pattern_id / approve_on_use?
    And NO field carries the secret value

  @covered-by:secrets_request_provision_response_never_carries_sentinel @covered-by:secrets_poll_status_response_never_carries_sentinel
  Scenario: secrets_request_provision opens a UI-side dialog, never returns the value
    When the agent calls "tools/call secrets_request_provision path: \"team/jira/api-key\""
    Then the reply contains only a request_id ("prov-<12-hex>") with no value field
    When the agent polls "secrets_poll_status" repeatedly
    Then the response cycles through pending → ok / cancelled / expired / failed
    And no terminal status carries the value the user typed in the dialog

  @covered-by:secrets_request_use_approval_response_never_carries_sentinel @covered-by:gated_resolver_allows_session_policy_after_cache_record
  Scenario: secrets_request_use_approval cannot leak the value either
    When the agent calls "tools/call secrets_request_use_approval path: \"team/prod-db/password\" reason: \"deploy\""
    Then the reply contains only a request_id with no value field
    When polling settles to status: "session" or "once"
    Then the agent receives ONLY the verdict
    And the actual secret value never crosses the MCP wire
    And the daemon-side resolver supplies the value to the high-level provider tool internally

  @covered-by:expose_secret_only_appears_in_allowlisted_mcp_files @covered-by:allowlist_entries_actually_match_real_files
  Scenario: AgentSafeReply marker enforces invariant at compile time
    Given a contributor adds a new field "value: SecretString" to SecretsListItem
    When CI runs the workspace check
    Then the build fails with a typed error because SecretString does not implement AgentSafeReply
    And the negative test in tests/secret_tool_responses_never_leak_value.rs catches the leak even if the marker is bypassed
    And the grep gate in tests/no_expose_secret_outside_allowlist.rs flags any new `.expose_secret()` call outside the allowlist

  @covered-by:a_declined_approval_ends_the_exchange @covered-by:gated_resolver_always_refuses_per_call_even_with_cache
  Scenario: Approve-on-use deny propagates as a hard error, not a hidden value
    Given "team/prod-db/password" has approve_on_use = "per-call"
    When the agent invokes a high-level tool that needs the value
    And the user clicks "Deny" in the dialog
    Then the high-level tool returns an MCP error result (isError = true)
    And the error reason names the path and the policy ("denied by user; path requires per-call approval")
    And the agent never receives the value
