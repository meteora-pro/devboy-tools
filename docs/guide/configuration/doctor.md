# Doctor command
`devboy doctor` runs diagnostic checks for your local DevBoy setup and reports problems across config, credentials, provider APIs, MCP tools, and proxy servers.

## Quick start

```bash
# Run full diagnostics
devboy doctor

# List all available check IDs
devboy doctor --list-checks

# Run only selected checks
devboy doctor --checks config.exists,providers.github

# JSON output for scripts/CI
devboy doctor --format json
```

## CLI commands

### Run full diagnostics

```bash
devboy doctor
```

### List available checks

```bash
devboy doctor --list-checks
```

### Run selected checks

```bash
# Comma-delimited
devboy doctor --checks config.exists,config.valid_toml

# Repeated flag
devboy doctor --checks credentials.github --checks providers.github
```

### JSON output

```bash
# Full report in JSON
devboy doctor --format json

# JSON for selected checks
devboy doctor --format json --checks providers.github,providers.gitlab
```

## Options

| Option | Description |
|--------|-------------|
| `--list-checks` | Print all check IDs and exit |
| `--checks <id,...>` | Run only selected checks (comma-delimited or repeated) |
| `--format json` | Output machine-readable JSON |
| `--verbose` | Include more detailed diagnostic output |

## Exit codes

| Exit code | Meaning |
|----------|---------|
| `0` | All checks passed |
| `1` | At least one warning |
| `2` | At least one error |

This makes `devboy doctor` safe to use in CI gates.

## Available checks

### Environment

- `environment.os_support` - Operating system supported
- `environment.config_dir` - Config directory exists
- `environment.credential_store` - Credential store available

### Configuration

- `config.exists` - Config file exists
- `config.valid_toml` - Config file valid TOML
- `config.active_context` - Active context valid

### Credentials

- `credentials.github` - GitHub token available
- `credentials.gitlab` - GitLab token available
- `credentials.clickup` - ClickUp token available
- `credentials.jira` - Jira token available
- `credentials.confluence` - Confluence credential available

### Provider connectivity

- `providers.github` - GitHub API connectivity
- `providers.gitlab` - GitLab API connectivity
- `providers.clickup` - ClickUp API connectivity
- `providers.jira` - Jira API connectivity
- `providers.confluence` - Confluence API connectivity

### MCP and proxy

- `mcp.tools` - Built-in MCP tools configuration
- `proxy.servers` - Proxy MCP server connectivity

## Examples

```bash
# Check only config validity and active context
devboy doctor --checks config.valid_toml,config.active_context

# Check provider API access only
devboy doctor --checks providers.github,providers.gitlab

# Save JSON report
devboy doctor --format json > doctor-report.json
```

## Typical workflow

1. Run `devboy doctor`.
2. Fix reported errors first.
3. Re-run with targeted checks using `--checks`.
4. Add `devboy doctor --format json` in CI if you want automated validation.
