#!/usr/bin/env python3
"""Offline Cargo, npm, and pip cache workflow acceptance."""

from __future__ import annotations

import argparse
import base64
import functools
import hashlib
import http.server
import json
import os
import platform
import shutil
import stat
import subprocess
import sys
import tempfile
import threading
import zipfile
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Sequence


class OfflineError(RuntimeError):
    pass


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description=(
            "Run real Cargo, npm, and pip offline workflows through cowbox "
            "using only local fixtures and host cache copies."
        )
    )
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--root", type=Path)
    parser.add_argument("--json-output", type=Path)
    parser.add_argument("--keep", action="store_true")
    return parser.parse_args()


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
    output = (process.stdout or process.stderr).strip()
    return output or None


def binary_digest(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as file:
        for chunk in iter(lambda: file.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def tree_digest(root: Path) -> str:
    digest = hashlib.sha256()
    for current, directories, files in os.walk(root, followlinks=False):
        current_path = Path(current)
        directories.sort()
        files.sort()
        for name in directories + files:
            path = current_path / name
            relative = path.relative_to(root).as_posix()
            metadata = path.lstat()
            if stat.S_ISLNK(metadata.st_mode):
                payload = os.readlink(path).encode()
                kind = "symlink"
            elif stat.S_ISDIR(metadata.st_mode):
                payload = b""
                kind = "directory"
            elif stat.S_ISREG(metadata.st_mode):
                payload = path.read_bytes()
                kind = "file"
            else:
                payload = b""
                kind = "other"
            digest.update(kind.encode())
            digest.update(relative.encode())
            digest.update(str(stat.S_IMODE(metadata.st_mode)).encode())
            digest.update(hashlib.sha256(payload).digest())
    return digest.hexdigest()


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


def invoke(
    binary: Path,
    workspace: Path,
    state: Path,
    args: Sequence[str],
    *,
    expect_success: bool = True,
) -> tuple[subprocess.CompletedProcess[str], dict[str, Any]]:
    env = os.environ.copy()
    env["COWBOX_HOME"] = str(state)
    env["COWBOX_HELPER_EXE"] = str(binary)
    env["TMPDIR"] = "/tmp"
    env["LC_ALL"] = "C"
    env["LANG"] = "C"
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
    try:
        value = json.loads(process.stdout)
    except json.JSONDecodeError as error:
        raise OfflineError(
            f"non-JSON output from {args}: exit={process.returncode} "
            f"stdout={process.stdout!r} stderr={process.stderr!r}"
        ) from error
    if expect_success and (process.returncode != 0 or value.get("ok") is not True):
        raise OfflineError(
            f"command failed: {args} exit={process.returncode} "
            f"stdout={process.stdout.strip()!r} stderr={process.stderr.strip()!r}"
        )
    return process, value


def run_script(
    binary: Path,
    workspace: Path,
    state: Path,
    script: str,
    *,
    expect_zero_exit: bool = True,
) -> tuple[subprocess.CompletedProcess[str], dict[str, Any]]:
    read_paths = []
    run_env = []
    rustup_home = Path(os.environ.get("RUSTUP_HOME", Path.home() / ".rustup"))
    for candidate in (
        Path.home() / ".cargo" / "bin",
        rustup_home,
    ):
        if candidate.is_dir():
            read_paths.extend(["--allow-read", str(candidate)])
    if rustup_home.is_dir():
        run_env.extend(["--env", f"RUSTUP_HOME={rustup_home}"])
    process, value = invoke(
        binary,
        workspace,
        state,
        [
            "run",
            "--json",
            "--network",
            "off",
            *read_paths,
            *run_env,
            "--",
            "sh",
            "-c",
            script,
        ],
        expect_success=False,
    )
    if value.get("ok") is not True:
        raise OfflineError(f"sandbox run failed: {value}")
    if expect_zero_exit and process.returncode != 0:
        raise OfflineError(
            f"sandbox command failed: exit={process.returncode} "
            f"stdout={process.stdout!r} stderr={process.stderr!r}"
        )
    return process, value


def write_cargo_fixture(workspace: Path, dependency: Path) -> None:
    consumer = workspace / "cargo-app"
    (consumer / "src").mkdir(parents=True)
    (consumer / ".cargo").mkdir()
    (consumer / "Cargo.toml").write_text(
        """[package]
name = "offline-consumer"
version = "0.1.0"
edition = "2021"

[dependencies]
demo-dep = "=1.0.0"
""",
        encoding="utf-8",
    )
    (consumer / "src/main.rs").write_text(
        "fn main() { println!(\"{}\", demo_dep::answer()); }\n",
        encoding="utf-8",
    )
    (consumer / ".cargo/config.toml").write_text(
        """[source.crates-io]
replace-with = "cowbox-vendor"

[source.cowbox-vendor]
directory = "unused"
""",
        encoding="utf-8",
    )
    vendor = dependency / "registry" / "vendor" / "demo-dep"
    (vendor / "src").mkdir(parents=True, exist_ok=True)
    (vendor / "Cargo.toml").write_text(
        """[package]
name = "demo-dep"
version = "1.0.0"
edition = "2021"
""",
        encoding="utf-8",
    )
    (vendor / "src/lib.rs").write_text(
        "pub fn answer() -> u32 { 42 }\n", encoding="utf-8"
    )
    files = {
        name: hashlib.sha256((vendor / name).read_bytes()).hexdigest()
        for name in ("Cargo.toml", "src/lib.rs")
    }
    (vendor / ".cargo-checksum.json").write_text(
        json.dumps({"files": files, "package": None}),
        encoding="utf-8",
    )


def prepare_npm_distribution(root: Path) -> Path:
    source = root / "npm-package"
    (source / "lib").mkdir(parents=True, exist_ok=True)
    (source / "package.json").write_text(
        json.dumps(
            {
                "name": "demo-pkg",
                "version": "1.0.0",
                "main": "lib/index.js",
                "files": ["lib"],
            }
        ),
        encoding="utf-8",
    )
    (source / "lib/index.js").write_text(
        "module.exports = 42;\n", encoding="utf-8"
    )
    distribution = root / "npm-dist"
    distribution.mkdir()
    packed = subprocess.run(
        ["npm", "pack", "--silent", "--pack-destination", str(distribution)],
        cwd=source,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        check=False,
    )
    if packed.returncode != 0:
        raise OfflineError(
            f"npm pack failed: exit={packed.returncode} "
            f"stdout={packed.stdout!r} stderr={packed.stderr!r}"
        )
    archives = sorted(distribution.glob("demo-pkg-*.tgz"))
    if len(archives) != 1:
        raise OfflineError(f"expected one npm package archive, got {archives}")
    return archives[0]


def write_npm_fixture(workspace: Path, package_url: str) -> None:
    app = workspace / "npm-app"
    app.mkdir()
    (app / "package.json").write_text(
        json.dumps(
            {
                "name": "offline-npm-consumer",
                "version": "1.0.0",
                "private": True,
                "dependencies": {"demo-pkg": package_url},
            }
        ),
        encoding="utf-8",
    )
    locked = subprocess.run(
        [
            "npm",
            "install",
            "--package-lock-only",
            "--ignore-scripts",
            "--no-audit",
            "--no-fund",
        ],
        cwd=app,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        check=False,
    )
    if locked.returncode != 0:
        raise OfflineError(
            f"npm package-lock generation failed: exit={locked.returncode} "
            f"stdout={locked.stdout!r} stderr={locked.stderr!r}"
        )

def seed_npm_cache(npm_cache: Path, package_url: str, cwd: Path) -> None:
    cached = subprocess.run(
        ["npm", "cache", "add", "--cache", str(npm_cache), package_url],
        cwd=cwd,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        check=False,
    )
    if cached.returncode != 0:
        raise OfflineError(
            f"npm cache seed failed: exit={cached.returncode} "
            f"stdout={cached.stdout!r} stderr={cached.stderr!r}"
        )


def start_npm_server(distribution: Path) -> tuple[http.server.ThreadingHTTPServer, str]:
    class QuietHandler(http.server.SimpleHTTPRequestHandler):
        def log_message(self, _format: str, *args: Any) -> None:
            _ = args

    handler = functools.partial(
        QuietHandler,
        directory=str(distribution),
    )
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    archive = next(distribution.glob("demo-pkg-*.tgz"))
    return server, f"http://127.0.0.1:{server.server_port}/{archive.name}"


def build_wheel(destination: Path) -> Path:
    destination.mkdir(parents=True, exist_ok=True)
    wheel = destination / "demo_pkg-1.0.0-py3-none-any.whl"
    entries = {
        "demo_pkg/__init__.py": b"VALUE = 42\n",
        "demo_pkg-1.0.0.dist-info/METADATA": (
            b"Metadata-Version: 2.1\nName: demo-pkg\nVersion: 1.0.0\n"
        ),
        "demo_pkg-1.0.0.dist-info/WHEEL": (
            b"Wheel-Version: 1.0\nGenerator: cowbox-offline\n"
            b"Root-Is-Purelib: true\nTag: py3-none-any\n"
        ),
    }
    record = []
    for name, payload in entries.items():
        digest = base64.urlsafe_b64encode(hashlib.sha256(payload).digest()).decode()
        record.append(f"{name},sha256={digest},{len(payload)}")
    record.append("demo_pkg-1.0.0.dist-info/RECORD,,")
    entries["demo_pkg-1.0.0.dist-info/RECORD"] = (
        "\n".join(record) + "\n"
    ).encode()
    with zipfile.ZipFile(wheel, "w", zipfile.ZIP_DEFLATED) as archive:
        for name, payload in entries.items():
            archive.writestr(name, payload)
    return wheel


def prepare_workspace(
    workspace: Path,
    cargo_cache: Path,
    npm_url: str,
    pip_cache: Path,
) -> None:
    (workspace / ".git").mkdir(parents=True)
    write_cargo_fixture(workspace, cargo_cache)
    write_npm_fixture(workspace, npm_url)
    build_wheel(pip_cache / "wheelhouse")


def configure_host_caches(
    workspace: Path, cargo_cache: Path, npm_cache: Path, pip_cache: Path
) -> None:
    config = workspace / ".cowbox.toml"
    text = config.read_text(encoding="utf-8")
    replacement = "\n".join(
        [
            f"cargo = {json.dumps(str(cargo_cache))}",
            f"npm = {json.dumps(str(npm_cache))}",
            f"pip = {json.dumps(str(pip_cache))}",
        ]
    )
    marker = "cargo = false\nnpm = false\npip = false"
    if marker not in text:
        raise OfflineError("init template did not contain the expected host cache block")
    config.write_text(text.replace(marker, replacement), encoding="utf-8")


CARGO_COMMAND = (
    '(cd cargo-app && cargo --offline --config '
    '"source.cowbox-vendor.directory=\\"$CARGO_HOME/registry/vendor\\"" '
    "metadata --format-version 1 >/dev/null)"
)
NPM_COMMAND = (
    "(cd npm-app && npm ci --offline --ignore-scripts --no-audit --no-fund)"
)
PIP_COMMAND = (
    "python3 -m pip install --no-index --disable-pip-version-check "
    "--no-compile --target installed demo-pkg >/dev/null"
)


def verify_warm_script(script: str, exit_code: int) -> None:
    if exit_code != 0:
        raise OfflineError(f"warm cache workflow failed with exit {exit_code}: {script}")


def main() -> int:
    args = parse_args()
    binary = args.binary.resolve()
    if not binary.is_file() or not os.access(binary, os.X_OK):
        raise OfflineError(f"binary is missing or not executable: {binary}")
    root = args.root.resolve() if args.root else Path(
        tempfile.mkdtemp(prefix="cowbox-offline-", dir="/tmp")
    )
    if args.root and root.exists() and any(root.iterdir()):
        raise OfflineError(f"offline root must be empty: {root}")
    root.mkdir(parents=True, exist_ok=True)
    filesystem = filesystem_for(root)
    if filesystem != "ext4":
        raise OfflineError(f"offline fixture root must be ext4, got {filesystem}: {root}")

    repo = Path(__file__).resolve().parent.parent
    version = command_output([str(binary), "--version"], repo)
    if version is None:
        raise OfflineError("cannot read the cowbox binary version")
    source_commit = command_output(["git", "rev-parse", "HEAD"], repo)
    source_status = command_output(["git", "status", "--short"], repo)

    host_cargo = root / "host-cargo"
    host_npm = root / "host-npm"
    host_pip = root / "host-pip"
    cold_workspace = root / "cold-workspace"
    warm_workspace = root / "warm-workspace"
    cold_state = root / "cold-state"
    warm_state = root / "warm-state"
    for path in (
        host_cargo,
        host_npm,
        host_pip,
        cold_workspace,
        warm_workspace,
    ):
        path.mkdir()
    for path in (cold_state, warm_state):
        path.mkdir()

    prepare_npm_distribution(root)
    npm_server, npm_url = start_npm_server(root / "npm-dist")
    prepare_workspace(cold_workspace, host_cargo, npm_url, host_pip)
    prepare_workspace(warm_workspace, host_cargo, npm_url, host_pip)
    seed_npm_cache(host_npm, npm_url, root)
    npm_server.shutdown()
    npm_server.server_close()

    invoke(binary, cold_workspace, cold_state, ["init", "--json"])
    cold_baseline = tree_digest(cold_workspace)
    cold_results: dict[str, Any] = {}
    for tool, script in (
        ("cargo", CARGO_COMMAND),
        ("npm", NPM_COMMAND),
        ("pip", PIP_COMMAND),
    ):
        process, value = run_script(
            binary,
            cold_workspace,
            cold_state,
            script,
            expect_zero_exit=False,
        )
        if value["data"]["policy"]["network"] != "off":
            raise OfflineError(f"{tool} cold run did not use the offline policy")
        cold_results[tool] = {
            "missing": process.returncode != 0,
            "exit_code": process.returncode,
            "stderr_tail": value["data"]["stderr"][-500:],
        }
        if process.returncode == 0:
            raise OfflineError(
                f"{tool} unexpectedly succeeded without a host cache"
            )

    invoke(binary, warm_workspace, warm_state, ["init", "--json"])
    configure_host_caches(warm_workspace, host_cargo, host_npm, host_pip)
    host_hashes = {
        "cargo": tree_digest(host_cargo),
        "npm": tree_digest(host_npm),
        "pip": tree_digest(host_pip),
    }
    warm_baseline = tree_digest(warm_workspace)
    warm_script = f"""set -e
{CARGO_COMMAND}
{NPM_COMMAND}
{PIP_COMMAND}
printf 'cargo session cache\\n' > "$CARGO_HOME/registry/cowbox-session-marker"
printf 'npm session cache\\n' > "$NPM_CONFIG_CACHE/_cacache/cowbox-session-marker"
printf 'pip session cache\\n' > "$PIP_CACHE_DIR/cowbox-session-marker"
"""
    first_process, first_value = run_script(
        binary, warm_workspace, warm_state, warm_script
    )
    verify_warm_script(warm_script, first_process.returncode)
    policy = first_value["data"]["policy"]
    if policy["network"] != "off":
        raise OfflineError(f"warm workflow did not run offline: {policy}")
    if sorted(policy["host_caches"]) != ["cargo", "npm", "pip"]:
        raise OfflineError(f"warm workflow did not record all host caches: {policy}")

    second_process, second_value = run_script(
        binary,
        warm_workspace,
        warm_state,
        """set -e
test "$(cat "$CARGO_HOME/registry/cowbox-session-marker")" = "cargo session cache"
test "$(cat "$NPM_CONFIG_CACHE/_cacache/cowbox-session-marker")" = "npm session cache"
test "$(cat "$PIP_CACHE_DIR/cowbox-session-marker")" = "pip session cache"
PYTHONPATH=installed python3 -c 'import demo_pkg; assert demo_pkg.VALUE == 42'
cd npm-app && node -e 'if (require("demo-pkg") !== 42) process.exit(1)'
""",
    )
    verify_warm_script("session cache persistence", second_process.returncode)
    if second_value["data"]["step"] != 2:
        raise OfflineError(f"warm workflow step was not 2: {second_value}")

    host_after = {
        "cargo": tree_digest(host_cargo),
        "npm": tree_digest(host_npm),
        "pip": tree_digest(host_pip),
    }
    if host_hashes != host_after:
        raise OfflineError(f"host cache changed during the session: {host_hashes} -> {host_after}")

    _diff_process, diff = invoke(
        binary, warm_workspace, warm_state, ["diff", "--json"]
    )
    diff_paths = [
        change["rel_path"] for change in diff["data"]["changes"]
    ]
    if not any(path.endswith("Cargo.lock") for path in diff_paths):
        raise OfflineError(f"warm workflow diff did not include Cargo.lock: {diff_paths}")
    if not any(path.startswith("installed/") for path in diff_paths):
        raise OfflineError(f"warm workflow diff did not include the pip install: {diff_paths}")

    invoke(binary, warm_workspace, warm_state, ["rollback", "0", "--json"])
    _clean_process, clean = invoke(
        binary, warm_workspace, warm_state, ["diff", "--json"]
    )
    if clean["data"]["changes"]:
        raise OfflineError(f"rollback did not restore the logical fixture: {clean}")
    if tree_digest(warm_workspace) != warm_baseline:
        raise OfflineError("rollback changed the host warm workspace")

    invoke(binary, cold_workspace, cold_state, ["discard", "--yes", "--purge", "--json"])
    invoke(binary, warm_workspace, warm_state, ["discard", "--yes", "--purge", "--json"])

    result = {
        "schema_version": 1,
        "source": {
            "commit": source_commit,
            "dirty": bool(source_status),
        },
        "binary": {
            "path": str(binary),
            "sha256": binary_digest(binary),
            "version": version,
        },
        "environment": {
            "generated_at": datetime.now(timezone.utc).isoformat(timespec="seconds"),
            "platform": platform.platform(),
            "kernel": platform.release(),
            "filesystem": filesystem,
            "python": command_output(["python3", "--version"], repo),
            "cargo": command_output(["cargo", "--version"], repo),
            "node": command_output(["node", "--version"], repo),
            "npm": command_output(["npm", "--version"], repo),
        },
        "policy": {
            "network": "off",
            "host_caches": ["cargo", "npm", "pip"],
        },
        "results": {
            "cold": cold_results,
            "warm": {
                "cargo": True,
                "npm": True,
                "pip": True,
                "session_writes_persisted": True,
                "host_caches_unchanged": True,
                "host_cache_sha256": host_hashes,
            },
            "diff_entries": len(diff_paths),
            "diff_paths": diff_paths,
            "undo_clean": True,
            "host_workspace_unchanged_after_undo": True,
            "cold_host_workspace_unchanged": tree_digest(cold_workspace) == cold_baseline,
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
        shutil.rmtree(root, ignore_errors=True)
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OfflineError, subprocess.CalledProcessError) as error:
        print(f"offline workflow failed: {error}", file=sys.stderr)
        raise SystemExit(1)
