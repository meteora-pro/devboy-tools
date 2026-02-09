# DevBoy Tools

Open Source MCP server written in Rust. Designed for coding agents with plugin system (API providers + LLM-optimized pipeline) and project-scoped isolation (1 server = 1 project).

## Why DevBoy?

| | Others | DevBoy |
|-|--------|--------|
| **Privacy** | Cloud-based credentials | Local OS keychain |
| **Focus** | All projects at once | 1 server = 1 project (intentional) |
| **Tools** | Generic aggregators | Purpose-built for dev workflows |

## Features

- **Secure Credential Storage**: Tokens stored in OS keychain (macOS Keychain, Windows Credential Manager, Linux Secret Service)
- **GitHub Integration**: Full support for issues, pull requests, and code review
- **GitLab Integration**: Support for merge requests and project management
- **Issue Tracker Support**: ClickUp and Jira integrations (coming soon)
- **MCP Protocol**: Native Model Context Protocol support for AI assistants

## Quick Start

1. [Install](/getting-started/) DevBoy Tools
2. Configure with [GitHub](/integrations/github)
3. Connect to your [AI assistant](/getting-started/quick-start#step-5-integrate-with-ai-assistants)

## Next Steps

- [Installation Guide](/getting-started/) - Detailed installation instructions
- [Quick Start](/getting-started/quick-start) - Get up and running in minutes
- [GitHub Integration](/integrations/github) - Configure GitHub access
