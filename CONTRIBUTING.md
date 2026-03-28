# Contributing to DevBoy tools

Thank you for your interest in contributing to DevBoy tools! This document provides guidelines and instructions for contributing.

## Code of Conduct

By participating in this project, you agree to maintain a respectful and inclusive environment for everyone.

## Getting started

### Prerequisites

- [Rust](https://rustup.rs/) 1.75 or later
- Git
- A GitLab or GitHub account for testing

### Development setup

1. **Fork and clone the repository**

   ```bash
   git clone https://github.com/YOUR_USERNAME/devboy-tools.git
   cd devboy-tools
   ```

2. **Build the project**

   ```bash
   cargo build
   ```

3. **Run tests**

   ```bash
   cargo test
   ```

4. **Run the CLI**

   ```bash
   cargo run -- --help
   ```

## Development workflow

### Branch naming

Use descriptive branch names with prefixes:

- `feat/description` - New features
- `fix/description` - Bug fixes
- `docs/description` - Documentation updates
- `refactor/description` - Code refactoring
- `test/description` - Test additions or fixes
- `chore/description` - Maintenance tasks

Example: `feat/add-jira-provider`

### Commit messages

Follow [Conventional Commits](https://www.conventionalcommits.org/):

```
type(scope): description

[optional body]

[optional footer]
```

Types:
- `feat` - New feature
- `fix` - Bug fix
- `docs` - Documentation
- `style` - Formatting (no code change)
- `refactor` - Code refactoring
- `test` - Adding tests
- `chore` - Maintenance

Examples:
```
feat(gitlab): add support for issue labels
fix(storage): handle missing keychain on Linux
docs: update installation instructions
```

## Code style

### Formatting

All code must be formatted with `rustfmt`:

```bash
cargo fmt --all
```

### Linting

Code must pass `clippy` without warnings:

```bash
cargo clippy --all-targets --all-features
```

### Best practices

- Write idiomatic Rust code
- Use descriptive variable and function names
- Add documentation comments for public APIs
- Handle errors explicitly (avoid `.unwrap()` in library code)
- Write tests for new functionality

## Testing

### Running tests

```bash
# Run all tests
cargo test

# Run tests for a specific crate
cargo test -p devboy-core

# Run tests with output
cargo test -- --nocapture
```

### Writing tests

- Place unit tests in the same file using `#[cfg(test)]` module
- Place integration tests in `tests/` directory
- Use descriptive test names: `test_get_issues_returns_open_issues`

Example:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_display() {
        let error = Error::Auth("Invalid token".to_string());
        assert!(error.to_string().contains("Invalid token"));
    }
}
```

## Pull request process

1. **Create a feature branch** from the latest `master`

   ```bash
   git checkout master
   git pull origin master
   git checkout -b feat/your-feature
   ```

2. **Make your changes** with appropriate commits

3. **Ensure all checks pass**

   ```bash
   cargo fmt --all --check
   cargo clippy --all-targets --all-features
   cargo test
   ```

4. **Push your branch**

   ```bash
   git push -u origin feat/your-feature
   ```

5. **Create a Pull Request**

   - Provide a clear description of changes
   - Reference related issues (e.g., "Closes #123")
   - Ensure CI passes

6. **Address review feedback**

   - Make requested changes
   - Push additional commits
   - Re-request review when ready

## Project structure

```
devboy-tools/
├── crates/
│   ├── devboy-core/          # Core abstractions (Provider, ToolEnricher traits, types)
│   │   └── src/
│   │       ├── provider.rs   # IssueProvider, MergeRequestProvider, Provider traits
│   │       ├── enricher.rs   # ToolEnricher trait, ToolSchema utilities
│   │       ├── types.rs      # Unified types (Issue, MergeRequest, Discussion, etc.)
│   │       ├── config.rs     # Configuration management
│   │       └── error.rs      # Error types
│   ├── devboy-executor/      # Tool execution engine + enrichment pipeline
│   │   └── src/
│   │       ├── executor.rs   # Executor (dispatch tools, run enrichers)
│   │       ├── context.rs    # AdditionalContext, ProviderConfig, scopes
│   │       ├── factory.rs    # create_provider(), create_enricher()
│   │       ├── enricher.rs   # Built-in enrichers (PipelineFormatEnricher)
│   │       ├── output.rs     # ToolOutput typed enum
│   │       └── format.rs     # ToolOutput → pipeline text formatting
│   ├── devboy-storage/       # Credential storage (keychain, env vars)
│   ├── devboy-mcp/           # MCP server (JSON-RPC over stdio)
│   ├── devboy-cli/           # CLI binary
│   └── plugins/
│       ├── api/
│       │   ├── gitlab/       # GitLab client + GitLabSchemaEnricher
│       │   ├── github/       # GitHub client + GitHubSchemaEnricher
│       │   ├── clickup/      # ClickUp client + enricher + metadata types
│       │   └── jira/         # Jira client + enricher + metadata types
│       └── pipeline/         # Output formatting (markdown, truncation)
├── .github/
│   └── workflows/            # CI/CD pipelines
├── Cargo.toml                # Workspace config
└── README.md
```

### Adding a New Provider

1. Create a new crate: `crates/plugins/api/{provider}/`
2. Implement the `Provider` trait from `devboy-core` (client + types)
3. Implement `ToolEnricher` from `devboy-core` (schema enricher)
4. Add provider to `devboy-executor/src/factory.rs` (create_provider + create_enricher)
5. Add tests and documentation
6. Update README with new provider info

### Adding an Enricher

Enrichers implement the `ToolEnricher` trait from `devboy-core`:

```rust
use devboy_core::{ToolEnricher, ToolSchema};

pub struct MyEnricher;

impl ToolEnricher for MyEnricher {
    fn supported_tools(&self) -> &[&str] {
        &["get_issues", "create_issue"]
    }

    fn enrich_schema(&self, tool_name: &str, schema: &mut ToolSchema) {
        // Add enum params, remove unsupported params, etc.
        schema.add_enum_param("status", &["open", "closed"], "Issue status");
    }

    fn transform_args(&self, tool_name: &str, args: &mut serde_json::Value) {
        // Transform args before provider call (e.g., cf_* → customFields)
    }
}
```

Register enrichers with the Executor:

```rust
let mut executor = Executor::new();
executor.add_enricher(Box::new(MyEnricher));
```

See existing enrichers in `crates/plugins/api/*/src/enricher.rs` for examples.

## Getting help

- Open an [issue](https://github.com/meteora-pro/devboy-tools/issues) for bugs or feature requests
- Start a [discussion](https://github.com/meteora-pro/devboy-tools/discussions) for questions

## License

By contributing, you agree that your contributions will be licensed under the Apache License 2.0.
