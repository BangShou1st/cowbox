#!/usr/bin/env python3
"""Windows-EXE MCP forwarding acceptance for cowbox M3.

Run with a Windows Python interpreter. The script launches the real
cowbox.exe from a Windows workspace and keeps one MCP stdio session open while
the launcher forwards every request through wsl.exe.
"""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import subprocess
import time


class Client:
    def __init__(self, executable: pathlib.Path, workspace: pathlib.Path, server: str):
        env = os.environ.copy()
        self.process = subprocess.Popen(
            [
                str(executable),
                "mcp-proxy",
                "--network",
                "off",
                "--",
                "python3",
                server,
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

    def request(self, method: str, params: dict | None = None, timeout: float = 180) -> dict:
        request_id = self.next_id
        self.next_id += 1
        message = {"jsonrpc": "2.0", "id": request_id, "method": method}
        if params is not None:
            message["params"] = params
        self.process.stdin.write(json.dumps(message, separators=(",", ":")) + "\n")
        self.process.stdin.flush()
        deadline = time.time() + timeout
        while time.time() < deadline:
            line = self.process.stdout.readline()
            if not line:
                stderr = self.process.stderr.read() if self.process.stderr else ""
                raise RuntimeError(f"cowbox.exe exited while waiting for {method}: {stderr}")
            value = json.loads(line)
            if value.get("id") == request_id:
                return value
        raise TimeoutError(method)

    def initialize(self) -> dict:
        value = self.request(
            "initialize",
            {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "cowbox-windows-acceptance", "version": "1"},
            },
        )
        self.process.stdin.write(
            '{"jsonrpc":"2.0","method":"notifications/initialized"}\n'
        )
        self.process.stdin.flush()
        assert self.request("ping")["result"] == {}
        return value

    def tool(self, name: str, arguments: dict) -> dict:
        return self.request(
            "tools/call",
            {"name": name, "arguments": arguments},
        )

    def close(self) -> int:
        self.process.stdin.close()
        try:
            return self.process.wait(timeout=30)
        except subprocess.TimeoutExpired:
            self.process.kill()
            return self.process.wait()


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--exe", required=True)
    parser.add_argument("--workspace", required=True)
    parser.add_argument("--server", required=True)
    parser.add_argument("--appdata", required=True)
    parser.add_argument("--linux-binary", required=True)
    parser.add_argument("--json-output")
    args = parser.parse_args()

    exe = pathlib.Path(args.exe).resolve()
    workspace = pathlib.Path(args.workspace).resolve()
    appdata = pathlib.Path(args.appdata).resolve()
    config_dir = appdata / "cowbox"
    config_dir.mkdir(parents=True, exist_ok=True)
    (config_dir / "config.toml").write_text(
        "[wsl]\n"
        'distro = "Ubuntu"\n'
        f'linux_binary = "{args.linux_binary}"\n\n'
        "[agent]\n"
        'mode = "ask"\n'
        "require_confirmation = true\n",
        encoding="utf-8",
        newline="\n",
    )
    (workspace / ".git").mkdir(exist_ok=True)
    previous_appdata = os.environ.get("APPDATA")
    os.environ["APPDATA"] = str(appdata)
    report = {
        "exe": str(exe),
        "workspace": str(workspace),
        "server": args.server,
        "appdata": str(appdata),
    }
    client = Client(exe, workspace, args.server)
    try:
        report["initialize"] = client.initialize()["result"]
        listed = client.request("tools/list")
        assert listed.get("error") is None, listed
        names = [tool["name"] for tool in listed["result"]["tools"]]
        report["tools"] = names
        for name in ("write_file", "cowbox_diff", "cowbox_rollback", "cowbox_commit"):
            assert name in names, (name, names)

        wrong = client.tool(
            "write_file",
            {"path": "windows-wrong.txt", "content": "WRONG\n"},
        )["result"]
        report["wrong_step"] = wrong["_meta"]["cowbox"]
        assert report["wrong_step"]["head_before"] == 0
        assert report["wrong_step"]["step"] == 1
        read = client.tool("read_file", {"path": "windows-wrong.txt"})["result"]
        report["read_head"] = read["_meta"]["cowbox"]["head_before"]
        assert report["read_head"] == 1
        diff = client.tool("cowbox_diff", {})["result"]
        assert any(
            change["rel_path"] == "windows-wrong.txt"
            for change in diff["structuredContent"]["changes"]
        ), diff
        assert client.tool("cowbox_rollback", {"step": 0})["result"][
            "structuredContent"
        ]["head"] == 0
        right = client.tool(
            "write_file",
            {"path": "windows-wrong.txt", "content": "RIGHT\n"},
        )["result"]
        report["right_step"] = right["_meta"]["cowbox"]
        preview = client.tool("cowbox_commit", {"step": 3})["result"]
        report["commit_preview"] = preview["structuredContent"]
        assert preview["structuredContent"]["committed"] is False
    finally:
        report["proxy_exit_code"] = client.close()
        if previous_appdata is None:
            os.environ.pop("APPDATA", None)
        else:
            os.environ["APPDATA"] = previous_appdata
    report["ok"] = True
    text = json.dumps(report, indent=2) + "\n"
    if args.json_output:
        pathlib.Path(args.json_output).write_text(text, encoding="utf-8")
    print(text)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
