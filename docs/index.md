# DevBoy Tools

Open Source MCP server written in Rust. Designed for coding agents with plugin system (API providers + LLM-optimized pipeline) and multi-project context switching.

## Why DevBoy?

| | Others | DevBoy |
|-|--------|--------|
| **Privacy** | Cloud-based credentials | Local OS keychain |
| **Focus** | All projects at once | Context-based project isolation |
| **Tools** | Generic aggregators | Purpose-built for dev workflows |

## Features

- **Secure Credential Storage**: Tokens stored in OS keychain (macOS Keychain, Windows Credential Manager, Linux Secret Service)
- **GitHub Integration**: Full support for issues, pull requests, and code review
- **GitLab Integration**: Full support for issues, merge requests, and code review (including self-hosted instances)
- **ClickUp Integration**: Task management — create, update, and track tasks through AI assistants
- **Jira Integration**: Issue tracking for Jira Cloud (API v3) and Self-Hosted/Data Center (API v2)
- **Multi-Project Contexts**: Manage multiple projects in one config and [switch between them](/configuration/contexts) on the fly
- **MCP Protocol**: Native Model Context Protocol support for AI assistants

## Quick Start

1. [Install](/getting-started/) DevBoy Tools
2. Configure with [GitHub](/integrations/github), [GitLab](/integrations/gitlab), [ClickUp](/integrations/clickup), or [Jira](/integrations/jira)
3. Connect to your [AI assistant](/getting-started/quick-start#step-5-integrate-with-ai-assistants)

## Next Steps

- [Installation Guide](/getting-started/) - Detailed installation instructions
- [Quick Start](/getting-started/quick-start) - Get up and running in minutes
- [Configuration](/configuration/) - Config files, secrets, and multi-project contexts
- [GitHub Integration](/integrations/github) - Configure GitHub access
- [GitLab Integration](/integrations/gitlab) - Configure GitLab access
- [ClickUp Integration](/integrations/clickup) - Configure ClickUp access
- [Jira Integration](/integrations/jira) - Configure Jira access

## Related Projects

- [devboy-tools-agent-usage](https://github.com/meteora-pro/devboy-tools-agent-usage) — CLI tool for analyzing AI agent usage (Claude Code): cost, time, tasks, focus
