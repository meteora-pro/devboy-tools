# language: en
Feature: setup-secrets wizard onboards a fresh project
  As a developer who just cloned a project that depends on devboy-tools
  I want a single command to scan my repo, propose ADR-020 paths, and
  walk me through provisioning the missing values
  So that I do not have to author the manifest by hand or copy
  credentials between my keychain and the project

  Background:
    Given devboy 0.29.0 is installed
    And the project at "<repo>" has a "<repo>/.env.example" listing real credentials
    And the project has no "<repo>/.devboy/secrets.toml"

  @covered-by:handle_cli_scan_only_succeeds_against_seeded_project @covered-by:proposer_openai_api_key_known_provider @covered-by:proposer_skips_configuration_suffixes
  Scenario: Read-only preview shows the proposer's plan
    When I run "devboy secrets setup --scan-only --root <repo>"
    Then the command exits 0
    And the output reports the scanned-files / env-var-references / proposed-paths counts
    And every "OPENAI_API_KEY"-style real credential appears as a "team/openai/api-key" path
    And configuration vars matching "<PROVIDER>_BASE_URL" / "<PROVIDER>_MODEL" are listed as Skip with a clear reason
    And no file under "<repo>/.devboy/" is created

  @covered-by:handle_cli_write_manifest_creates_secrets_toml_in_dot_devboy @covered-by:handle_cli_write_manifest_against_empty_project_writes_a_header_only_file
  Scenario: Writing the manifest commits proposals to disk
    When I run "devboy secrets setup --root <repo> --write-manifest"
    Then "<repo>/.devboy/secrets.toml" is created
    And the file contains a "required = [...]" block listing every Path proposal
    And the file contains a "[secret.\"<path>\"]" block per Path with description tracing back to the env var

  @covered-by:wizard_resume_skips_done_phases_on_second_run @covered-by:setup_state_partial_run_records_progress_only_up_to_failure @covered-by:wizard_state_file_written_with_done_statuses
  Scenario: Re-running with --resume picks up where the wizard left off
    Given a previous wizard run got as far as Phase 3 (Provision) and recorded "scan" + "propose" as done
    When I run "devboy secrets setup --resume --root <repo>"
    Then the wizard prints "resuming at phase 3/4"
    And Scan and Propose phases emit "Skipped — already done"
    And only the Provision and Doctor phases re-run

  @covered-by:proposer_picks_catalog_pattern_over_heuristic @covered-by:proposer_catalog_skip_outranks_path_proposal @covered-by:proposer_skips_ambiguous_generic_segments
  Scenario: Catalog-driven proposer accuracy on a real project
    Given the bundled token catalogs are loaded
    When I run "devboy secrets setup --scan-only --root meteora/devboy-env-1"
    Then the proposed-paths count is at most 175
    And "ANTHROPIC_API_KEY" maps to "team/anthropic/anthropic-api-key" (provider_known=true)
    And "ANTHROPIC_BASE_URL" is dropped with reason "skipped by catalog `anthropic` env_var_skip pattern `ANTHROPIC_BASE_URL`"
    And "SERVICE_TOKEN" is dropped with the "ambiguous provider segment" reason
