# Contributing to DevBoy Tools

Thank you for your interest in contributing to DevBoy Tools! This document provides guidelines and instructions for contributing.

## Code of Conduct

By participating in this project, you agree to maintain a respectful and inclusive environment for everyone.

## Getting Started

### Prerequisites

- [Rust](https://rustup.rs/) 1.75 or later
- Git
- A GitLab or GitHub account for testing

### Development Setup

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

## Development Workflow

### Branch Naming

Use descriptive branch names with prefixes:

- `feat/description` - New features
- `fix/description` - Bug fixes
- `docs/description` - Documentation updates
- `refactor/description` - Code refactoring
- `test/description` - Test additions or fixes
- `chore/description` - Maintenance tasks

Example: `feat/add-jira-provider`

### Commit Messages

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

## Code Style

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

### Best Practices

- Write idiomatic Rust code
- Use descriptive variable and function names
- Add documentation comments for public APIs
- Handle errors explicitly (avoid `.unwrap()` in library code)
- Write tests for new functionality

## Testing

### Running Tests

```bash
# Run all tests
cargo test

# Run tests for a specific crate
cargo test -p devboy-core

# Run tests with output
cargo test -- --nocapture
```

### Writing Tests

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

## Pull Request Process

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

## Project Structure

```
devboy-tools/
├── crates/
│   ├── devboy-core/      # Core abstractions
│   │   ├── src/
│   │   │   ├── lib.rs    # Crate entry point
│   │   │   ├── error.rs  # Error types
│   │   │   ├── provider.rs # Provider trait
│   │   │   └── types.rs  # Common types
│   │   └── Cargo.toml
│   ├── devboy-storage/   # Credential storage
│   ├── devboy-gitlab/    # GitLab implementation
│   ├── devboy-github/    # GitHub implementation
│   ├── devboy-mcp/       # MCP server
│   └── devboy-cli/       # CLI binary
├── .github/
│   └── workflows/        # CI/CD pipelines
├── Cargo.toml            # Workspace config
└── README.md
```

### Adding a New Provider

1. Create a new crate: `crates/devboy-{provider}/`
2. Implement the `Provider` trait from `devboy-core`
3. Add integration to `devboy-mcp` and `devboy-cli`
4. Add tests and documentation
5. Update README with new provider info

## Getting Help

- Open an [issue](https://github.com/meteora-pro/devboy-tools/issues) for bugs or feature requests
- Start a [discussion](https://github.com/meteora-pro/devboy-tools/discussions) for questions

## License

By contributing, you agree that your contributions will be licensed under the Apache License 2.0.
