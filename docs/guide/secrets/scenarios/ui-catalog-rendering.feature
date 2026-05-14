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

  Scenario: Catalog-matched path renders the full procedure block
    When I open "devboy secrets ui --gui --provision team/openai/api-key"
    Then the dialog title is "Provision secret"
    And the PATH row shows "team/openai/api-key"
    And the FORMAT row shows the variant's `format_hint` ("starts with sk-, 20+ chars")
    And the dialog includes the variant's `description` rendered as an italic block
    And the dialog includes a "How to obtain:" header
    And the dialog lists every entry of `retrieval.steps` numbered "1.", "2.", "3.", "4.", "5."
    And the dialog includes a "Note: ..." footer with `retrieval.notes` content
    And the [Open URL] button targets `retrieval.console_url`

  Scenario: Path without a catalog match falls back to manifest-only rendering
    Given the manifest declares "personal/some-internal-tool/api-key" with no matching catalog
    When I open "devboy secrets ui --gui --provision personal/some-internal-tool/api-key"
    Then the dialog renders PATH, VIA, ROTATION rows
    And no "How to obtain:" header appears
    And no "Note:" footer appears
    And the [Open URL] button is disabled (no `retrieval_url` in the manifest either)

  Scenario: Variant chip selection re-renders the procedure block
    Given the Kimi (Moonshot AI) catalog declares two variants — `kimi-cn` and `kimi-global`
    When I open the provision dialog for "personal/kimi/api-key" and click the `kimi-global` chip
    Then the dialog's description switches to the global variant's wording
    And the steps list re-renders from the global variant's `retrieval.steps`
    And the [Open URL] button targets the global variant's `console_url`

  Scenario: TUI mirrors the GUI procedure layout
    When I open "devboy secrets ui --tui --provision team/openai/api-key"
    Then the modal renders inside the terminal at 70% width × 70% height
    And the metadata block carries PATH / VIA / ROTATION / FORMAT lines
    And the description appears in italic style below the metadata block
    And the steps render as "  1. ...", "  2. ...", numbered in order
    And the notes appear dimmed after the steps prefixed with "Note: "
