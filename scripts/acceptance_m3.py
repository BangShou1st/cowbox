#!/usr/bin/env python3
"""Reproducible local-fixture acceptance for the cowbox M3 MCP workflow."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import pty
import select
import shutil
import socket
import statistics
import subprocess
import sys
import tempfile
import time
import traceback


class McpClient:
    def __init__(self, binary: pathlib.Path, workspace: pathlib.Path, state: pathlib.Path, server: pathlib.Path):
        env = os.environ.copy()
        env["COWBOX_HOME"] = str(state)
        self.process = subprocess.Popen(
            [
                str(binary),
                "mcp-proxy",
                "--workspace",
                str(workspace),
                "--network",
                "off",
                "--timeout",
                "20s",
                "--discovery-timeout",
                "20s",
                "--",
                sys.executable,
                str(server),
            ],
            cwd=workspace,
            env=env,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            bufsize=1,
        )
        self.next_id = 1

    def send(self, message: dict) -> None:
        assert self.process.stdin is not None
        self.process.stdin.write(json.dumps(message, separators=(",", ":")) + "\n")
        self.process.stdin.flush()

    def request(self, method: str, params: dict | None = None, timeout: float = 90) -> dict:
        request_id = self.next_id
        self.next_id += 1
        message = {"jsonrpc": "2.0", "id": request_id, "method": method}
        if params is not None:
            message["params"] = params
        started = time.perf_counter_ns()
        self.send(message)
        deadline = time.time() + timeout
        assert self.process.stdout is not None
        while time.time() < deadline:
            line = self.process.stdout.readline()
            if not line:
                stderr = self.process.stderr.read() if self.process.stderr else ""
                raise RuntimeError(f"mcp-proxy exited while waiting for {method}: {stderr}")
            value = json.loads(line)
            if value.get("id") != request_id:
                continue
            elapsed_ms = (time.perf_counter_ns() - started) / 1_000_000
            return {"value": value, "elapsed_ms": elapsed_ms}
        raise TimeoutError(f"timed out waiting for {method}")

    def initialize(self) -> dict:
        response = self.request(
            "initialize",
            {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "cowbox-acceptance", "version": "1"},
            },
        )
        self.send({"jsonrpc": "2.0", "method": "notifications/initialized"})
        ping = self.request("ping")
        assert ping["value"].get("result") == {}
        return response

    def tool(self, name: str, arguments: dict, timeout: float = 120) -> dict:
        return self.request(
            "tools/call",
            {"name": name, "arguments": arguments},
            timeout=timeout,
        )

    def close(self) -> int:
        if self.process.stdin:
            self.process.stdin.close()
        try:
            return self.process.wait(timeout=30)
        except subprocess.TimeoutExpired:
            self.process.kill()
            return self.process.wait()


def digest(path: pathlib.Path) -> str:
    hasher = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            hasher.update(chunk)
    return hasher.hexdigest()


def percentile(values: list[float], fraction: float) -> float:
    ordered = sorted(values)
    index = max(0, min(len(ordered) - 1, int(round((len(ordered) - 1) * fraction))))
    return ordered[index]


def summarize(values: list[float]) -> dict:
    return {
        "samples": len(values),
        "median_ms": round(statistics.median(values), 3),
        "p95_ms": round(percentile(values, 0.95), 3),
        "min_ms": round(min(values), 3),
        "max_ms": round(max(values), 3),
    }


def text_content(result: dict) -> str:
    return "\n".join(
        block.get("text", "")
        for block in result.get("content", [])
        if block.get("type") == "text"
    )


def command(binary: pathlib.Path, workspace: pathlib.Path, state: pathlib.Path, *args: str) -> dict:
    env = os.environ.copy()
    env["COWBOX_HOME"] = str(state)
    output = subprocess.run(
        [str(binary), *args],
        cwd=workspace,
        env=env,
        capture_output=True,
        text=True,
        timeout=180,
        check=False,
    )
    parsed = json.loads(output.stdout) if output.stdout.strip() else None
    return {
        "exit_code": output.returncode,
        "json": parsed,
        "stderr": output.stderr[-4000:],
    }


def commit_with_terminal_confirmation(
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
    try:
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
    finally:
        os.close(master)
        if process.poll() is None:
            process.kill()
    return {
        "exit_code": exit_code,
        "confirmation_submitted": submitted,
        "output_tail": captured.decode("utf-8", "replace")[-8000:],
    }


def measure_protocol(binary: pathlib.Path, root: pathlib.Path, server: pathlib.Path) -> dict:
    workspace = root / "bench-workspace"
    state = root / "bench-state"
    workspace.mkdir()
    (workspace / ".git").mkdir()
    (workspace / "seed.txt").write_text("seed\n", encoding="utf-8")
    local_server = workspace / "mcp_upstream.py"
    shutil.copy2(server, local_server)
    client = McpClient(binary, workspace, state, local_server)
    try:
        initialize = []
        for _ in range(21):
            initialize.append(client.initialize()["elapsed_ms"])
        discovery = client.request("tools/list")
        first_call = client.tool(
            "write_file",
            {"path": "seed.txt", "content": "seed\nwarm\n", "append": True},
        )
        continuous = []
        for _ in range(21):
            continuous.append(
                client.tool("read_file", {"path": "seed.txt"})["elapsed_ms"]
            )
        assert discovery["value"].get("error") is None, discovery
        assert first_call["value"]["result"].get("isError") is not True, first_call
        return {
            "mcp_initialize_21": summarize(initialize),
            "first_tool_discovery_ms": round(discovery["elapsed_ms"], 3),
            "first_tool_call_ms": round(first_call["elapsed_ms"], 3),
            "continuous_tool_calls_20_plus_1": summarize(continuous[1:]),
        }
    finally:
        client.close()


def run_workflow(binary: pathlib.Path, root: pathlib.Path, server: pathlib.Path) -> dict:
    workspace = root / "workflow-workspace"
    state = root / "workflow-state"
    workspace.mkdir()
    (workspace / ".git").mkdir()
    (workspace / "base.txt").write_text("base\n", encoding="utf-8")
    local_server = workspace / "mcp_upstream.py"
    shutil.copy2(server, local_server)
    listener = socket.socket()
    listener.bind(("127.0.0.1", 0))
    listener.listen(1)
    port = listener.getsockname()[1]
    client = McpClient(binary, workspace, state, local_server)
    report = {}
    try:
        report["initialize_protocol"] = client.initialize()["value"]["result"][
            "protocolVersion"
        ]
        listed = client.request("tools/list")
        assert listed["value"].get("error") is None, listed
        names = [
            tool["name"] for tool in listed["value"]["result"]["tools"]
        ]
        report["tools"] = names
        for required in ("write_file", "read_file", "cowbox_diff", "cowbox_rollback", "cowbox_commit"):
            assert required in names, (required, names)

        wrong = client.tool(
            "write_file",
            {"path": "app.txt", "content": "WRONG\n"},
        )["value"]["result"]
        report["wrong_step"] = wrong["_meta"]["cowbox"]
        assert report["wrong_step"]["head_before"] == 0
        assert report["wrong_step"]["step"] == 1
        assert not (workspace / "app.txt").exists()

        read = client.tool("read_file", {"path": "app.txt"})["value"]["result"]
        report["read_after_write"] = text_content(read)
        assert "WRONG" in report["read_after_write"]
        assert read["_meta"]["cowbox"]["head_before"] == 1
        assert read["_meta"]["cowbox"]["step"] == 2

        diff = client.tool("cowbox_diff", {})["value"]["result"]
        report["diff_wrong"] = diff["structuredContent"]
        assert any(
            change["rel_path"] == "app.txt" and change["kind"] == "added"
            for change in report["diff_wrong"]["changes"]
        )

        rollback = client.tool("cowbox_rollback", {"step": 0})["value"]["result"]
        report["rollback_head"] = rollback["structuredContent"]["head"]
        assert report["rollback_head"] == 0
        empty_diff = client.tool("cowbox_diff", {})["value"]["result"]
        report["diff_after_rollback"] = empty_diff["structuredContent"]
        assert report["diff_after_rollback"]["changes"] == []

        network = client.tool(
            "connect_tcp",
            {"host": "127.0.0.1", "port": port},
        )["value"]["result"]
        report["network_off"] = {
            "is_error": network.get("isError") is True,
            "text": text_content(network),
            "step": network["_meta"]["cowbox"]["step"],
        }
        assert report["network_off"]["is_error"]
        assert report["network_off"]["step"] == 3
        assert client.tool("cowbox_rollback", {"step": 0})["value"]["result"][
            "structuredContent"
        ]["head"] == 0

        right = client.tool(
            "write_file",
            {"path": "app.txt", "content": "RIGHT\n"},
        )["value"]["result"]
        target = right["_meta"]["cowbox"]["step"]
        report["right_step"] = right["_meta"]["cowbox"]
        assert report["right_step"]["head_before"] == 0
        assert target == 4

        diff = client.tool("cowbox_diff", {})["value"]["result"]
        report["diff_right"] = diff["structuredContent"]
        assert len(report["diff_right"]["changes"]) == 1
        preview = client.tool(
            "cowbox_commit", {"step": target}
        )["value"]["result"]
        report["commit_preview"] = preview["structuredContent"]
        assert report["commit_preview"]["committed"] is False
        assert report["commit_preview"]["target_step"] == target
        assert not (workspace / "app.txt").exists()
    finally:
        listener.close()
        proxy_exit = client.close()
        report["proxy_exit_code"] = proxy_exit

    terminal = commit_with_terminal_confirmation(binary, workspace, state, 4)
    report["terminal_commit"] = terminal
    assert terminal["exit_code"] == 0, terminal
    assert terminal["confirmation_submitted"], terminal
    assert (workspace / "app.txt").read_text(encoding="utf-8") == "RIGHT\n"
    assert (workspace / "base.txt").read_text(encoding="utf-8") == "base\n"
    report["host_files"] = sorted(
        str(path.relative_to(workspace))
        for path in workspace.rglob("*")
        if path.is_file()
    )
    report["sessions"] = command(binary, workspace, state, "sessions", "--json")["json"]
    statuses = {
        item["session_id"]: item["status"]
        for item in report["sessions"]["data"]["sessions"]
    }
    assert "committed" in statuses.values(), statuses
    return report


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", required=True)
    parser.add_argument(
        "--fixture-server",
        default="crates/cowbox-cli/tests/fixtures/mcp_upstream.py",
    )
    parser.add_argument("--root")
    parser.add_argument("--json-output")
    parser.add_argument("--skip-benchmark", action="store_true")
    args = parser.parse_args()
    binary = pathlib.Path(args.binary).resolve()
    fixture = pathlib.Path(args.fixture_server).resolve()
    if not binary.is_file():
        raise SystemExit(f"missing binary: {binary}")
    if not fixture.is_file():
        raise SystemExit(f"missing fixture server: {fixture}")
    if args.root:
        root = pathlib.Path(args.root).resolve()
        root.mkdir(parents=True, exist_ok=True)
    else:
        root = pathlib.Path(tempfile.mkdtemp(prefix="cowbox-m3-acceptance-", dir="/tmp"))
    server = root / "mcp_upstream.py"
    shutil.copy2(fixture, server)
    report = {
        "binary": str(binary),
        "binary_sha256": digest(binary),
        "root": str(root),
        "kernel": os.uname().release,
        "os_release": pathlib.Path("/etc/os-release").read_text(encoding="utf-8"),
        "fixture_server": str(server),
        "fixture_sha256": digest(server),
        "started_at_unix": int(time.time()),
    }
    try:
        report["workflow"] = run_workflow(binary, root, server)
        if not args.skip_benchmark:
            report["protocol_benchmark"] = measure_protocol(binary, root, server)
        report["ok"] = True
    except Exception as error:
        report["ok"] = False
        report["error"] = f"{type(error).__name__}: {error}"
        report["traceback"] = traceback.format_exc()
        raise
    finally:
        report["finished_at_unix"] = int(time.time())
        text = json.dumps(report, indent=2) + "\n"
        if args.json_output:
            output = pathlib.Path(args.json_output)
            output.parent.mkdir(parents=True, exist_ok=True)
            output.write_text(text, encoding="utf-8")
        print(text)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
