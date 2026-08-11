# language: en
Feature: Catalog URL source lifecycle (subscribe → refresh → recover)
  As a team lead who maintains a shared `secret-tokens-catalog` Git repo
  I want every developer to subscribe to the canonical catalogs by URL
  and recover from upstream rotations without hand-editing config
  So that knowledge of "where do I get OPENAI_API_KEY" stays in one place
  and any provider procedure update reaches every machine on next refresh

  Background:
    Given devboy 0.29.0 is installed with the URL-loaded catalog feature
    And no "~/.devboy/secrets/catalog/sources.toml" exists yet

  @covered-by:fetch_with_client_happy_path @covered-by:strict_first_fetch_returns_confirmation_error_then_record_lets_it_through
  Scenario: Subscribe to a remote catalog via add-url
    When I run "devboy secrets catalog add-url https://raw.githubusercontent.com/team/catalog/main/anthropic.json --enable --yes"
    Then the loader fetches once with all P23 defence layers (HTTPS-only, SSRF guard, size cap, content-type, schema-version)
    And the body SHA256 is printed for trust verification
    And "~/.devboy/secrets/catalog/sources.toml" is created with one [[source]] entry
    And "enable_url_catalogs = true" is set

  @covered-by:add_url_rejects_non_https_scheme @covered-by:fetch_rejects_http_scheme_directly
  Scenario: HTTPS-only enforcement rejects an http URL outright
    When I run "devboy secrets catalog add-url http://example.invalid/anthropic.json --yes"
    Then the command exits non-zero
    And the error message reads "URL must start with `https://`"
    And NO network request is made

  @covered-by:status_json_lists_bundled_catalogs_by_default @covered-by:status_human_output_lists_header_row
  Scenario: Status command surfaces every active catalog with origin
    Given the bundled catalogs are loaded
    And one URL source has been added and enabled
    When I run "devboy secrets catalog status"
    Then a table is printed with columns "provider | origin | variants | patterns | skip | source"
    And every bundled catalog appears with origin "bundled" and source "(in-binary)"
    And the URL-loaded catalog appears with origin "url" and source "<url> [tofu]" or "<url> [pin:<sha8>…]"

  @covered-by:tofu_rejects_when_known_hash_changes
  Scenario: TOFU mismatch after upstream rotation
    Given a URL source has been recorded under TOFU with sha256 = "<old-sha>"
    And the upstream now serves a body with sha256 = "<new-sha>"
    When I run "devboy secrets catalog refresh"
    Then the refresh fails with BlockedTofuMismatch
    And the printed error names both the URL and the recorded "<old-sha>"

  @covered-by:forget_drops_only_matching_url_when_filter_given @covered-by:tofu_records_on_first_fetch_and_accepts_on_second
  Scenario: Recover from TOFU mismatch via forget
    Given a TOFU mismatch was just refused
    When I run "devboy secrets catalog forget anthropic"
    Then the entry for the matching URL is dropped from "~/.devboy/secrets/catalog/known_hashes.toml"
    When I run "devboy secrets catalog refresh"
    Then the refresh succeeds, recording the new sha256 under TOFU

  @covered-by:pin_copies_tofu_sha_when_no_explicit_sha_given @covered-by:pinned_sha_mismatch_rejects @covered-by:audit_log_records_blocked_pin_event
  Scenario: Promote TOFU to a hard pin
    Given a URL source has a TOFU entry with sha256 = "ba7816bf…"
    When I run "devboy secrets catalog pin anthropic"
    Then "sources.toml" is rewritten with `sha256 = "ba7816bf…"` on the matching [[source]]
    When the upstream rotates and the body sha changes
    Then the next refresh fails with BlockedPin (not TOFU prompt) — the operator must explicitly pin again
