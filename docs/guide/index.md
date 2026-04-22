---
pageType: home

hero:
  name: DevBoy tools
  text: Configurable Tool Bundle for AI Coding Agents
  tagline: Open Source. Written in Rust. Use the same tools via MCP, CLI, or agent skills. Privacy-first, with multi-project context switching.
  actions:
    - theme: brand
      text: Quick start
      link: /getting-started/quick-start
    - theme: alt
      text: GitHub
      link: https://github.com/meteora-pro/devboy-tools

features:
  - title: Secure credentials
    details: Tokens stored in OS keychain (macOS Keychain, Windows Credential Manager, Linux Secret Service). Never in plain text.
    icon: 🔐
  - title: GitHub & GitLab
    details: Full support for issues, PRs/MRs, code review, diffs, and inline comments. Self-hosted GitLab included.
    icon: 🔀
  - title: ClickUp & Jira
    details: Task management through AI assistants. Jira Cloud (API v3) and Self-Hosted (API v2) supported.
    icon: 📋
  - title: Multi-project contexts
    details: Switch between projects on the fly. Each context has its own providers, tokens, and settings.
    icon: 🔄
  - title: Multiple integration modes
    details: Same tool bundle, three ways to call it — as an MCP server (Claude Code, Claude Desktop, any MCP client), as a CLI for humans and CI, or directly from agent skills via `devboy tools call`. Proxy upstream MCP servers to combine tools into a single endpoint.
    icon: 🤖
  - title: Built with Rust
    details: Fast, reliable, and cross-platform. Single binary, no runtime dependencies.
    icon: ⚡
---
