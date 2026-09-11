#!/usr/bin/env python3
"""Fake stdio MCP server for cosmo-mcp integration tests.

Speaks the MCP stdio framing: newline-delimited JSON-RPC 2.0. Advertises a
fixed tool list with annotations, including a `run_shell` tool that must
never be registered by the host, and one tool not on the allowlist.
"""

import json
import sys

TOOLS = [
    {
        "name": "list_windows",
        "description": "List windows",
        "inputSchema": {"type": "object", "properties": {}},
        "annotations": {"readOnlyHint": True, "destructiveHint": False},
    },
    {
        "name": "click",
        "description": "Click a point",
        "inputSchema": {
            "type": "object",
            "properties": {
                "x": {"type": "integer", "description": "X in pixels"},
                "y": {"type": "integer", "description": "Y in pixels"},
            },
            "required": ["x", "y"],
        },
        "annotations": {"readOnlyHint": False, "destructiveHint": True},
    },
    {
        # No `annotations` key at all: the host must default this to
        # destructive (MCP's own default) so the gate holds it.
        "name": "press_key",
        "description": "Press a key (deliberately unannotated)",
        "inputSchema": {"type": "object", "properties": {"key": {"type": "string"}}},
    },
    {
        "name": "screenshot",
        "description": "Take a screenshot",
        "inputSchema": {"type": "object", "properties": {}},
        "annotations": {"readOnlyHint": False, "destructiveHint": False},
    },
    {
        "name": "run_shell",
        "description": "Run a shell command (MUST be refused by the host)",
        "inputSchema": {"type": "object", "properties": {}},
        "annotations": {"readOnlyHint": True, "destructiveHint": False},
    },
    {
        "name": "not_in_allowlist",
        "description": "Not on cosmo's allowlist",
        "inputSchema": {"type": "object", "properties": {}},
        "annotations": {"readOnlyHint": True, "destructiveHint": False},
    },
]


def reply(msg_id, result):
    print(json.dumps({"jsonrpc": "2.0", "id": msg_id, "result": result}), flush=True)


for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    req = json.loads(line)
    method = req.get("method")
    msg_id = req.get("id")
    if method == "initialize":
        reply(
            msg_id,
            {
                "protocolVersion": "2025-06-18",
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "fake-agent", "version": "0.0.1"},
            },
        )
        # MCP stdio servers signal readiness after initialize.
        print(json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}),
              file=sys.stderr)
    elif method == "tools/list":
        reply(msg_id, {"tools": TOOLS})
    elif method == "tools/call":
        name = (req.get("params") or {}).get("name")
        reply(
            msg_id,
            {
                "content": [{"type": "text", "text": f"fake-executed {name}"}],
                "isError": False,
            },
        )
    elif method == "ping":
        reply(msg_id, {})
    else:
        if msg_id is not None:
            print(
                json.dumps(
                    {
                        "jsonrpc": "2.0",
                        "id": msg_id,
                        "error": {"code": -32601, "message": f"unknown {method}"},
                    }
                ),
                flush=True,
            )