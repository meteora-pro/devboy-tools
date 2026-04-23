# Paper 1 reproduction environment — hermetic & pinned.
#
# Build:
#   docker build -t paper1-repro -f Dockerfile ../../../..
# (build context = repo root so scripts can reference other project files)
#
# Run:
#   docker run --rm -it \
#       -v $(pwd)/.env:/work/docs/research/paper1-repro/.env:ro \
#       -v paper1-artifacts:/work/docs/research/paper1-repro/artifacts \
#       --network=host \
#       paper1-repro make repro-cloud-only
#
# `--network=host` is needed only if Ollama is local to the Docker host;
# for cloud-only runs (Anthropic / z.ai) omit it.

FROM python:3.12.10-slim-bookworm

# Pinned OS packages
RUN apt-get update && apt-get install -y --no-install-recommends \
        git=1:2.39.5-0+deb12u2 \
        curl=7.88.1-10+deb12u13 \
        ca-certificates \
        make \
        ripgrep \
    && rm -rf /var/lib/apt/lists/*

# uv is our Python script runner (PEP 723 inline deps)
RUN pip install --no-cache-dir uv==0.11.7

WORKDIR /work

# Copy only the paper-specific bits + the few other files they depend on
COPY docs/research/paper1-repro/ ./docs/research/paper1-repro/
COPY docs/research/scripts/find_gold_selection.py ./docs/research/scripts/find_gold_selection.py
COPY .gitignore ./.gitignore

# Pre-warm uv cache — this downloads all Python deps listed in PEP 723
# headers of 01-07 scripts. Makes subsequent `make repro` fast.
RUN cd docs/research/paper1-repro \
    && python -m uv run --help > /dev/null 2>&1 \
    && for s in scripts/0*.py; do \
         python -m uv run --quiet "$s" --help > /dev/null 2>&1 || true; \
       done

WORKDIR /work/docs/research/paper1-repro

# Default: show available targets
CMD ["make", "help"]
