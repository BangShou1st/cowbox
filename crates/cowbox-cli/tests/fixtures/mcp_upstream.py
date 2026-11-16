#!/usr/bin/env python3
"""Deterministic stdio MCP server used by cowbox MCP integration tests."""

from __future__ import annotations

import argparse
import json
import os
import socket
import sys
import time
from pathlib import Path


def tool(name: str, description: str, schema: dict) -> dict:
    return {
        "name": name,
        "description": description,
        "inputSchema": schema,
    }


TOOLS = [
    tool(
        "write_file",
        "Write UTF-8 content to a workspace-relative path.",
        {
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "content": {"type": "string"},
                "append": {"type": "boolean", "default": False},
            },
            "required": ["path", "content"],
            "additionalProperties": False,
        },
    ),
    tool(
        "read_file",
        "Read a UTF-8 workspace-relative path.",
        {
            "type": "object",
            "properties": {"path": {"type": "string"}},
            "required": ["path"],
            "additionalProperties": False,
        },
    ),
    tool(
        "delete_file",
        "Delete a workspace-relative regular file.",
        {
            "type": "object",
            "properties": {"path": {"type": "string"}},
            "required": ["path"],
            "additionalProperties": False,
        },
    ),
    tool(
        "fail_tool",
        "Return a tool-level error without changing files.",
        {"type": "object", "properties": {}, "additionalProperties": False},
    ),
    tool(
        "rpc_error",
        "Return a JSON-RPC error without changing files.",
        {"type": "object", "properties": {}, "additionalProperties": False},
    ),
    tool(
        "hang_tool",
        "Sleep so timeout handling can be tested.",
        {"type": "object", "properties": {}, "additionalProperties": False},
    ),
    tool(
        "large_tool",
        "Return a large text result.",
        {
            "type": "object",
            "properties": {"bytes": {"type": "integer", "minimum": 1}},
            "required": ["bytes"],
            "additionalProperties": False,
        },
    ),
    tool(
        "connect_tcp",
        "Attempt a TCP connection and report whether it succeeded.",
        {
            "type": "object",
            "properties": {
                "host": {"type": "string"},
                "port": {"type": "integer"},
            },
            "required": ["host", "port"],
            "additionalProperties": False,
        },
    ),
    tool(
        "exit_tool",
        "Exit the upstream process without a protocol response.",
        {"type": "object", "properties": {}, "additionalProperties": False},
    ),
]


def workspace_path(raw: str) -> Path:
    root = Path.cwd().resolve()
    path = (root / raw).resolve()
    if root != path and root not in path.parents:
        raise ValueError("path escapes the workspace")
    return path


def text_result(text: str, is_error: bool = False) -> dict:
    return {
        "content": [{"type": "text", "text": text}],
        "isError": is_error,
    }


def call_tool(params: dict) -> dict:
    name = params.get("name")
    arguments = params.get("arguments") or {}
    if name == "write_file":
        path = workspace_path(arguments["path"])
        path.parent.mkdir(parents=True, exist_ok=True)
        mode = "a" if arguments.get("append") else "w"
        with path.open(mode, encoding="utf-8") as handle:
            handle.write(arguments["content"])
        return text_result(f"wrote {path.relative_to(Path.cwd().resolve())}")
    if name == "read_file":
        path = workspace_path(arguments["path"])
        return text_result(path.read_text(encoding="utf-8"))
    if name == "delete_file":
        path = workspace_path(arguments["path"])
        path.unlink()
        return text_result(f"deleted {path.relative_to(Path.cwd().resolve())}")
    if name == "fail_tool":
        return text_result("requested tool failure", is_error=True)
    if name == "hang_tool":
        time.sleep(120)
        return text_result("finished after sleep")
    if name == "large_tool":
        size = int(arguments.get("bytes", 1))
        return text_result("x" * size)
    if name == "connect_tcp":
        try:
            with socket.create_connection(
                (arguments["host"], int(arguments["port"])), timeout=0.5
            ):
                return text_result("connected")
        except OSError as error:
            return text_result(f"connection failed: {error}", is_error=True)
    if name == "exit_tool":
        os._exit(7)
    raise KeyError(f"unknown tool: {name}")


def send(value: dict) -> None:
    sys.stdout.write(json.dumps(value, separators=(",", ":")) + "\n")
    sys.stdout.flush()


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--page-size", type=int, default=0)
    parser.add_argument("--conflict", action="store_true")
    parser.add_argument("--startup-failure", action="store_true")
    parser.add_argument("--startup-delay", type=float, default=0.0)
    args = parser.parse_args()
    if args.startup_failure:
        print("requested startup failure", file=sys.stderr)
        return 42
    if args.startup_delay:
        time.sleep(args.startup_delay)

    tools = TOOLS + (
        [
            tool(
                "cowbox_diff",
                "Conflicting upstream tool.",
                {"type": "object", "properties": {}},
            )
        ]
        if args.conflict
        else []
    )
    print(f"fixture MCP server pid={os.getpid()}", file=sys.stderr)
    for raw in sys.stdin:
        try:
            message = json.loads(raw)
        except json.JSONDecodeError as error:
            send(
                {
                    "jsonrpc": "2.0",
                    "id": None,
                    "error": {"code": -32700, "message": str(error)},
                }
            )
            continue
        method = message.get("method")
        request_id = message.get("id")
        if request_id is None:
            continue
        if method == "initialize":
            requested = (message.get("params") or {}).get(
                "protocolVersion", "2024-11-05"
            )
            send(
                {
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "result": {
                        "protocolVersion": requested,
                        "capabilities": {"tools": {"listChanged": False}},
                        "serverInfo": {"name": "cowbox-fixture", "version": "1.0"},
                    },
                }
            )
        elif method == "tools/list":
            cursor = (message.get("params") or {}).get("cursor")
            start = int(cursor or 0)
            page_size = args.page_size or len(tools)
            end = min(start + page_size, len(tools))
            result = {"tools": tools[start:end]}
            if end < len(tools):
                result["nextCursor"] = str(end)
            send({"jsonrpc": "2.0", "id": request_id, "result": result})
        elif method == "tools/call":
            params = message.get("params") or {}
            if params.get("name") == "rpc_error":
                send(
                    {
                        "jsonrpc": "2.0",
                        "id": request_id,
                        "error": {
                            "code": -32010,
                            "message": "requested RPC failure",
                            "data": {"fixture": True},
                        },
                    }
                )
                continue
            try:
                result = call_tool(params)
                send({"jsonrpc": "2.0", "id": request_id, "result": result})
            except Exception as error:
                send(
                    {
                        "jsonrpc": "2.0",
                        "id": request_id,
                        "result": text_result(str(error), is_error=True),
                    }
                )
        elif method == "ping":
            send({"jsonrpc": "2.0", "id": request_id, "result": {}})
        else:
            send(
                {
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "error": {"code": -32601, "message": f"unknown method {method}"},
                }
            )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
