# Installation

This guide covers various ways to install DevBoy Tools on your system.

## Prerequisites

- **Rust** 1.75 or later (for building from source)
- **Git** (for cloning the repository)
- A **GitHub**, **GitLab**, **Jira**, or **ClickUp** account (for API access)

## Installation Methods

### From Source (Recommended for Development)

Building from source gives you the latest features and is recommended for development.

```bash
# Clone the repository
git clone https://github.com/meteora-pro/devboy-tools.git
cd devboy-tools

# Build the project
cargo build --release

# The binary will be available at:
# ./target/release/devboy
```

### From Pre-built Binaries

Download pre-built binaries from the [Releases](https://github.com/meteora-pro/devboy-tools/releases) page.

**macOS:**
```bash
# Download the latest release for your architecture
curl -L -o devboy.tar.gz https://github.com/meteora-pro/devboy-tools/releases/latest/download/devboy-darwin-arm64.tar.gz

# Extract
tar -xzf devboy.tar.gz

# Move to a directory in your PATH
sudo mv devboy /usr/local/bin/
```

**Linux:**
```bash
# Download for x86_64
curl -L -o devboy.tar.gz https://github.com/meteora-pro/devboy-tools/releases/latest/download/devboy-linux-x64.tar.gz

# Extract
tar -xzf devboy.tar.gz

# Move to a directory in your PATH
sudo mv devboy /usr/local/bin/
```

**Windows:**
1. Download `devboy-windows-x64.zip` from releases
2. Extract the ZIP file
3. Add the directory to your PATH environment variable

## Verify Installation

After installation, verify that DevBoy is working:

```bash
devboy --help
```

You should see the help output with available commands.

## Next Steps

Now that you have DevBoy installed, proceed to the [Quick Start](/getting-started/quick-start) guide to configure your first integration.
