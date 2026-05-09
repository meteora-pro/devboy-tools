#!/usr/bin/env python3
"""
Sample SecretSource plugin: echo-source.

Implements the devboy-tools subprocess plugin protocol from
ADR-021 §10. Every `secret_source.get` call returns
`value = "echo:<reference>"` so the framework's wiring can be
tested end-to-end without a real backend.

Wire-format reference: docs/guide/secrets/source-plugin-protocol.md
Host-side supervisor:   crates/devboy-storage/src/plugin_client.rs
"""

from __future__ import annotations

import json
import sys
from typing import Any

PROTOCOL_VERSION = "1.0"
PLUGIN_VERSION = "0.1.0"
PLUGIN_NAME_DEFAULT = "echo"

# Capability bits — see crates/devboy-storage/src/source.rs::Capabilities.
# This plugin claims READ + LIST + VALIDATE.
CAP_READ = 0b0000_0001
CAP_LIST = 0b0000_0010
CAP_VALIDATE = 0b0000_0100
CAPABILITIES = CAP_READ | CAP_LIST | CAP_VALIDATE


def reply_result(req_id: int, result: dict[str, Any]) -> dict[str, Any]:
    return {"jsonrpc": "2.0", "id": req_id, "result": result}


def reply_error(req_id: int, kind: str, **fields: Any) -> dict[str, Any]:
    return {"jsonrpc": "2.0", "id": req_id, "error": {"kind": kind, **fields}}


def handle(req: dict[str, Any], state: dict[str, Any]) -> dict[str, Any]:
    """Dispatch one JSON-RPC request and return the response object."""
    req_id = req.get("id")
    method = req.get("method", "")
    params = req.get("params") or {}

    if not isinstance(req_id, int):
        # No id ⇒ can't reply per JSON-RPC; treat as a soft error.
        return reply_error(0, "other", detail="request missing integer id")

    if method == "secret_source.init":
        proto = params.get("protocol_version", "")
        if not proto.startswith("1."):
            return reply_error(
                req_id,
                "other",
                detail=f"unsupported protocol_version: {proto!r}",
            )
        state["source_name"] = params.get("source_name") or PLUGIN_NAME_DEFAULT
        state["initialized"] = True
        return reply_result(
            req_id,
            {
                "source_name": state["source_name"],
                "capabilities_bits": CAPABILITIES,
                "plugin_version": PLUGIN_VERSION,
            },
        )

    if not state.get("initialized"):
        return reply_error(req_id, "other", detail="init must be the first call")

    if method == "secret_source.is_available":
        return reply_result(req_id, {"status": "available", "detail": None})

    if method == "secret_source.get":
        reference = params.get("reference", "")
        if not isinstance(reference, str) or not reference:
            return reply_error(
                req_id,
                "bad-reference",
                reference=reference,
                reason="reference must be a non-empty string",
            )
        # Echo the reference back as the value. Useful for smoke
        # tests; in a real plugin, dispatch to the backend here.
        return reply_result(
            req_id,
            {"value": f"echo:{reference}", "lease_seconds": None},
        )

    if method == "secret_source.list":
        # Static demo inventory — three references the plugin
        # would happily resolve via `get`.
        entries = [
            {
                "reference": "demo/team/echo/api-key",
                "display": "Demo team API key",
            },
            {
                "reference": "demo/personal/echo/feature-flag",
                "display": None,
            },
            {
                "reference": "demo/sandbox/echo/local-token",
                "display": "Sandbox local token",
            },
        ]
        return reply_result(req_id, {"entries": entries})

    if method == "secret_source.validate":
        reference = params.get("reference", "")
        if not isinstance(reference, str) or not reference:
            return reply_error(
                req_id,
                "bad-reference",
                reference=reference,
                reason="reference must be a non-empty string",
            )
        return reply_result(req_id, {"ok": True})

    return reply_error(
        req_id,
        "unsupported-capability",
        capability=method.replace("secret_source.", ""),
    )


def main() -> int:
    state: dict[str, Any] = {"initialized": False}
    for raw in sys.stdin:
        line = raw.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
        except json.JSONDecodeError as exc:
            sys.stdout.write(
                json.dumps(reply_error(0, "other", detail=f"invalid json: {exc}"))
                + "\n"
            )
            sys.stdout.flush()
            continue

        resp = handle(req, state)
        sys.stdout.write(json.dumps(resp) + "\n")
        sys.stdout.flush()
    return 0


if __name__ == "__main__":
    sys.exit(main())
