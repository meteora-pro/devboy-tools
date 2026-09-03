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

  @not-covered:no-egui-interaction-harness-for-the-modal
  Scenario: The dialog is a modal overlay, not an inline route
    When I open "devboy secrets ui --gui --provision team/openai/api-key"
    Then the dialog renders as a modal overlay on top of the inventory
    And the inventory stays visible, dimmed, underneath
    And pressing ESC or clicking the dimmed backdrop dismisses the dialog
    And dismissing it returns to the inventory without losing scroll position

  @covered-by:render_does_not_panic_with_value_revealed @covered-by:reveal_starts_off_so_the_value_is_masked_by_default @covered-by:provision_title_is_pinned_to_adr_copy
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
    And the value input sits below a separator as the final action
    And the value input is password-masked by default
    And an eye-toggle next to the input unmasks the value in place when clicked
    And there is no separate "Show entered value" checkbox

  @covered-by:rotation_title_is_pinned_to_adr_copy @covered-by:rotation_submit_blocks_until_destructive_confirm_is_acknowledged @covered-by:rotation_requires_confirmation_provision_does_not
  Scenario: Rotation mode adds the cadence row and the rotation section
    When I open the rotation dialog for "team/openai/api-key"
    Then the dialog title is "Rotate secret"
    And the metadata grid now includes the ROTATION cadence row
    And a "Rotating this secret:" section renders `rotation.notes` as a block
    And the rotation section shows a "Rotation guide ↗" link targeting `rotation.guide_url`
    And a destructive-confirm checkbox gates the save

  @not-covered:no-fixture-with-rotation-notes-but-no-guide-url
  Scenario: Variant with rotation notes but no guide URL still renders the section
    Given the manifest declares "personal/kimi/api-key" and the Kimi catalog has `rotation.notes` but no `rotation.guide_url`
    When I open the rotation dialog for "personal/kimi/api-key"
    Then the "Rotating this secret:" section still renders
    And it shows the `rotation.notes` block
    And no "Rotation guide" link appears (guide_url is absent)

  @covered-by:no_catalog_match_falls_back_to_manifest_only @covered-by:render_omits_catalog_blocks_when_metadata_is_empty @covered-by:empty_catalog_list_collapses_every_catalog_field
  Scenario: Path without a catalog match falls back to manifest-only rendering
    Given the manifest declares "personal/some-internal-tool/api-key" with no matching catalog
    When I open "devboy secrets ui --gui --provision personal/some-internal-tool/api-key"
    Then the dialog renders PATH and VIA rows
    And no "How to obtain:" header appears
    And no "Note:" footer appears
    And no links row appears (no console_url, no docs_url)

  @covered-by:variant_id_argument_selects_named_variant_over_default @covered-by:variant_id_falls_back_to_first_when_id_not_found
  Scenario: Variant chip selection re-renders the procedure block
    Given the Kimi (Moonshot AI) catalog declares two variants — `kimi-cn` and `kimi-global`
    When I open the provision dialog for "personal/kimi/api-key" and click the `kimi-global` chip
    Then the dialog's description switches to the global variant's wording
    And the steps list re-renders from the global variant's `retrieval.steps`
    And the "Open console ↗" link targets the global variant's `console_url`

  @covered-by:render_provision_shows_console_and_docs_but_not_rotation_block @covered-by:render_includes_description_steps_and_notes_when_catalog_match_present @covered-by:render_provision_does_not_panic_and_writes_chrome_into_test_backend
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

  @covered-by:render_rotation_mode_shows_the_rotation_block @covered-by:metadata_height_is_taller_in_rotation_mode
  Scenario: TUI rotation mode shows the rotation block
    When I open "devboy secrets ui --tui" and start a rotation for "team/openai/api-key"
    Then the metadata block now carries a ROTATION cadence line
    And a "Rotating this secret:" block renders `rotation.notes` and a "guide: ..." line
