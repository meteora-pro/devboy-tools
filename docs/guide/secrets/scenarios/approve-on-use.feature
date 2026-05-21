# language: en
Feature: Approve-on-use protocol gates resolve-time access to high-stakes paths
  As a security-conscious developer who shares a workstation with AI agents
  I want certain paths (production passwords, signing keys) to require
  explicit approval at every USE, not just at provision time
  So that an agent that knows the path cannot silently exfiltrate the value
  by routing it through any high-level provider tool

  Background:
    Given the secret framework is provisioned and a daemon is running
    And the manifest declares:
      | path                       | approve_on_use |
      | team/jira/api-key          | session        |
      | team/prod-db/password      | per-call       |
      | personal/openai/api-key    | never          |

  Scenario: `never` paths resolve silently with no dialog
    When the agent invokes a tool that resolves "@secret:personal/openai/api-key"
    Then the value is resolved without surfacing the use-approval dialog
    And the agent observes the high-level tool's normal response
    And the SessionApprovalCache records nothing for this path

  Scenario: `session` paths prompt once, then cache for the rest of the session
    When the agent invokes a tool that resolves "@secret:team/jira/api-key" for the first time
    Then the daemon opens the use-approval dialog with the agent-supplied reason rendered verbatim
    And the user clicks "Allow always (this session)"
    And the resolve completes successfully
    When the SAME agent resolves "@secret:team/jira/api-key" again in the same process
    Then no dialog opens
    And the SessionApprovalCache returns AlreadyApproved

  Scenario: `per-call` paths prompt on every resolve regardless of cache
    When the agent resolves "@secret:team/prod-db/password" three times in a row
    Then the dialog opens THREE times
    And each click of "Allow once" produces a one-shot resolve

  Scenario: User denies, agent gets a hard error
    When the agent invokes a tool that resolves "@secret:team/jira/api-key"
    And the user clicks "Deny"
    Then the daemon refuses the resolve
    And the high-level tool returns an error to the agent surface
    And the SessionApprovalCache stays empty for this path
    And no future call within this session can promote the denial without a fresh dialog

  Scenario: Agent cannot escalate a `denied`
    Given the user denied the most recent use-approval request for "@secret:team/prod-db/password"
    When the agent attempts to extend ttl_seconds to a larger value via secrets_request_use_approval
    Then the request_id is issued normally but the registry caps the TTL at the registry-wide 5-minute maximum
    And no MCP tool exists that lets the agent forge an approval
    And no MCP tool exists that lets the agent forget the denial

  Scenario: Agent inspects approve_on_use up-front via secrets_describe
    When the agent calls "secrets_describe(path: \"team/prod-db/password\")"
    Then the reply includes "approve_on_use: \"per-call\""
    And the agent can pre-warn the user "this resolve will surface a dialog"
    When the agent calls "secrets_describe(path: \"personal/openai/api-key\")"
    Then the reply OMITS the approve_on_use field
    And the agent treats absence as the default "never"
