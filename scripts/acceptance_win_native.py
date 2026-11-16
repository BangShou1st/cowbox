#!/usr/bin/env python3
"""Native Windows NTFS snapshot-editing acceptance.

Run with a Windows Python 3.12+ interpreter. The script creates disposable
fixtures below --root and --cross-volume-root, drives the real cowbox.exe, and
records wall-clock timings, state growth, and free-space changes as JSON.
"""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import shutil
import subprocess
import time


def disk_free(path: pathlib.Path) -> int:
    return shutil.disk_usage(path).free


def free_space() -> dict[str, int]:
    result = {}
    for drive in ("C:\\", "D:\\", "E:\\"):
        result[drive[0]] = disk_free(pathlib.Path(drive))
    return result


def tree_size(root: pathlib.Path) -> int:
    total = 0
    if not root.exists():
        return 0
    stack = [root]
    while stack:
        directory = stack.pop()
        try:
            entries = list(os.scandir(directory))
        except OSError:
            continue
        for entry in entries:
            try:
                if entry.is_junction():
                    continue
                if entry.is_dir(follow_symlinks=False):
                    stack.append(pathlib.Path(entry.path))
                elif entry.is_file(follow_symlinks=False):
                    total += entry.stat(follow_symlinks=False).st_size
            except OSError:
                continue
    return total


def write(path: pathlib.Path, text: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8", newline="\n")


def generate_project(root: pathlib.Path, count: int) -> None:
    write(root / ".git" / "config", "[core]\n")
    write(root / "README.md", "fixture\n")
    for index in range(count):
        write(
            root / "src" / f"bucket-{index % 100:02d}" / f"file-{index:05d}.txt",
            f"file-{index:05d}-payload\n",
        )


class Cowbox:
    def __init__(self, executable: pathlib.Path, index_root: pathlib.Path):
        self.executable = executable
        self.env = os.environ.copy()
        self.env["COWBOX_WIN_HOME"] = str(index_root)

    def run(self, args: list[str], cwd: pathlib.Path | None = None) -> tuple[int, dict]:
        output = subprocess.run(
            [str(self.executable), *args],
            cwd=cwd,
            env=self.env,
            text=True,
            capture_output=True,
            encoding="utf-8",
            errors="replace",
        )
        try:
            value = json.loads(output.stdout)
        except json.JSONDecodeError as error:
            raise RuntimeError(
                f"command {args} returned no JSON\nstdout={output.stdout}\nstderr={output.stderr}"
            ) from error
        return output.returncode, value

    def ok(self, args: list[str], cwd: pathlib.Path | None = None) -> dict:
        code, value = self.run(args, cwd)
        if code != 0 or not value.get("ok"):
            raise RuntimeError(f"command {args} failed: {json.dumps(value, indent=2)}")
        return value


def measure(operation):
    started = time.perf_counter()
    value = operation()
    return value, round((time.perf_counter() - started) * 1000.0, 3)


def session_commands(cowbox: Cowbox, session: str, state_root: pathlib.Path) -> list[str]:
    return ["--session", session, "--state-root", str(state_root)]


def run_scale_fixture(
    cowbox: Cowbox,
    root: pathlib.Path,
    count: int,
) -> dict:
    name = f"{count // 1000}k" if count >= 1000 else str(count)
    fixture = root / f"fixture-{name}"
    state = root / f"state-{name}"
    if fixture.exists():
        shutil.rmtree(fixture)
    if state.exists():
        shutil.rmtree(state)
    generate_project(fixture, count)

    init_result, init_ms = measure(
        lambda: cowbox.ok(
            [
                "win",
                "init",
                "--workspace",
                str(fixture),
                "--state-root",
                str(state),
                "--json",
            ]
        )
    )
    session = init_result["data"]["session_id"]
    managed = pathlib.Path(init_result["data"]["managed_workspace"])
    session_args = session_commands(cowbox, session, state)
    initial_state_bytes = tree_size(state)

    write(managed / "src" / "bucket-00" / "file-00000.txt", "changed-at-checkpoint\n")
    checkpoint_result, checkpoint_ms = measure(
        lambda: cowbox.ok(["win", "checkpoint", *session_args, "--json"])
    )
    checkpoint_state_bytes = tree_size(state)

    diff_result, diff_ms = measure(
        lambda: cowbox.ok(["win", "diff", *session_args, "--json"])
    )
    rollback_result, rollback_ms = measure(
        lambda: cowbox.ok(["win", "rollback", "0", *session_args, "--json"])
    )
    managed = pathlib.Path(rollback_result["data"]["managed_workspace"])

    for index in range(3):
        write(
            managed / "src" / f"bucket-{index:02d}" / f"file-{index:05d}.txt",
            f"committed-{index}\n",
        )
    commit_checkpoint, commit_checkpoint_ms = measure(
        lambda: cowbox.ok(["win", "checkpoint", *session_args, "--json"])
    )
    committed_step = int(commit_checkpoint["data"]["step"])
    commit_result, commit_ms = measure(
        lambda: cowbox.ok(
            ["win", "commit", *session_args, "--step", str(committed_step), "--yes", "--json"]
        )
    )
    for index in range(3):
        actual = (
            fixture / "src" / f"bucket-{index:02d}" / f"file-{index:05d}.txt"
        ).read_text(encoding="utf-8")
        if actual != f"committed-{index}\n":
            raise AssertionError(f"host content mismatch at index {index}: {actual!r}")

    return {
        "files": count,
        "session": session,
        "workspace": str(fixture),
        "state_root": str(state),
        "managed_workspace": str(managed),
        "init_ms": init_ms,
        "checkpoint_ms": checkpoint_ms,
        "diff_ms": diff_ms,
        "rollback_ms": rollback_ms,
        "commit_checkpoint_ms": commit_checkpoint_ms,
        "commit_ms": commit_ms,
        "initial_state_bytes": initial_state_bytes,
        "checkpoint_state_bytes": checkpoint_state_bytes,
        "checkpoint_growth_bytes": checkpoint_state_bytes - initial_state_bytes,
        "diff_changes": len(diff_result["data"]["changes"]),
        "committed_step": committed_step,
        "committed": commit_result["data"]["committed"],
        "checkpoint_step": checkpoint_result["data"]["step"],
    }


def run_growth_fixture(cowbox: Cowbox, root: pathlib.Path) -> dict:
    fixture = root / "fixture-growth"
    state = root / "state-growth"
    if fixture.exists():
        shutil.rmtree(fixture)
    if state.exists():
        shutil.rmtree(state)
    generate_project(fixture, 100)
    init_result = cowbox.ok(
        [
            "win",
            "init",
            "--workspace",
            str(fixture),
            "--state-root",
            str(state),
            "--json",
        ]
    )
    session = init_result["data"]["session_id"]
    session_args = session_commands(cowbox, session, state)
    managed = pathlib.Path(init_result["data"]["managed_workspace"])
    samples = [{"step": 0, "state_bytes": tree_size(state)}]
    for index in range(10):
        target = managed / "src" / f"growth-{index:02d}.txt"
        write(target, f"growth-{index}-" + ("x" * 32) + "\n")
        result = cowbox.ok(["win", "checkpoint", *session_args, "--json"])
        samples.append(
            {
                "step": int(result["data"]["step"]),
                "state_bytes": tree_size(state),
            }
        )
    before_discard = tree_size(state)
    discard_result = cowbox.ok(
        ["win", "discard", *session_args, "--yes", "--json"]
    )
    after_discard = tree_size(state)
    return {
        "files": 100,
        "session": session,
        "samples": samples,
        "growth_deltas": [
            samples[index + 1]["state_bytes"] - samples[index]["state_bytes"]
            for index in range(len(samples) - 1)
        ],
        "before_discard_bytes": before_discard,
        "after_discard_bytes": after_discard,
        "discard_reported_freed_bytes": discard_result["data"]["freed_bytes"],
        "actual_freed_bytes": before_discard - after_discard,
    }


def run_cross_volume(cowbox: Cowbox, cross_root: pathlib.Path, state_root: pathlib.Path) -> dict:
    fixture = cross_root / "project"
    state = state_root / "cross-volume"
    if fixture.exists():
        shutil.rmtree(fixture)
    if state.exists():
        shutil.rmtree(state)
    write(fixture / "src" / "hello.txt", "cross-0\n")
    write(fixture / "keep.txt", "keep\n")
    init_result, init_ms = measure(
        lambda: cowbox.ok(
            [
                "win",
                "init",
                "--workspace",
                str(fixture),
                "--state-root",
                str(state),
                "--json",
            ]
        )
    )
    session = init_result["data"]["session_id"]
    session_args = session_commands(cowbox, session, state)
    managed = pathlib.Path(init_result["data"]["managed_workspace"])
    write(managed / "src" / "hello.txt", "cross-1\n")
    checkpoint_result, checkpoint_ms = measure(
        lambda: cowbox.ok(["win", "checkpoint", *session_args, "--json"])
    )
    commit_result, commit_ms = measure(
        lambda: cowbox.ok(["win", "commit", *session_args, "--yes", "--json"])
    )
    actual = (fixture / "src" / "hello.txt").read_text(encoding="utf-8")
    if actual != "cross-1\n":
        raise AssertionError(f"cross-volume content mismatch: {actual!r}")
    return {
        "workspace": str(fixture),
        "state_root": str(state),
        "workspace_drive": fixture.drive,
        "state_drive": state.drive,
        "session": session,
        "init_ms": init_ms,
        "checkpoint_ms": checkpoint_ms,
        "commit_ms": commit_ms,
        "checkpoint_step": checkpoint_result["data"]["step"],
        "committed": commit_result["data"]["committed"],
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--exe", required=True)
    parser.add_argument("--root", required=True)
    parser.add_argument("--cross-volume-root", required=True)
    parser.add_argument("--json-output", required=True)
    parser.add_argument("--skip-10k", action="store_true")
    args = parser.parse_args()

    executable = pathlib.Path(args.exe).resolve()
    root = pathlib.Path(args.root).resolve()
    cross_root = pathlib.Path(args.cross_volume_root).resolve()
    root.mkdir(parents=True, exist_ok=True)
    cross_root.mkdir(parents=True, exist_ok=True)
    index_root = root / "index"
    cowbox = Cowbox(executable, index_root)

    version = subprocess.run(
        [str(executable), "--version"], capture_output=True, text=True, check=True
    ).stdout.strip()
    free_before = free_space()
    report = {
        "schema_version": 1,
        "binary": str(executable),
        "version": version,
        "root": str(root),
        "cross_volume_root": str(cross_root),
        "free_before": free_before,
        "scale": [],
    }

    report["scale"].append(run_scale_fixture(cowbox, root, 1000))
    if not args.skip_10k:
        report["scale"].append(run_scale_fixture(cowbox, root, 10000))
    report["growth"] = run_growth_fixture(cowbox, root)
    report["cross_volume"] = run_cross_volume(cowbox, cross_root, root)
    report["free_after"] = free_space()
    report["free_delta"] = {
        drive: report["free_after"][drive] - free_before[drive]
        for drive in free_before
    }
    output = pathlib.Path(args.json_output).resolve()
    output.write_text(
        json.dumps(report, indent=2, ensure_ascii=False) + "\n",
        encoding="utf-8",
        newline="\n",
    )
    print(json.dumps(report, indent=2, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
