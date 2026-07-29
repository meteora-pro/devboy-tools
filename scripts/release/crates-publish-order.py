#!/usr/bin/env python3
"""Emit publishable workspace crates (publish != false) in dependency order
(deps before dependents), one per line, for `cargo publish`.

Derived from `cargo metadata` so the publish set never drifts from reality:
new crates are picked up automatically, `publish = false` crates (the app
binary devboy-cli, devboy-mcp, the internal secrets plugins) are excluded,
and the order always matches the real dependency graph. See #308 — hardcoded
crate lists / manual "layers" broke every release.
"""
import json
import subprocess
import sys

meta = json.loads(
    subprocess.check_output(["cargo", "metadata", "--format-version", "1"])
)
ws = set(meta["workspace_members"])
publishable = {
    p["name"]
    for p in meta["packages"]
    if p["id"] in ws and p.get("publish") != []
}

# intra-workspace, publishable dependency edges (normal + build, not dev)
deps = {n: set() for n in publishable}
for p in meta["packages"]:
    if p["id"] not in ws or p["name"] not in publishable:
        continue
    for dep in p.get("dependencies", []):
        if dep.get("kind") in (None, "build") and dep["name"] in publishable:
            deps[p["name"]].add(dep["name"])

order, done, stack = [], set(), set()


def visit(n):
    if n in done:
        return
    if n in stack:
        sys.exit(f"dependency cycle involving {n}")
    stack.add(n)
    for d in sorted(deps[n]):
        visit(d)
    stack.discard(n)
    done.add(n)
    order.append(n)


for n in sorted(publishable):
    visit(n)

print("\n".join(order))
