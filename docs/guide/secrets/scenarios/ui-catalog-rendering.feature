# language: en
Feature: Provision dialog binds to the active token catalog
  As a developer (or AI agent) about to fill a missing secret
  I want the provision dialog to show me the exact procedure
  the provider's console expects — description, numbered steps,
  caveats — sourced from the bundled / user / project catalog
  So that I do not have to alt-tab to a wiki or guess the
  format

  Background:
    Given devboy 0.29.0 is installed
    And the manifest declares "team/openai/api-key" as required
    And the bundled OpenAI catalog is loaded
    And the local-vault agent is running

  Scenario: The dialog is a modal overlay, not an inline route
    When I open "devboy secrets ui --gui --provision team/openai/api-key"
    Then the dialog renders as a modal overlay on top of the inventory
    And the inventory stays visible, dimmed, underneath
    And pressing ESC or clicking the dimmed backdrop dismisses the dialog
    And dismissing it returns to the inventory without losing scroll position

  Scenario: Provision mode — important content first, links are links
    When I open "devboy secrets ui --gui --provision team/openai/api-key"
    Then the dialog title is "Provision secret"
    And the metadata grid shows PATH / VIA / FORMAT rows
    And the metadata grid does NOT show a ROTATION row (Provision mode)
    And the variant's `description` renders as an italic block
    And a links row sits ABOVE the steps with two real hyperlinks:
      | label            | target                     |
      | Open console ↗   | retrieval.console_url      |
      | Provider docs ↗  | retrieval.docs_url         |
    And both links open the OS browser directly (no caller round-trip)
    And the "How to obtain:" header + numbered `retrieval.steps` follow the links
    And the "Note: ..." footer carries `retrieval.notes`
    And the dialog does NOT show a "Rotating this secret:" section (Provision mode)
    And the hidden value input sits below a separator as the final action

  Scenario: Rotation mode adds the cadence row and the rotation section
    When I open the rotation dialog for "team/openai/api-key"
    Then the dialog title is "Rotate secret"
    And the metadata grid now includes the ROTATION cadence row
    And a "Rotating this secret:" section renders `rotation.notes` as a block
    And the rotation section shows a "Rotation guide ↗" link targeting `rotation.guide_url`
    And a destructive-confirm checkbox gates the save

  Scenario: Variant with rotation notes but no guide URL still renders the section
    Given the manifest declares "personal/kimi/api-key" and the Kimi catalog has `rotation.notes` but no `rotation.guide_url`
    When I open the rotation dialog for "personal/kimi/api-key"
    Then the "Rotating this secret:" section still renders
    And it shows the `rotation.notes` block
    And no "Rotation guide" link appears (guide_url is absent)

  Scenario: Path without a catalog match falls back to manifest-only rendering
    Given the manifest declares "personal/some-internal-tool/api-key" with no matching catalog
    When I open "devboy secrets ui --gui --provision personal/some-internal-tool/api-key"
    Then the dialog renders PATH and VIA rows
    And no "How to obtain:" header appears
    And no "Note:" footer appears
    And no links row appears (no console_url, no docs_url)

  Scenario: Variant chip selection re-renders the procedure block
    Given the Kimi (Moonshot AI) catalog declares two variants — `kimi-cn` and `kimi-global`
    When I open the provision dialog for "personal/kimi/api-key" and click the `kimi-global` chip
    Then the dialog's description switches to the global variant's wording
    And the steps list re-renders from the global variant's `retrieval.steps`
    And the "Open console ↗" link targets the global variant's `console_url`

  Scenario: TUI mirrors the GUI priority ordering
    When I open "devboy secrets ui --tui --provision team/openai/api-key"
    Then the modal renders inside the terminal at 70% width × 70% height
    And the metadata block carries PATH / VIA / FORMAT lines (no ROTATION row in Provision mode)
    And a CONSOLE line carries `retrieval.console_url`
    And a DOCS line carries `retrieval.docs_url`
    And the description appears in italic style below the metadata block
    And the steps render as "  1. ...", "  2. ...", numbered in order
    And the notes appear dimmed after the steps prefixed with "Note: "
    And no "Rotating this secret:" block appears in Provision mode

  Scenario: TUI rotation mode shows the rotation block
    When I open "devboy secrets ui --tui" and start a rotation for "team/openai/api-key"
    Then the metadata block now carries a ROTATION cadence line
    And a "Rotating this secret:" block renders `rotation.notes` and a "guide: ..." line
