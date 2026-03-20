# Environment variables

DevBoy supports environment variables as an alternative to OS keychain for credential storage. This enables seamless use in CI/CD pipelines, containerized environments, and cloud workspaces where keychain access may be unavailable.

## Credential Resolution Order

When resolving credentials, DevBoy checks sources in this order:

1. **Environment variables** (highest priority)
   - `DEVBOY_{PROVIDER}_TOKEN` (prefixed, explicit)
   - `{PROVIDER}_TOKEN` (unprefixed, fallback)
2. **OS Keychain** (macOS Keychain, Windows Credential Manager, Linux Secret Service)

This means environment variables **always take priority** over keychain values, allowing you to override credentials in CI/CD without modifying your local setup.

## Supported Environment Variables

### Provider Tokens

| Provider | Prefixed Variable | Unprefixed Fallback |
|----------|-------------------|---------------------|
| **GitHub** | `DEVBOY_GITHUB_TOKEN` | `GITHUB_TOKEN` |
| **GitLab** | `DEVBOY_GITLAB_TOKEN` | `GITLAB_TOKEN` |
| **ClickUp** | `DEVBOY_CLICKUP_TOKEN` | `CLICKUP_TOKEN` |
| **Jira** | `DEVBOY_JIRA_TOKEN` | `JIRA_TOKEN` |

### Context-Scoped Tokens

For multi-context setups, you can set tokens per context:

| Context | Variable |
|---------|----------|
| `dashboard` context, GitHub | `DEVBOY_CONTEXTS_DASHBOARD_GITHUB_TOKEN` |
| `dashboard` context, GitLab | `DEVBOY_CONTEXTS_DASHBOARD_GITLAB_TOKEN` |
| `prod` context, GitHub | `DEVBOY_CONTEXTS_PROD_GITHUB_TOKEN` |

### Proxy Server Tokens

For upstream MCP server proxies:

| Proxy Name | Variable |
|------------|----------|
| `devboy-cloud` | `DEVBOY_DEVBOY_CLOUD_TOKEN` |
| `my-server` | `DEVBOY_MY_SERVER_TOKEN` |

## Key to Environment Variable Mapping

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

## CI/CD Examples

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

## Multiple Contexts with Environment Variables

When using multiple contexts, you can set different tokens for each:

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

## Special Environment Variables

### DEVBOY_SKIP_KEYCHAIN

Completely disable keychain access (useful for CI where keychain may hang):

```bash
export DEVBOY_SKIP_KEYCHAIN=1
```

When set to `1` or `true`:
- `devboy init` uses in-memory storage instead of keychain
- Tokens are only read from environment variables
- Write operations (`set-secret`) go to memory (not persisted)

## Security Best Practices

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

### Keychain vs Environment Variable Priority

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
