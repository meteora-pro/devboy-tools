# Environment variables

DevBoy supports environment variables as an alternative to OS keychain for credential storage. This enables seamless use in CI/CD pipelines, containerized environments, and cloud workspaces where keychain access may be unavailable.

## Credential resolution order

When resolving credentials, DevBoy checks sources in this order:

1. **Environment variables** (highest priority)
   - `DEVBOY_{PROVIDER}_TOKEN` (prefixed, explicit)
   - `{PROVIDER}_TOKEN` (unprefixed, fallback)
2. **OS Keychain** (macOS Keychain, Windows Credential Manager, Linux Secret Service)

This means environment variables **always take priority** over keychain values, allowing you to override credentials in CI/CD without modifying your local setup.

## Supported environment variables

### Provider tokens

| Provider | Prefixed Variable | Unprefixed Fallback |
|----------|-------------------|---------------------|
| **GitHub** | `DEVBOY_GITHUB_TOKEN` | `GITHUB_TOKEN` |
| **GitLab** | `DEVBOY_GITLAB_TOKEN` | `GITLAB_TOKEN` |
| **ClickUp** | `DEVBOY_CLICKUP_TOKEN` | `CLICKUP_TOKEN` |
| **Jira** | `DEVBOY_JIRA_TOKEN` | `JIRA_TOKEN` |

### Context-scoped tokens

For multi-context setups, you can set tokens per context:

| Context | Variable |
|---------|----------|
| `dashboard` context, GitHub | `DEVBOY_CONTEXTS_DASHBOARD_GITHUB_TOKEN` |
| `dashboard` context, GitLab | `DEVBOY_CONTEXTS_DASHBOARD_GITLAB_TOKEN` |
| `prod` context, GitHub | `DEVBOY_CONTEXTS_PROD_GITHUB_TOKEN` |

### Proxy server tokens

For upstream MCP server proxies:

| Proxy Name | Variable |
|------------|----------|
| `devboy-cloud` | `DEVBOY_DEVBOY_CLOUD_TOKEN` |
| `my-server` | `DEVBOY_MY_SERVER_TOKEN` |

### Proxy server URLs

You can also define proxy URLs via environment variables (no config file needed):

| Proxy Name | Variable |
|------------|----------|
| `devboy-cloud` | `DEVBOY_DEVBOY_CLOUD_URL` |
| `my-server` | `DEVBOY_MY_SERVER_URL` |

When both URL and TOKEN are set, DevBoy automatically creates the proxy connection without any config file.

## Key to environment variable mapping

DevBoy converts credential keys to environment variable names using these rules:

1. Convert to **UPPERCASE**
2. Replace `.`, `/`, and `-` with `_`
3. Add `DEVBOY_` prefix (checked first)
4. Try without prefix (fallback)

### Examples

| Credential Key | Prefixed Variable | Unprefixed Fallback |
|----------------|-------------------|---------------------|
| `github.token` | `DEVBOY_GITHUB_TOKEN` | `GITHUB_TOKEN` |
| `gitlab.token` | `DEVBOY_GITLAB_TOKEN` | `GITLAB_TOKEN` |
| `contexts.dashboard.github.token` | `DEVBOY_CONTEXTS_DASHBOARD_GITHUB_TOKEN` | `CONTEXTS_DASHBOARD_GITHUB_TOKEN` |
| `devboy-cloud.token` | `DEVBOY_DEVBOY_CLOUD_TOKEN` | `DEVBOY_CLOUD_TOKEN` |

## CI/CD examples

### GitHub Actions

```yaml
name: Code Review
on: [pull_request]

jobs:
  review:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      - name: Run DevBoy MCP
        env:
          # Uses built-in GITHUB_TOKEN (unprefixed fallback)
          GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}
          # Or use explicit prefixed variable
          # DEVBOY_GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}
        run: |
          devboy mcp
```

### GitLab CI

```yaml
stages:
  - review

code-review:
  stage: review
  image: rust:latest
  variables:
    # GitLab CI automatically provides CI_JOB_TOKEN
    DEVBOY_GITLAB_TOKEN: ${CI_JOB_TOKEN}
    # Or use a custom token from CI variables
    # GITLAB_TOKEN: ${GITLAB_API_TOKEN}
  script:
    - devboy issues
    - devboy mrs
```

### Docker

```bash
# Pass tokens as environment variables
docker run \
  -e DEVBOY_GITHUB_TOKEN="$GITHUB_TOKEN" \
  -e DEVBOY_GITLAB_TOKEN="$GITLAB_TOKEN" \
  devboy-tools mcp

# Or use a .env file
docker run --env-file .env devboy-tools mcp
```

### Docker Compose

```yaml
version: '3.8'
services:
  devboy:
    image: devboy-tools
    environment:
      - DEVBOY_GITHUB_TOKEN=${GITHUB_TOKEN}
      - DEVBOY_GITLAB_TOKEN=${GITLAB_TOKEN}
      - DEVBOY_CLICKUP_TOKEN=${CLICKUP_TOKEN}
```

### Kubernetes

```yaml
apiVersion: v1
kind: Pod
metadata:
  name: devboy
spec:
  containers:
    - name: devboy
      image: devboy-tools
      env:
        - name: DEVBOY_GITHUB_TOKEN
          valueFrom:
            secretKeyRef:
              name: devboy-secrets
              key: github-token
        - name: DEVBOY_GITLAB_TOKEN
          valueFrom:
            secretKeyRef:
              name: devboy-secrets
              key: gitlab-token
```

### Claude Code / Claude Desktop

Add to `~/.claude/claude_desktop_config.json` or project `.claude/settings.json`:

```json
{
  "mcpServers": {
    "devboy": {
      "command": "devboy",
      "args": ["mcp"],
      "env": {
        "DEVBOY_SKIP_KEYCHAIN": "1",
        "DEVBOY_NO_CONFIG": "1",
        "DEVBOY_DEVBOY_CLOUD_URL": "https://app.devboy.pro/api/mcp?name=my-project",
        "DEVBOY_DEVBOY_CLOUD_TOKEN": "your-token"
      }
    }
  }
}
```

> **Note:** Use `"command": "devboy"` if installed globally via `npm install -g @devboy-tools/cli`,
> or specify the full path like `"/path/to/devboy"` for local builds.

## Full context configuration via environment variables

You can define entire contexts (including provider settings) purely through environment variables, without any config file. This is useful for CI/CD, Docker, and Kubernetes deployments.

### Pattern

```
DEVBOY_CONTEXTS_{CONTEXT}_{PROVIDER}_{FIELD}
```

### Supported fields

| Provider | Required Fields | Optional Fields |
|----------|-----------------|-----------------|
| **GitHub** | `OWNER`, `REPO` | `BASE_URL` |
| **GitLab** | `PROJECT_ID` | `URL` (default: https://gitlab.com) |
| **ClickUp** | `LIST_ID` | `TEAM_ID` |
| **Jira** | `URL`, `PROJECT_KEY`, `EMAIL` | — |

### Examples

**GitHub context:**
```bash
export DEVBOY_CONTEXTS_PROD_GITHUB_OWNER="company"
export DEVBOY_CONTEXTS_PROD_GITHUB_REPO="production-app"
export DEVBOY_CONTEXTS_PROD_GITHUB_TOKEN="ghp_xxx"
```

**GitLab context:**
```bash
export DEVBOY_CONTEXTS_STAGING_GITLAB_URL="https://gitlab.example.com"
export DEVBOY_CONTEXTS_STAGING_GITLAB_PROJECT_ID="company/staging-app"
export DEVBOY_CONTEXTS_STAGING_GITLAB_TOKEN="glpat-xxx"
```

**ClickUp context:**
```bash
export DEVBOY_CONTEXTS_TASKS_CLICKUP_LIST_ID="abc123"
export DEVBOY_CONTEXTS_TASKS_CLICKUP_TEAM_ID="team456"
export DEVBOY_CONTEXTS_TASKS_CLICKUP_TOKEN="pk_xxx"
```

**Jira context:**
```bash
export DEVBOY_CONTEXTS_JIRA_PROJ_JIRA_URL="https://company.atlassian.net"
export DEVBOY_CONTEXTS_JIRA_PROJ_JIRA_PROJECT_KEY="PROJ"
export DEVBOY_CONTEXTS_JIRA_PROJ_JIRA_EMAIL="user@company.com"
export DEVBOY_CONTEXTS_JIRA_PROJ_JIRA_TOKEN="xxx"
```

**Multiple providers in one context:**
```bash
# Context with both GitHub and GitLab
export DEVBOY_CONTEXTS_MYAPP_GITHUB_OWNER="company"
export DEVBOY_CONTEXTS_MYAPP_GITHUB_REPO="my-app"
export DEVBOY_CONTEXTS_MYAPP_GITHUB_TOKEN="ghp_xxx"
export DEVBOY_CONTEXTS_MYAPP_GITLAB_PROJECT_ID="123"
export DEVBOY_CONTEXTS_MYAPP_GITLAB_TOKEN="glpat-xxx"
```

### Complete env-only setup

Run DevBoy MCP with contexts defined entirely via environment:

```bash
# Disable config and keychain
export DEVBOY_SKIP_KEYCHAIN=1
export DEVBOY_NO_CONFIG=1

# Define context
export DEVBOY_CONTEXTS_PROD_GITHUB_OWNER="company"
export DEVBOY_CONTEXTS_PROD_GITHUB_REPO="app"
export DEVBOY_CONTEXTS_PROD_GITHUB_TOKEN="ghp_xxx"

# Run MCP
devboy mcp
```

Or as a one-liner:

```bash
DEVBOY_SKIP_KEYCHAIN=1 DEVBOY_NO_CONFIG=1 \
  DEVBOY_CONTEXTS_PROD_GITHUB_OWNER="company" \
  DEVBOY_CONTEXTS_PROD_GITHUB_REPO="app" \
  DEVBOY_CONTEXTS_PROD_GITHUB_TOKEN="ghp_xxx" \
  devboy mcp
```

### Context naming

Context names are converted from `UPPERCASE_WITH_UNDERSCORES` to `lowercase-with-dashes`:
- `PROD` → `prod`
- `MY_PROJECT` → `my-project`
- `DEV_TEAM_ALPHA` → `dev-team-alpha`

## Context-scoped tokens (with config file)

When using contexts defined in config file, you can still set tokens per context via environment:

```bash
# Default/global tokens
export DEVBOY_GITHUB_TOKEN="ghp_default_token"
export DEVBOY_GITLAB_TOKEN="glpat_default_token"

# Context-specific tokens (take priority for that context)
export DEVBOY_CONTEXTS_PROD_GITHUB_TOKEN="ghp_prod_token"
export DEVBOY_CONTEXTS_DEV_GITHUB_TOKEN="ghp_dev_token"
```

Resolution for `prod` context:
1. `DEVBOY_CONTEXTS_PROD_GITHUB_TOKEN` (found, used)
2. ~~`DEVBOY_GITHUB_TOKEN`~~ (skipped)
3. ~~Keychain~~ (skipped)

Resolution for `staging` context (no context-specific var):
1. `DEVBOY_CONTEXTS_STAGING_GITHUB_TOKEN` (not found)
2. `DEVBOY_GITHUB_TOKEN` (found, used)
3. ~~Keychain~~ (skipped)

## Special environment variables

### DEVBOY_SKIP_KEYCHAIN

Completely disable keychain access (useful for CI where keychain may hang):

```bash
export DEVBOY_SKIP_KEYCHAIN=1
```

When set to `1` or `true`:
- Disables OS keychain access completely
- Tokens are only read from environment variables
- Write operations go to in-memory storage (not persisted)

### DEVBOY_NO_CONFIG

Skip loading the config file, use only environment variables:

```bash
export DEVBOY_NO_CONFIG=1
```

When set to `1` or `true`:
- Config file (`config.toml`) is not loaded
- All providers and proxies must be defined via environment variables
- Useful for pure env-only deployments (Docker, Kubernetes, CI)

Alternatively, use the `--no-config` CLI flag:

```bash
devboy mcp --no-config
```

### Full env-only mode

For completely config-free operation, combine both:

```bash
export DEVBOY_SKIP_KEYCHAIN=1
export DEVBOY_NO_CONFIG=1
export DEVBOY_DEVBOY_CLOUD_URL="https://app.devboy.pro/api/mcp?name=my-project"
export DEVBOY_DEVBOY_CLOUD_TOKEN="my-token"

devboy mcp
```

Or as a one-liner:

```bash
DEVBOY_SKIP_KEYCHAIN=1 DEVBOY_NO_CONFIG=1 \
  DEVBOY_DEVBOY_CLOUD_URL="https://..." \
  DEVBOY_DEVBOY_CLOUD_TOKEN="..." \
  devboy mcp
```

## Security best practices

1. **Never commit tokens** to version control
2. **Use CI/CD secrets** (GitHub Secrets, GitLab CI Variables, etc.)
3. **Prefer prefixed variables** (`DEVBOY_*`) to avoid conflicts with other tools
4. **Scope variables** to specific jobs/stages when possible
5. **Rotate tokens** regularly
6. **Use short-lived tokens** when available (e.g., `CI_JOB_TOKEN` in GitLab)

## Troubleshooting

### Token not found

If DevBoy can't find your token:

```bash
# Check if environment variable is set
echo $DEVBOY_GITHUB_TOKEN
echo $GITHUB_TOKEN

# Enable debug logging to see resolution
RUST_LOG=debug devboy test github
```

### Keychain vs environment variable priority

Environment variables **always** take priority. If you have a token in both keychain and env var, the env var value is used.

To force using keychain:
```bash
# Unset environment variables
unset DEVBOY_GITHUB_TOKEN
unset GITHUB_TOKEN

devboy test github  # Now uses keychain
```

### CI hangs on keychain access

Some CI environments have a keychain that hangs waiting for user input. Use:

```bash
export DEVBOY_SKIP_KEYCHAIN=1
```

This disables keychain completely and only uses environment variables.
