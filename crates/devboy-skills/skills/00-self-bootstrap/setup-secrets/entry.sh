#!/usr/bin/env bash
# entry.sh — initialise (or resume) the setup-secrets state file.
#
# This is the bootstrap helper invoked by the `setup-secrets` skill
# at step 0. It keeps the wizard idempotent: a fresh run creates
# `~/.devboy/secrets/setup-state.toml`; a resumed run prints the
# first step that is still pending so the agent jumps straight to it.
#
# The script never reads or writes secret values — it only touches
# wizard state metadata. See ADR-023 §3.8 (the eight-step flow) and
# the SKILL.md beside this script for the full procedure.

set -euo pipefail

state_dir="${DEVBOY_SECRETS_HOME:-$HOME/.devboy/secrets}"
state_file="$state_dir/setup-state.toml"

mkdir -p "$state_dir"

# Step ids must match SKILL.md exactly. The wizard reads `last_step`
# and the per-step `status` to decide where to resume.
steps=(
  vault_state
  create_vault
  touch_id
  routing
  required
  optional
  validation
  doctor
)

if [ ! -f "$state_file" ]; then
  started_at="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
  {
    printf 'schema_version = 1\n'
    printf 'started_at = "%s"\n' "$started_at"
    printf 'last_step = 0\n'
    for s in "${steps[@]}"; do
      printf '\n[steps.%s]\nstatus = "pending"\n' "$s"
    done
  } > "$state_file"
  printf 'created %s\n' "$state_file"
  printf 'next: 1\n'
  exit 0
fi

# Resume: print the first non-done / non-skipped step (1-indexed).
# Default to step 1 when nothing matches — the wizard re-runs the
# entire flow safely thanks to per-step idempotence.
next_step=1
i=1
for s in "${steps[@]}"; do
  status="$(awk -v section="[steps.$s]" '
    $0 == section { found = 1; next }
    found && /^\[/  { exit }
    found && /^status[[:space:]]*=/ {
      # Extract the quoted value: split on `"` and take field 2.
      n = split($0, parts, "\"")
      if (n >= 2) print parts[2]
      exit
    }' "$state_file")"
  if [ "$status" != "done" ] && [ "$status" != "skipped" ]; then
    next_step=$i
    break
  fi
  i=$((i + 1))
  if [ $i -gt ${#steps[@]} ]; then
    # Every step already settled — wizard is complete.
    next_step="complete"
  fi
done

printf 'state %s\n' "$state_file"
printf 'next: %s\n' "$next_step"
