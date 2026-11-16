#!/usr/bin/env python3
"""Real Claude Code harness acceptance for the cowbox M3 MCP workflow."""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import pty
import select
import shutil
import subprocess
import tempfile
import time
import traceback


PROMPT = """You are a cowbox MCP end-to-end test operator. Use ONLY the MCP server named cowbox. Do not use Bash, Edit, Write, Read, Glob, or Grep.

Perform exactly this sequence:
1. Call write_file to write app.txt with content "WRONG\\n".
2. Call read_file for app.txt and confirm it contains WRONG.
3. Call cowbox_diff.
4. Call cowbox_rollback with step 0.
5. Call write_file to write app.txt with content "RIGHT\\n".
6. Call cowbox_diff again.
7. Call cowbox_commit for the resulting current step.

Do not run a shell commit. In your final answer, report each cowbox step number, head_before, head_after, whether diff showed app.txt, and the terminal_command returned by cowbox_commit."""

MCP_TOOLS = [
    "mcp__cowbox__write_file",
    "mcp__cowbox__read_file",
    "mcp__cowbox__cowbox_diff",
    "mcp__cowbox__cowbox_rollback",
    "mcp__cowbox__cowbox_commit",
]


def run_pty_commit(
    binary: pathlib.Path,
    workspace: pathlib.Path,
    state: pathlib.Path,
    step: int,
) -> dict:
    env = os.environ.copy()
    env["COWBOX_HOME"] = str(state)
    master, slave = pty.openpty()
    process = subprocess.Popen(
        [str(binary), "commit", "--step", str(step)],
        cwd=workspace,
        env=env,
        stdin=slave,
        stdout=slave,
        stderr=slave,
        close_fds=True,
    )
    os.close(slave)
    captured = b""
    submitted = False
    deadline = time.time() + 180
    while process.poll() is None and time.time() < deadline:
        readable, _, _ = select.select([master], [], [], 0.2)
        if not readable:
            continue
        try:
            chunk = os.read(master, 8192)
        except OSError:
            break
        if not chunk:
            break
        captured += chunk
        if not submitted and b"[y/N]" in captured:
            os.write(master, b"y\n")
            submitted = True
    while True:
        try:
            chunk = os.read(master, 8192)
        except OSError:
            break
        if not chunk:
            break
        captured += chunk
    exit_code = process.wait(timeout=10)
    os.close(master)
    return {
        "exit_code": exit_code,
        "confirmation_submitted": submitted,
        "output_tail": captured.decode("utf-8", "replace")[-8000:],
    }


def session_state(
    binary: pathlib.Path,
    workspace: pathlib.Path,
    state: pathlib.Path,
    command: str,
) -> dict:
    env = os.environ.copy()
    env["COWBOX_HOME"] = str(state)
    output = subprocess.run(
        [str(binary), command, "--json"],
        cwd=workspace,
        env=env,
        capture_output=True,
        text=True,
        timeout=60,
        check=False,
    )
    return {
        "exit_code": output.returncode,
        "json": json.loads(output.stdout) if output.stdout.strip() else None,
        "stderr": output.stderr[-4000:],
    }


def cowbox_meta(result: dict) -> dict:
    value = result.get("tool_use_result") or {}
    return value.get("_meta", {}).get("cowbox", {})


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", required=True)
    parser.add_argument("--claude", default="claude")
    parser.add_argument(
        "--fixture-server",
        default="crates/cowbox-cli/tests/fixtures/mcp_upstream.py",
    )
    parser.add_argument("--root")
    parser.add_argument("--json-output")
    parser.add_argument("--timeout", type=int, default=300)
    args = parser.parse_args()

    binary = pathlib.Path(args.binary).resolve()
    fixture = pathlib.Path(args.fixture_server).resolve()
    if not binary.is_file():
        raise SystemExit(f"missing binary: {binary}")
    if not fixture.is_file():
        raise SystemExit(f"missing fixture server: {fixture}")
    root = (
        pathlib.Path(args.root).resolve()
        if args.root
        else pathlib.Path(
            tempfile.mkdtemp(prefix="cowbox-m3-claude-", dir="/tmp")
        )
    )
    workspace = root / "workspace"
    state = root / "state"
    workspace.mkdir(parents=True, exist_ok=True)
    (workspace / ".git").mkdir(exist_ok=True)
    (workspace / "base.txt").write_text("base\n", encoding="utf-8")
    server = workspace / "mcp_upstream.py"
    shutil.copy2(fixture, server)

    mcp_config = root / "mcp.json"
    mcp_config.write_text(
        json.dumps(
            {
                "mcpServers": {
                    "cowbox": {
                        "command": str(binary),
                        "args": [
                            "mcp-proxy",
                            "--workspace",
                            str(workspace),
                            "--network",
                            "off",
                            "--",
                            "python3",
                            str(server),
                        ],
                        "env": {"COWBOX_HOME": str(state)},
                    }
                }
            },
            indent=2,
        )
        + "\n",
        encoding="utf-8",
    )

    command = [
        args.claude,
        "-p",
        PROMPT,
        "--mcp-config",
        str(mcp_config),
        "--allowedTools",
        ",".join(MCP_TOOLS),
        "--disallowedTools",
        "Bash,Edit,Write,Read,Glob,Grep,Task,Skill,Workflow",
        "--permission-mode",
        "dontAsk",
        "--output-format",
        "stream-json",
        "--verbose",
        "--no-session-persistence",
    ]
    env = os.environ.copy()
    env["COWBOX_HOME"] = str(state)
    started = time.time()
    process = subprocess.Popen(
        command,
        cwd=workspace,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        stdout, stderr = process.communicate(timeout=args.timeout)
    except subprocess.TimeoutExpired:
        process.kill()
        stdout, stderr = process.communicate()
        log_path = root / "claude.jsonl"
        log_path.write_text(stdout, encoding="utf-8")
        raise RuntimeError(
            f"Claude harness timed out after {args.timeout}s; log={log_path}"
        )
    return_code = process.returncode
    log_path = root / "claude.jsonl"
    log_path.write_text(stdout, encoding="utf-8")
    preserved_log = None
    if args.json_output:
        output = pathlib.Path(args.json_output)
        preserved_log = output.with_suffix(output.suffix + ".claude.jsonl")
        preserved_log.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(log_path, preserved_log)

    report = {
        "ok": False,
        "root": str(root),
        "binary": str(binary),
        "workspace": str(workspace),
        "state": str(state),
        "claude_exit_code": return_code,
        "claude_stderr_tail": stderr[-8000:],
        "claude_log": str(log_path),
        "claude_log_output": str(preserved_log) if preserved_log else None,
        "started_at_unix": int(started),
        "tool_calls": [],
    }
    try:
        events = [
            json.loads(line)
            for line in stdout.splitlines()
            if line.strip().startswith("{")
        ]
        tool_results = {}
        final_text = None
        model = None
        mcp_servers = None
        for event in events:
            if event.get("type") == "system" and event.get("subtype") == "init":
                model = event.get("model")
                mcp_servers = event.get("mcp_servers")
            message = event.get("message") or {}
            for part in message.get("content") or []:
                if event.get("type") == "assistant" and part.get("type") == "tool_use":
                    report["tool_calls"].append(
                        {
                            "name": part.get("name"),
                            "input": part.get("input"),
                        }
                    )
                if event.get("type") == "user" and part.get("type") == "tool_result":
                    tool_results[part.get("tool_use_id")] = event
            if event.get("type") == "result":
                final_text = event.get("result")

        mcp_calls = [
            call
            for call in report["tool_calls"]
            if call["name"].startswith("mcp__cowbox__")
        ]
        names = [call["name"] for call in mcp_calls]
        assert names == [
            "mcp__cowbox__write_file",
            "mcp__cowbox__read_file",
            "mcp__cowbox__cowbox_diff",
            "mcp__cowbox__cowbox_rollback",
            "mcp__cowbox__write_file",
            "mcp__cowbox__cowbox_diff",
            "mcp__cowbox__cowbox_commit",
        ], names
        assert return_code == 0, stderr
        assert final_text, "Claude returned no final text"

        result_events = list(tool_results.values())
        metas = [cowbox_meta(event) for event in result_events]
        step_metas = [meta for meta in metas if meta.get("step") is not None]
        assert step_metas[0]["step"] == 1
        assert step_metas[0]["head_before"] == 0
        assert step_metas[1]["step"] == 2
        assert step_metas[1]["head_before"] == 1
        assert step_metas[2]["step"] == 3
        assert step_metas[2]["head_before"] == 0
        assert not (workspace / "app.txt").exists()

        log = session_state(binary, workspace, state, "log")
        assert log["exit_code"] == 0, log
        assert log["json"]["session"]["head"] == 3, log
        target = log["json"]["session"]["head"]
        terminal = run_pty_commit(binary, workspace, state, target)
        assert terminal["exit_code"] == 0, terminal
        assert terminal["confirmation_submitted"], terminal
        assert (workspace / "app.txt").read_text(encoding="utf-8") == "RIGHT\n"
        assert (workspace / "base.txt").read_text(encoding="utf-8") == "base\n"
        sessions = session_state(binary, workspace, state, "sessions")
        statuses = {
            item["session_id"]: item["status"]
            for item in sessions["json"]["data"]["sessions"]
        }
        assert "committed" in statuses.values(), statuses

        report.update(
            {
                "ok": True,
                "model": model,
                "mcp_servers": mcp_servers,
                "mcp_calls": mcp_calls,
                "final_text": final_text,
                "terminal_commit": terminal,
                "session_statuses": statuses,
                "host_app": "RIGHT\n",
                "finished_at_unix": int(time.time()),
            }
        )
    except Exception as error:
        report["error"] = f"{type(error).__name__}: {error}"
        report["traceback"] = traceback.format_exc()
        raise
    finally:
        text = json.dumps(report, indent=2) + "\n"
        if args.json_output:
            output = pathlib.Path(args.json_output)
            output.parent.mkdir(parents=True, exist_ok=True)
            output.write_text(text, encoding="utf-8")
        print(text)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
