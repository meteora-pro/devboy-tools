ARG RUST_VERSION=1.85
FROM mcr.microsoft.com/devcontainers/rust:1-bookworm

ARG RUST_VERSION

# Toolchain pinning + extra components
RUN rustup toolchain install ${RUST_VERSION} \
 && rustup default ${RUST_VERSION} \
 && rustup component add rustfmt clippy rust-analyzer

# Native build essentials + mold linker (fast incremental linking)
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      build-essential \
      pkg-config \
      libssl-dev \
      mold \
      jq \
      git \
 && rm -rf /var/lib/apt/lists/*

# Use mold as default linker for x86_64-unknown-linux-gnu
RUN mkdir -p /usr/local/cargo \
 && printf '[target.x86_64-unknown-linux-gnu]\nlinker = "clang"\nrustflags = ["-C", "link-arg=-fuse-ld=mold"]\n' \
      > /usr/local/cargo/config.toml \
 && apt-get update && apt-get install -y --no-install-recommends clang \
 && rm -rf /var/lib/apt/lists/*

# Pre-create target dir owned by vscode user (volume mount overlays it)
RUN mkdir -p /workspaces && chown -R vscode:vscode /workspaces /usr/local/cargo
