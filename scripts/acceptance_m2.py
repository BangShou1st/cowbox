#!/usr/bin/env python3
"""Real-CLI M2 network, offline-cache, and long-session acceptance."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path
from typing import Any, Sequence


class AcceptanceError(RuntimeError):
    pass


MUTATION = r"""set -e
printf 'modified\n' > modify.txt
rm delete.txt
rm -rf opaque
mkdir opaque
printf 'new opaque\n' > opaque/new.txt
rm -rf type-dir
printf 'now a file\n' > type-dir
rm type-file
mkdir type-file
printf 'type child\n' > type-file/child.txt
ln -sfn new-target link
printf 'hardlink contents\n' > hard-session-a.txt
ln hard-session-a.txt hard-session-b.txt
printf 'xattr changed\n' > xattr.txt
mkdir locked-000
printf 'locked child\n' > locked-000/child.txt
printf 'locked file\n' > locked-file.txt
mkdir read-only-dir
printf 'read-only child\n' > read-only-dir/child.txt
python3 - <<'PY'
import os
os.setxattr("xattr.txt", b"user.cowbox.test", b"session")
os.chmod("xattr.txt", 0o444)
os.setxattr("locked-file.txt", b"user.cowbox.test", b"locked")
os.chmod("locked-file.txt", 0o000)
os.chmod("locked-000", 0o000)
os.setxattr("read-only-dir", b"user.cowbox.test", b"directory")
os.chmod("read-only-dir", 0o555)
PY
"""

VIEW = r"""set -e
test ! -e delete.txt
test -f modify.txt
test "$(cat modify.txt)" = modified
test -f opaque/new.txt
test ! -e opaque/old.txt
test "$(cat opaque/new.txt)" = "new opaque"
test -f type-dir
test "$(cat type-dir)" = "now a file"
test -d type-file
test "$(cat type-file/child.txt)" = "type child"
test "$(readlink link)" = new-target
test "$(stat -c %d:%i hard-session-a.txt)" = "$(stat -c %d:%i hard-session-b.txt)"
test "$(stat -c %h hard-session-a.txt)" -ge 2
python3 - <<'PY'
import os
assert os.getxattr("xattr.txt", b"user.cowbox.test") == b"session"
assert os.stat("xattr.txt").st_mode & 0o777 == 0o444
assert os.getxattr("read-only-dir", b"user.cowbox.test") == b"directory"
assert os.stat("read-only-dir").st_mode & 0o777 == 0o555
assert os.stat("locked-000").st_mode & 0o777 == 0
assert os.stat("locked-000/child.txt").st_mode & 0o777 == 0o644
assert open("locked-000/child.txt", encoding="utf-8").read() == "locked child\n"
assert os.stat("locked-file.txt").st_mode & 0o777 == 0
assert os.getxattr("locked-file.txt", b"user.cowbox.test") == b"locked"
assert open("locked-file.txt", encoding="utf-8").read() == "locked file\n"
PY
"""

HOST_VIEW = r"""set -e
test "$(cat modify.txt)" = modified
test ! -e delete.txt
test -f opaque/new.txt
test ! -e opaque/old.txt
test -f type-dir
test -d type-file
test "$(readlink link)" = new-target
test "$(stat -c %d:%i hard-session-a.txt)" = "$(stat -c %d:%i hard-session-b.txt)"
test "$(stat -c %h hard-session-a.txt)" -ge 2
python3 - <<'PY'
import os
assert os.getxattr("xattr.txt", b"user.cowbox.test") == b"session"
assert os.stat("xattr.txt").st_mode & 0o777 == 0o444
assert os.stat("locked-000").st_mode & 0o777 == 0
assert os.stat("locked-file.txt").st_mode & 0o777 == 0
PY
"""


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description=(
            "Run network host/off checks, offline cache workflows, and the "
            "300-step real-change acceptance with a debug cowbox binary so "
            "deterministic failure and crash injection are available."
        )
    )
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--root", type=Path)
    parser.add_argument("--steps", type=int, default=300)
    parser.add_argument("--json-output", type=Path)
    parser.add_argument("--keep", action="store_true")
    args = parser.parse_args()
    if args.steps != 300:
        parser.error("the M2 acceptance protocol is defined for exactly 300 steps")
    return args


def invoke(
    binary: Path,
    workspace: Path,
    state: Path,
    args: Sequence[str],
    *,
    extra_env: dict[str, str] | None = None,
    expect_success: bool = True,
    expect_json: bool = True,
) -> tuple[subprocess.CompletedProcess[str], dict[str, Any]]:
    env = os.environ.copy()
    env["COWBOX_HOME"] = str(state)
    env["COWBOX_HELPER_EXE"] = str(binary)
    env["TMPDIR"] = "/tmp"
    env["LC_ALL"] = "C"
    env["LANG"] = "C"
    if extra_env:
        env.update(extra_env)
    process = subprocess.run(
        [str(binary), *args],
        cwd=workspace,
        env=env,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        check=False,
    )
    if expect_json:
        try:
            value = json.loads(process.stdout)
        except json.JSONDecodeError as error:
            raise AcceptanceError(
                f"non-JSON output from {args}: exit={process.returncode} "
                f"stdout={process.stdout!r} stderr={process.stderr!r}"
            ) from error
    else:
        value = {}
    if expect_success and (process.returncode != 0 or value.get("ok") is not True):
        raise AcceptanceError(
            f"command failed: {args} exit={process.returncode} "
            f"stdout={process.stdout.strip()!r} stderr={process.stderr.strip()!r}"
        )
    return process, value


def run(
    binary: Path,
    workspace: Path,
    state: Path,
    script: str,
    *,
    extra_env: dict[str, str] | None = None,
    network: str | None = "host",
) -> dict[str, Any]:
    network_args = [] if network is None else ["--network", network]
    _process, value = invoke(
        binary,
        workspace,
        state,
        ["run", "--json", *network_args, "--", "sh", "-c", script],
        extra_env=extra_env,
    )
    return value


class EchoServer:
    def __init__(self, family: int, host: str) -> None:
        self.family = family
        self.host = host
        self.stop = threading.Event()
        self.tcp = socket.socket(family, socket.SOCK_STREAM)
        self.tcp.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.tcp.bind((host, 0))
        self.tcp.listen()
        self.tcp.settimeout(0.2)
        self.tcp_port = self.tcp.getsockname()[1]
        self.udp = socket.socket(family, socket.SOCK_DGRAM)
        self.udp.bind((host, 0))
        self.udp.settimeout(0.2)
        self.udp_port = self.udp.getsockname()[1]
        self.threads = [
            threading.Thread(target=self._serve_tcp, daemon=True),
            threading.Thread(target=self._serve_udp, daemon=True),
        ]
        for thread in self.threads:
            thread.start()

    def _serve_tcp(self) -> None:
        while not self.stop.is_set():
            try:
                connection, _ = self.tcp.accept()
            except (TimeoutError, OSError):
                continue
            with connection:
                connection.settimeout(1)
                try:
                    request = connection.recv(4)
                    if request == b"ping":
                        connection.sendall(b"pong")
                except OSError:
                    pass

    def _serve_udp(self) -> None:
        while not self.stop.is_set():
            try:
                request, peer = self.udp.recvfrom(64)
            except (TimeoutError, OSError):
                continue
            if request == b"ping":
                try:
                    self.udp.sendto(b"pong", peer)
                except OSError:
                    pass

    def close(self) -> None:
        self.stop.set()
        self.tcp.close()
        self.udp.close()
        for thread in self.threads:
            thread.join(timeout=1)


def reachable_script(family: int, host: str, tcp_port: int, udp_port: int) -> str:
    family_name = "AF_INET6" if family == socket.AF_INET6 else "AF_INET"
    return f"""python3 - <<'PY'
import socket
family = socket.{family_name}
with socket.create_connection(("{host}", {tcp_port}), timeout=2) as stream:
    stream.sendall(b"ping")
    assert stream.recv(4) == b"pong"
sock = socket.socket(family, socket.SOCK_DGRAM)
sock.settimeout(2)
sock.sendto(b"ping", ("{host}", {udp_port}))
assert sock.recvfrom(4)[0] == b"pong"
PY
"""


def isolated_script(family: int, host: str, tcp_port: int, udp_port: int) -> str:
    family_name = "AF_INET6" if family == socket.AF_INET6 else "AF_INET"
    return f"""python3 - <<'PY'
import socket
family = socket.{family_name}
sock = socket.socket(family, socket.SOCK_STREAM)
sock.settimeout(0.5)
try:
    sock.connect(("{host}", {tcp_port}))
except OSError:
    tcp_blocked = True
else:
    tcp_blocked = False
sock.close()
datagram = socket.socket(family, socket.SOCK_DGRAM)
datagram.settimeout(0.5)
try:
    datagram.sendto(b"ping", ("{host}", {udp_port}))
    datagram.recvfrom(4)
except OSError:
    udp_blocked = True
else:
    udp_blocked = False
assert tcp_blocked, "TCP reached the host namespace"
assert udp_blocked, "UDP reached the host namespace"
PY
"""


def state_for(root: Path, name: str) -> Path:
    state = root / name
    state.mkdir(parents=True)
    return state


def commit_journals(state: Path) -> list[Path]:
    root = state / "sessions"
    if not root.is_dir():
        return []
    return sorted(path for path in root.glob("*/commit/*") if path.is_dir())


def write_fixture(workspace: Path) -> None:
    (workspace / ".git").mkdir()
    (workspace / "modify.txt").write_text("old\n", encoding="utf-8")
    (workspace / "delete.txt").write_text("delete\n", encoding="utf-8")
    (workspace / "opaque").mkdir()
    (workspace / "opaque/old.txt").write_text("old opaque\n", encoding="utf-8")
    (workspace / "type-dir").mkdir()
    (workspace / "type-dir/child.txt").write_text("old child\n", encoding="utf-8")
    (workspace / "type-file").write_text("old file\n", encoding="utf-8")
    (workspace / "link").symlink_to("base-target")
    (workspace / "xattr.txt").write_text("base xattr\n", encoding="utf-8")


def filesystem_for(path: Path) -> str:
    resolved = str(path.resolve())
    best: tuple[int, str] | None = None
    for line in Path("/proc/mounts").read_text(encoding="utf-8").splitlines():
        fields = line.split()
        if len(fields) < 3:
            continue
        mountpoint = (
            fields[1]
            .replace("\\040", " ")
            .replace("\\011", "\t")
            .replace("\\012", "\n")
            .replace("\\134", "\\")
        )
        if resolved == mountpoint or resolved.startswith(mountpoint.rstrip("/") + "/"):
            candidate = (len(mountpoint), fields[2])
            if best is None or candidate[0] > best[0]:
                best = candidate
    return best[1] if best else "unknown"


def assert_diff_equal(before: dict[str, Any], after: dict[str, Any], label: str) -> None:
    if before["data"]["changes"] != after["data"]["changes"]:
        raise AcceptanceError(f"{label} changed the logical diff")


def command_output(argv: Sequence[str], cwd: Path) -> str | None:
    try:
        process = subprocess.run(
            list(argv),
            cwd=cwd,
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="replace",
            check=False,
        )
    except OSError:
        return None
    value = (process.stdout or process.stderr).strip()
    return value or None


def binary_digest(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as file:
        for chunk in iter(lambda: file.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def read_os_release() -> dict[str, str]:
    values: dict[str, str] = {}
    path = Path("/etc/os-release")
    if not path.exists():
        return values
    for line in path.read_text(encoding="utf-8").splitlines():
        if "=" not in line:
            continue
        key, value = line.split("=", 1)
        values[key] = value.strip().strip('"')
    return values


def read_cpu_model() -> str:
    for line in Path("/proc/cpuinfo").read_text(encoding="utf-8").splitlines():
        if line.lower().startswith("model name"):
            return line.split(":", 1)[1].strip()
    return "unknown"


def read_memory_total() -> int | None:
    for line in Path("/proc/meminfo").read_text(encoding="utf-8").splitlines():
        if line.startswith("MemTotal:"):
            return int(line.split()[1]) * 1024
    return None


def collect_environment(repo: Path, binary: Path, filesystem: str) -> dict[str, Any]:
    status = command_output(["git", "status", "--short"], repo)
    return {
        "generated_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "machine": {
            "platform": platform.platform(),
            "kernel": platform.release(),
            "os_release": read_os_release(),
            "cpu_model": read_cpu_model(),
            "logical_cpus": os.cpu_count(),
            "memory_total_bytes": read_memory_total(),
            "load_average": [round(value, 3) for value in os.getloadavg()],
        },
        "fixture_filesystem": filesystem,
        "source": {
            "commit": command_output(["git", "rev-parse", "HEAD"], repo),
            "describe": command_output(
                ["git", "describe", "--tags", "--always", "--dirty"], repo
            ),
            "dirty": bool(status),
            "status": status,
        },
        "binary": {
            "path": str(binary),
            "sha256": binary_digest(binary),
            "version": command_output([str(binary), "--version"], repo),
            "size_bytes": binary.stat().st_size,
        },
        "tools": {
            "rustc": command_output(["rustc", "--version"], repo),
            "cargo": command_output(["cargo", "--version"], repo),
            "python": command_output(["python3", "--version"], repo),
            "node": command_output(["node", "--version"], repo),
            "npm": command_output(["npm", "--version"], repo),
        },
    }


def check_network(binary: Path, root: Path) -> dict[str, Any]:
    workspace = root / "network-workspace"
    workspace.mkdir()
    (workspace / ".git").mkdir()
    state = state_for(root, "network-state")
    invoke(binary, workspace, state, ["init", "--json"])

    results: dict[str, Any] = {}
    families = [("ipv4", socket.AF_INET, "127.0.0.1")]
    try:
        ipv6_probe = socket.socket(socket.AF_INET6, socket.SOCK_STREAM)
        ipv6_probe.bind(("::1", 0))
        ipv6_probe.close()
    except OSError as error:
        results["ipv6"] = {"available": False, "reason": str(error)}
    else:
        families.append(("ipv6", socket.AF_INET6, "::1"))

    for label, family, host in families:
        server = EchoServer(family, host)
        try:
            host_result = run(
                binary,
                workspace,
                state,
                reachable_script(family, host, server.tcp_port, server.udp_port),
                network="host",
            )
            off_result = run(
                binary,
                workspace,
                state,
                isolated_script(family, host, server.tcp_port, server.udp_port),
                network="off",
            )
        finally:
            server.close()
        if host_result["data"]["policy"]["network"] != "host":
            raise AcceptanceError(f"{label} host policy was not recorded: {host_result}")
        if off_result["data"]["policy"]["network"] != "off":
            raise AcceptanceError(f"{label} off policy was not recorded: {off_result}")
        results[label] = {
            "available": True,
            "endpoint": host,
            "tcp_port": server.tcp_port,
            "udp_port": server.udp_port,
            "tcp_host_reachable": True,
            "udp_host_reachable": True,
            "tcp_off_isolated": True,
            "udp_off_isolated": True,
        }

    failure_workspace = root / "network-failure-workspace"
    failure_workspace.mkdir()
    (failure_workspace / ".git").mkdir()
    failure_state = state_for(root, "network-failure-state")
    invoke(binary, failure_workspace, failure_state, ["init", "--json"])
    marker_dir = root / "network-marker"
    marker_dir.mkdir()
    marker = marker_dir / "command-started"
    failed_process, failed = invoke(
        binary,
        failure_workspace,
        failure_state,
        [
            "run",
            "--json",
            "--network",
            "off",
            "--allow-write",
            str(marker_dir),
            "--",
            "sh",
            "-c",
            f"printf started > {marker}",
        ],
        extra_env={"COWBOX_TEST_FAIL": "network-namespace"},
        expect_success=False,
    )
    if (
        failed_process.returncode != 125
        or failed["error"]["code"] != "NETWORK_NAMESPACE_FAILED"
    ):
        raise AcceptanceError(f"network namespace failure was not structured: {failed}")
    if marker.exists():
        raise AcceptanceError("network namespace failure started the user command")
    results["namespace_failure"] = {
        "error_code": failed["error"]["code"],
        "exit_code": failed_process.returncode,
        "command_started": False,
    }
    return results


def check_policy_precedence_and_legacy_session(binary: Path, root: Path) -> dict[str, Any]:
    workspace = root / "policy-workspace"
    workspace.mkdir()
    (workspace / ".git").mkdir()
    state = state_for(root, "policy-state")
    invoke(binary, workspace, state, ["init", "--json"])
    config = workspace / ".cowbox.toml"
    text = config.read_text(encoding="utf-8")
    if 'network = "host"' not in text:
        raise AcceptanceError("init template did not contain the network policy")
    config.write_text(text.replace('network = "host"', 'network = "off"'), encoding="utf-8")

    project = run(binary, workspace, state, "true", network=None)
    if (
        project["data"]["policy"]["network"] != "off"
        or project["data"]["policy"]["network_source"] != "project"
    ):
        raise AcceptanceError(f"project network policy was not applied: {project}")
    cli = run(binary, workspace, state, "true", network="host")
    if (
        cli["data"]["policy"]["network"] != "host"
        or cli["data"]["policy"]["network_source"] != "cli"
    ):
        raise AcceptanceError(f"CLI network policy did not override project config: {cli}")

    session_id = cli["session"]["id"]
    session_path = state / "sessions" / session_id / "session.json"
    legacy = json.loads(session_path.read_text(encoding="utf-8"))
    legacy.pop("run_policies", None)
    session_path.write_text(json.dumps(legacy), encoding="utf-8")
    _process, stats = invoke(binary, workspace, state, ["stats", "--json"])
    if stats["data"]["policy"]["recorded"] is not False:
        raise AcceptanceError(f"legacy session policy was not marked unknown: {stats}")
    invoke(binary, workspace, state, ["discard", "--yes", "--purge", "--json"])
    return {
        "project_mode": "off",
        "cli_override": "host",
        "legacy_session_policy": "unknown",
    }


def check_offline_workflow(binary: Path, root: Path, repo: Path) -> dict[str, Any]:
    offline_root = root / "offline"
    offline_root.mkdir()
    output = root / "offline-workflow.json"
    process = subprocess.run(
        [
            sys.executable,
            str(repo / "scripts" / "offline_workflow.py"),
            "--binary",
            str(binary),
            "--root",
            str(offline_root),
            "--json-output",
            str(output),
        ],
        cwd=repo,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        check=False,
    )
    if process.returncode != 0:
        raise AcceptanceError(
            "offline workflow failed: "
            f"stdout={process.stdout!r} stderr={process.stderr!r}"
        )
    return json.loads(output.read_text(encoding="utf-8"))


def check_offline_demo(binary: Path, root: Path) -> dict[str, Any]:
    workspace = root / "demo-workspace"
    workspace.mkdir()
    (workspace / ".git").mkdir()
    state = root / "demo-state"
    _process, value = invoke(binary, workspace, state, ["demo", "--offline", "--json"])
    if (
        value["data"]["offline"] is not True
        or value["data"]["network"] != "off"
        or value["data"]["host_unchanged"] is not True
    ):
        raise AcceptanceError(f"offline demo did not pass: {value}")
    return value["data"]


def main() -> int:
    acceptance_started = time.perf_counter()
    args = parse_args()
    binary = args.binary.resolve()
    if not binary.is_file() or not os.access(binary, os.X_OK):
        raise AcceptanceError(f"binary is missing or not executable: {binary}")
    root = args.root.resolve() if args.root else Path(
        tempfile.mkdtemp(prefix="cowbox-m2-acceptance-", dir="/tmp")
    )
    if args.root and root.exists() and any(root.iterdir()):
        raise AcceptanceError(f"acceptance root must be empty: {root}")
    root.mkdir(parents=True, exist_ok=True)
    filesystem = filesystem_for(root)
    if filesystem != "ext4":
        raise AcceptanceError(f"acceptance root must be ext4, got {filesystem}: {root}")
    repo = Path(__file__).resolve().parent.parent
    metadata = collect_environment(repo, binary, filesystem)

    network_results = check_network(binary, root)
    policy_results = check_policy_precedence_and_legacy_session(binary, root)
    offline_results = check_offline_workflow(binary, root, repo)
    offline_demo = check_offline_demo(binary, root)

    workspace = root / "workspace"
    workspace.mkdir()
    write_fixture(workspace)
    state = state_for(root, "state")

    invoke(binary, workspace, state, ["init", "--json"])
    step_networks: dict[int, str] = {}
    policy_counts = {"host": 0, "off": 0}
    first = run(binary, workspace, state, MUTATION, network="off")
    if first["data"]["step"] != 1:
        raise AcceptanceError(f"mutation did not create step 1: {first}")
    if first["data"]["policy"]["network"] != "off":
        raise AcceptanceError(f"mutation did not record offline policy: {first}")
    step_networks[1] = "off"
    policy_counts["off"] += 1
    _process, before = invoke(binary, workspace, state, ["diff", "--json"])
    before_changes = before["data"]["changes"]
    if not before_changes:
        raise AcceptanceError("real-change fixture produced an empty diff")

    compaction_events: list[dict[str, Any]] = []
    view_steps = {150, 152, 269, 300}
    for step in range(2, args.steps + 1):
        network = "off" if step in {151, 268, 300} or step % 17 == 0 else "host"
        if step in view_steps:
            result = run(binary, workspace, state, VIEW, network=network)
        else:
            result = run(binary, workspace, state, "true", network=network)
        if result["data"]["step"] != step:
            raise AcceptanceError(f"expected step {step}, got {result}")
        if result["data"]["policy"]["network"] != network:
            raise AcceptanceError(
                f"step {step} policy mismatch: expected {network}, got {result}"
            )
        step_networks[step] = network
        policy_counts[network] += 1
        if step in {151, 268, args.steps}:
            _stats_process, stats = invoke(
                binary, workspace, state, ["stats", "--json"]
            )
            if stats["data"]["policy"]["network"] != network:
                raise AcceptanceError(
                    f"step {step} stats policy mismatch: {stats}"
                )
            if step in {151, 268}:
                compaction_events.append(
                    {
                        "step": step,
                        "compact_layers": stats["data"]["compact_layers"],
                        "mounted_layers": stats["data"]["mounted_layers"],
                        "network": stats["data"]["policy"]["network"],
                    }
                )

    _process, after = invoke(binary, workspace, state, ["diff", "--json"])
    assert_diff_equal(before, after, "two compactions")
    _stats_process, final_stats = invoke(binary, workspace, state, ["stats", "--json"])
    if final_stats["data"]["compact_layers"] != 2:
        raise AcceptanceError(f"expected two compacted layers: {final_stats}")
    if len(compaction_events) != 2:
        raise AcceptanceError(f"expected two compaction events: {compaction_events}")

    _process, rollback = invoke(binary, workspace, state, ["rollback", "100", "--json"])
    _stats_process, rollback_stats = invoke(binary, workspace, state, ["stats", "--json"])
    if (
        rollback_stats["data"]["policy"]["network"]
        != step_networks[100]
    ):
        raise AcceptanceError(
            f"rollback did not restore the step-100 policy snapshot: {rollback_stats}"
        )
    branch = run(binary, workspace, state, "true", network="off")
    if branch["data"]["step"] != 301:
        raise AcceptanceError(f"branch did not create the next DAG step: {branch}")
    if branch["data"]["policy"]["network"] != "off":
        raise AcceptanceError(f"branch policy was not recorded: {branch}")
    _stats_process, branch_stats = invoke(binary, workspace, state, ["stats", "--json"])
    if branch_stats["data"]["policy"]["network"] != "off":
        raise AcceptanceError(f"branch stats did not use the new policy: {branch_stats}")
    _process, branched = invoke(binary, workspace, state, ["diff", "--json"])
    assert_diff_equal(before, branched, "rollback and branch")
    run(binary, workspace, state, VIEW, network="host")

    modify = workspace / "modify.txt"
    original = modify.read_bytes()
    original_stat = modify.stat()
    modify.write_text("host drift\n", encoding="utf-8")
    process, drift = invoke(
        binary,
        workspace,
        state,
        ["commit", "--dry-run", "--json"],
        expect_success=False,
    )
    if process.returncode != 2 or drift["error"]["code"] != "COMMIT_CONFLICT":
        raise AcceptanceError(f"host drift was not refused: {drift}")
    modify.write_bytes(original)
    os.utime(
        modify,
        ns=(original_stat.st_atime_ns, original_stat.st_mtime_ns),
    )

    _process, committed = invoke(
        binary, workspace, state, ["commit", "--yes", "--json"]
    )
    if committed["data"]["committed"] is not True:
        raise AcceptanceError(f"commit did not apply: {committed}")
    subprocess.run(
        ["sh", "-c", HOST_VIEW],
        cwd=workspace,
        check=True,
        capture_output=True,
        text=True,
    )

    recovery_workspace = root / "recovery-workspace"
    recovery_workspace.mkdir()
    (recovery_workspace / ".git").mkdir()
    (recovery_workspace / "base.txt").write_text("base\n", encoding="utf-8")
    recovery_state = state_for(root, "recovery-state")
    invoke(binary, recovery_workspace, recovery_state, ["init", "--json"])
    recovery_session = run(
        binary,
        recovery_workspace,
        recovery_state,
        "printf 'commit recovery\\n' > commit-recovery.txt",
    )
    if recovery_session["data"]["step"] != 1:
        raise AcceptanceError(f"recovery session step was not 1: {recovery_session}")
    crashed_process, _crashed = invoke(
        binary,
        recovery_workspace,
        recovery_state,
        ["commit", "--yes", "--json"],
        extra_env={"COWBOX_TEST_CRASH": "commit-after-first-write"},
        expect_success=False,
        expect_json=False,
    )
    if crashed_process.returncode != -signal.SIGKILL:
        raise AcceptanceError(
            "commit recovery injection did not terminate the process; use a debug binary"
        )
    recovered_process, recovered = invoke(
        binary,
        recovery_workspace,
        recovery_state,
        ["commit", "--recover", "--yes", "--json"],
    )
    if recovered["data"]["action"] != "restore_host":
        raise AcceptanceError(f"unexpected recovery action: {recovered}")
    if (recovery_workspace / "commit-recovery.txt").exists():
        raise AcceptanceError("recovery left the partial host write behind")
    _process, retried = invoke(
        binary, recovery_workspace, recovery_state, ["commit", "--yes", "--json"]
    )
    if retried["data"]["committed"] is not True:
        raise AcceptanceError(f"commit retry failed: {retried}")
    if (
        recovery_workspace / "commit-recovery.txt"
    ).read_text(encoding="utf-8") != "commit recovery\n":
        raise AcceptanceError("commit retry wrote the wrong content")

    directory_workspace = root / "directory-recovery-workspace"
    directory_workspace.mkdir()
    (directory_workspace / ".git").mkdir()
    swap = directory_workspace / "swap"
    swap.write_text("before\n", encoding="utf-8")
    directory_state = state_for(root, "directory-recovery-state")
    invoke(binary, directory_workspace, directory_state, ["init", "--json"])
    directory_session = run(
        binary,
        directory_workspace,
        directory_state,
        "set -e\nrm swap\nmkdir swap\nprintf 'committed child\\n' > swap/child.txt",
    )
    if directory_session["data"]["step"] != 1:
        raise AcceptanceError(
            f"directory recovery session step was not 1: {directory_session}"
        )
    directory_crash, _directory_crashed = invoke(
        binary,
        directory_workspace,
        directory_state,
        ["commit", "--yes", "--json"],
        extra_env={"COWBOX_TEST_CRASH": "commit-after-first-write"},
        expect_success=False,
        expect_json=False,
    )
    if directory_crash.returncode != -signal.SIGKILL:
        raise AcceptanceError("directory commit recovery injection did not terminate")
    if not commit_journals(directory_state):
        raise AcceptanceError("directory commit crash left no journal")
    if not swap.is_dir() or swap.stat().st_mode & 0o777 != 0o700:
        raise AcceptanceError(f"expected a 0700 intermediate directory: {swap}")

    user_file = swap / "user.txt"
    user_file.write_text("user data\n", encoding="utf-8")
    directory_refused_process, directory_refused = invoke(
        binary,
        directory_workspace,
        directory_state,
        ["commit", "--recover", "--yes", "--json"],
        expect_success=False,
    )
    if (
        directory_refused_process.returncode == 0
        or directory_refused["error"]["code"] != "COMMIT_FAILED"
        or "host content changed inside" not in directory_refused["error"]["message"]
    ):
        raise AcceptanceError(
            f"directory recovery did not preserve user content: {directory_refused}"
        )
    if user_file.read_text(encoding="utf-8") != "user data\n":
        raise AcceptanceError("directory recovery damaged user content")
    if not commit_journals(directory_state):
        raise AcceptanceError("directory recovery removed the journal after refusal")

    user_file.unlink()
    _directory_recovered_process, directory_recovered = invoke(
        binary,
        directory_workspace,
        directory_state,
        ["commit", "--recover", "--yes", "--json"],
    )
    if directory_recovered["data"]["action"] != "restore_host":
        raise AcceptanceError(
            f"directory intermediate state did not roll back: {directory_recovered}"
        )
    if not swap.is_file() or swap.read_text(encoding="utf-8") != "before\n":
        raise AcceptanceError("directory recovery did not restore the original file")
    if commit_journals(directory_state):
        journal = commit_journals(directory_state)[0]
        meta = json.loads((journal / "meta.json").read_text(encoding="utf-8"))
        if meta["phase"] != "rolled_back":
            raise AcceptanceError(f"directory journal was not rolled back: {meta}")

    result = {
        "schema_version": 1,
        "source": metadata["source"],
        "binary": metadata["binary"],
        "environment": {
            key: value
            for key, value in metadata.items()
            if key not in {"source", "binary"}
        },
        "network": network_results,
        "policy": policy_results,
        "offline": {
            "workflow": offline_results,
            "demo": offline_demo,
        },
        "results": {
            "acceptance_duration_ms": round(
                (time.perf_counter() - acceptance_started) * 1000, 3
            ),
            "steps": args.steps,
            "policy_counts": policy_counts,
            "diff_entries": len(before_changes),
            "compaction_events": compaction_events,
            "final_stats": final_stats["data"],
            "rollback_head": rollback["data"]["head"],
            "rollback_policy": rollback_stats["data"]["policy"],
            "branch_step": branch["data"]["step"],
            "branch_policy": branch_stats["data"]["policy"],
            "host_drift_error": drift["error"]["code"],
            "commit_changes": len(committed["data"]["changes"]),
            "recovery_action": recovered["data"]["action"],
            "recovery_restored": recovered["data"]["restored"],
            "directory_recovery": {
                "intermediate_mode": "0700",
                "refusal_code": directory_refused["error"]["code"],
                "user_content_preserved": True,
                "journal_retained_after_refusal": True,
                "action": directory_recovered["data"]["action"],
                "restored": directory_recovered["data"]["restored"],
                "swap_restored": True,
            },
        },
    }
    if args.json_output:
        args.json_output.parent.mkdir(parents=True, exist_ok=True)
        args.json_output.write_text(
            json.dumps(result, indent=2, sort_keys=True) + "\n",
            encoding="utf-8",
        )
    print(json.dumps(result, indent=2, sort_keys=True))
    if not args.keep:
        def make_writable(path: Path) -> None:
            for candidate in [path, *path.rglob("*")]:
                try:
                    if not candidate.is_symlink():
                        candidate.chmod(candidate.stat().st_mode | 0o700)
                except OSError:
                    pass

        make_writable(root)
        shutil.rmtree(root, ignore_errors=True)
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (AcceptanceError, subprocess.CalledProcessError) as error:
        print(f"acceptance failed: {error}", file=sys.stderr)
        raise SystemExit(1)
