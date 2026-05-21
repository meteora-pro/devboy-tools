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

  Scenario: Subscribe to a remote catalog via add-url
    When I run "devboy secrets catalog add-url https://raw.githubusercontent.com/team/catalog/main/anthropic.json --enable --yes"
    Then the loader fetches once with all P23 defence layers (HTTPS-only, SSRF guard, size cap, content-type, schema-version)
    And the body SHA256 is printed for trust verification
    And "~/.devboy/secrets/catalog/sources.toml" is created with one [[source]] entry
    And "enable_url_catalogs = true" is set

  Scenario: HTTPS-only enforcement rejects an http URL outright
    When I run "devboy secrets catalog add-url http://example.invalid/anthropic.json --yes"
    Then the command exits non-zero
    And the error message reads "URL must start with `https://`"
    And NO network request is made

  Scenario: Status command surfaces every active catalog with origin
    Given the bundled catalogs are loaded
    And one URL source has been added and enabled
    When I run "devboy secrets catalog status"
    Then a table is printed with columns "provider | origin | variants | patterns | skip | source"
    And every bundled catalog appears with origin "bundled" and source "(in-binary)"
    And the URL-loaded catalog appears with origin "url" and source "<url> [tofu]" or "<url> [pin:<sha8>…]"

  Scenario: TOFU mismatch after upstream rotation
    Given a URL source has been recorded under TOFU with sha256 = "<old-sha>"
    And the upstream now serves a body with sha256 = "<new-sha>"
    When I run "devboy secrets catalog refresh"
    Then the refresh fails with BlockedTofuMismatch
    And the printed error names both the URL and the recorded "<old-sha>"

  Scenario: Recover from TOFU mismatch via forget
    Given a TOFU mismatch was just refused
    When I run "devboy secrets catalog forget anthropic"
    Then the entry for the matching URL is dropped from "~/.devboy/secrets/catalog/known_hashes.toml"
    When I run "devboy secrets catalog refresh"
    Then the refresh succeeds, recording the new sha256 under TOFU

  Scenario: Promote TOFU to a hard pin
    Given a URL source has a TOFU entry with sha256 = "ba7816bf…"
    When I run "devboy secrets catalog pin anthropic"
    Then "sources.toml" is rewritten with `sha256 = "ba7816bf…"` on the matching [[source]]
    When the upstream rotates and the body sha changes
    Then the next refresh fails with BlockedPin (not TOFU prompt) — the operator must explicitly pin again
