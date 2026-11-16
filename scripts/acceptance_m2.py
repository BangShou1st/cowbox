#!/usr/bin/env python3
"""Real-CLI M2 long-session acceptance on an ext4 fixture."""

from __future__ import annotations

import argparse
import json
import os
import shutil
import signal
import subprocess
import sys
import tempfile
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
            "Run the M2 300-step real-change acceptance with a debug cowbox "
            "binary so deterministic crash injection is available."
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
) -> dict[str, Any]:
    _process, value = invoke(
        binary,
        workspace,
        state,
        ["run", "--json", "--", "sh", "-c", script],
        extra_env=extra_env,
    )
    return value


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


def main() -> int:
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

    workspace = root / "workspace"
    workspace.mkdir()
    write_fixture(workspace)
    state = state_for(root, "state")

    invoke(binary, workspace, state, ["init", "--json"])
    first = run(binary, workspace, state, MUTATION)
    if first["data"]["step"] != 1:
        raise AcceptanceError(f"mutation did not create step 1: {first}")
    _process, before = invoke(binary, workspace, state, ["diff", "--json"])
    before_changes = before["data"]["changes"]
    if not before_changes:
        raise AcceptanceError("real-change fixture produced an empty diff")

    compaction_events: list[dict[str, Any]] = []
    view_steps = {150, 152, 269, 300}
    for step in range(2, args.steps + 1):
        if step in view_steps:
            result = run(binary, workspace, state, VIEW)
        else:
            result = run(binary, workspace, state, "true")
        if result["data"]["step"] != step:
            raise AcceptanceError(f"expected step {step}, got {result}")
        if step in {151, 268, args.steps}:
            _stats_process, stats = invoke(
                binary, workspace, state, ["stats", "--json"]
            )
            if step in {151, 268}:
                compaction_events.append(
                    {
                        "step": step,
                        "compact_layers": stats["data"]["compact_layers"],
                        "mounted_layers": stats["data"]["mounted_layers"],
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
    branch = run(binary, workspace, state, "true")
    if branch["data"]["step"] != 301:
        raise AcceptanceError(f"branch did not create the next DAG step: {branch}")
    _process, branched = invoke(binary, workspace, state, ["diff", "--json"])
    assert_diff_equal(before, branched, "rollback and branch")
    run(binary, workspace, state, VIEW)

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
        "binary": str(binary),
        "filesystem": filesystem,
        "steps": args.steps,
        "diff_entries": len(before_changes),
        "compaction_events": compaction_events,
        "final_stats": final_stats["data"],
        "rollback_head": rollback["data"]["head"],
        "branch_step": branch["data"]["step"],
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
