#!/usr/bin/env python3
"""Reproducible wall-clock benchmarks for cowbox CLI scenarios."""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import platform
import shlex
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Sequence


class BenchmarkError(RuntimeError):
    pass


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Measure cowbox M2 CLI scenarios on a local ext4 filesystem."
    )
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument(
        "--baseline-binary",
        type=Path,
        help="baseline binary for alternating first-run A/B",
    )
    parser.add_argument("--root", type=Path)
    parser.add_argument("--file-counts", nargs="+", type=int, default=[10_000, 200_000])
    parser.add_argument(
        "--depths",
        nargs="+",
        type=int,
        help="layer depths for the full single-binary protocol",
    )
    parser.add_argument("--samples", type=int, default=20)
    parser.add_argument("--warmups", type=int, default=1)
    parser.add_argument("--timeout-seconds", type=int, default=600)
    parser.add_argument("--require-fs", default="ext4")
    parser.add_argument("--json-output", type=Path)
    parser.add_argument("--markdown-output", type=Path)
    parser.add_argument("--keep", action="store_true")
    args = parser.parse_args()
    if args.depths is None:
        args.depths = [] if args.baseline_binary is not None else [1, 60, 149]
    elif args.baseline_binary is not None:
        parser.error(
            "--depths cannot be used with --baseline-binary; "
            "paired mode measures only the first run"
        )
    if args.samples < 1:
        parser.error("--samples must be at least 1")
    if args.warmups < 0:
        parser.error("--warmups cannot be negative")
    if any(value < 100 for value in args.file_counts):
        parser.error("--file-counts values must be at least 100")
    if any(value < 1 for value in args.depths):
        parser.error("--depths values must be at least 1")
    if 150 in args.depths:
        parser.error(
            "--depths 150 is not a valid timed sample; use 149 and let the limit probe test 150"
        )
    return args


def run_process(
    argv: Sequence[str],
    cwd: Path,
    env: dict[str, str],
    timeout_seconds: int,
) -> tuple[float, subprocess.CompletedProcess[str]]:
    started = time.perf_counter_ns()
    try:
        process = subprocess.run(
            list(argv),
            cwd=cwd,
            env=env,
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="replace",
            timeout=timeout_seconds,
            check=False,
        )
    except subprocess.TimeoutExpired as error:
        raise BenchmarkError(
            f"command timed out after {timeout_seconds}s: {shlex.join(argv)}"
        ) from error
    elapsed_ms = (time.perf_counter_ns() - started) / 1_000_000
    return elapsed_ms, process


def command_environment(binary: Path, state_root: Path) -> dict[str, str]:
    env = os.environ.copy()
    env["COWBOX_HOME"] = str(state_root)
    env["COWBOX_HELPER_EXE"] = str(binary)
    env["TMPDIR"] = "/tmp"
    env["LC_ALL"] = "C"
    env["LANG"] = "C"
    return env


def invoke_json(
    binary: Path,
    workspace: Path,
    state_root: Path,
    args: Sequence[str],
    timeout_seconds: int,
    expect_success: bool = True,
) -> tuple[float, dict[str, Any], subprocess.CompletedProcess[str]]:
    argv = [str(binary), *args]
    elapsed_ms, process = run_process(
        argv, workspace, command_environment(binary, state_root), timeout_seconds
    )
    try:
        envelope = json.loads(process.stdout)
    except json.JSONDecodeError as error:
        raise BenchmarkError(
            f"non-JSON output from {shlex.join(argv)}: "
            f"exit={process.returncode} stdout={process.stdout!r} stderr={process.stderr!r}"
        ) from error
    if expect_success:
        if process.returncode != 0 or envelope.get("ok") is not True:
            raise BenchmarkError(
                f"command failed: {shlex.join(argv)} exit={process.returncode} "
                f"stdout={process.stdout.strip()!r} stderr={process.stderr.strip()!r}"
            )
    return elapsed_ms, envelope, process


def summarize(values: Sequence[float], include_samples: bool = True) -> dict[str, Any]:
    if not values:
        raise BenchmarkError("cannot summarize an empty sample set")
    ordered = sorted(values)
    count = len(ordered)
    p95_rank = max(1, math.ceil(0.95 * count))
    result: dict[str, Any] = {
        "n": count,
        "median_ms": round(statistics.median(ordered), 3),
        "mean_ms": round(statistics.fmean(ordered), 3),
        "p95_ms": round(ordered[p95_rank - 1], 3),
        "min_ms": round(ordered[0], 3),
        "max_ms": round(ordered[-1], 3),
        "stdev_ms": round(statistics.stdev(ordered), 3) if count > 1 else None,
    }
    if include_samples:
        result["samples_ms"] = [round(value, 3) for value in values]
    return result


def remove_tree(path: Path) -> None:
    if not path.exists():
        return
    for root, directories, files in os.walk(path):
        for name in directories:
            candidate = Path(root) / name
            if not candidate.is_symlink():
                os.chmod(candidate, candidate.stat().st_mode | 0o700)
        for name in files:
            candidate = Path(root) / name
            if not candidate.is_symlink():
                os.chmod(candidate, candidate.stat().st_mode | 0o600)
    os.chmod(path, path.stat().st_mode | 0o700)
    shutil.rmtree(path)


class SampleState:
    def __init__(self, root: Path, label: str, counter: int) -> None:
        self.path = root / f"{label}-{counter:04d}"
        self.path.mkdir(parents=True)

    def __enter__(self) -> Path:
        return self.path

    def __exit__(self, _kind: Any, _value: Any, _traceback: Any) -> None:
        remove_tree(self.path)


def file_path_for_index(index: int) -> Path:
    shard = index // 100
    group = shard // 100
    return Path("src") / f"g{group:03d}" / f"d{shard:04d}" / f"f{index:06d}.txt"


def populate_fixture(root: Path, files: int) -> list[Path]:
    source = root / "src"
    shard_count = math.ceil(files / 100)
    group_count = math.ceil(shard_count / 100)
    for group in range(group_count):
        for shard in range(group * 100, min((group + 1) * 100, shard_count)):
            (source / f"g{group:03d}" / f"d{shard:04d}").mkdir(parents=True)
    payload = "x" * 56
    changed = []
    change_indices = {(index + 1) * files // 11 for index in range(10)}
    for index in range(files):
        path = root / file_path_for_index(index)
        path.write_text(f"{index:08d} {payload}\n", encoding="ascii")
        if index in change_indices:
            changed.append(file_path_for_index(index))
    target = file_path_for_index(0).relative_to("src")
    for index in range(10):
        (source / f"l{index:02d}").symlink_to(target)
    return changed


def filesystem_for(path: Path) -> tuple[str, str]:
    resolved = str(path.resolve())
    best: tuple[int, str, str] | None = None
    for line in Path("/proc/mounts").read_text(encoding="utf-8").splitlines():
        fields = line.split()
        if len(fields) < 3:
            continue
        source, mountpoint, filesystem = fields[0], fields[1], fields[2]
        mountpoint = (
            mountpoint.replace("\\040", " ")
            .replace("\\011", "\t")
            .replace("\\012", "\n")
            .replace("\\134", "\\")
        )
        if resolved == mountpoint or resolved.startswith(mountpoint.rstrip("/") + "/"):
            candidate = (len(mountpoint), filesystem, source)
            if best is None or candidate[0] > best[0]:
                best = candidate
    if best is None:
        raise BenchmarkError(f"cannot determine the filesystem for {path}")
    return best[1], best[2]


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


def binary_metadata(repo: Path, binary: Path) -> dict[str, Any]:
    return {
        "path": str(binary),
        "version": command_output([str(binary), "--version"], repo),
        "sha256": binary_digest(binary),
        "size_bytes": binary.stat().st_size,
    }


def collect_environment(repo: Path, binary: Path, bench_root: Path) -> dict[str, Any]:
    filesystem, source = filesystem_for(bench_root)
    return {
        "generated_at": datetime.now(timezone.utc).isoformat(timespec="seconds"),
        "machine": {
            "platform": platform.platform(),
            "kernel": platform.release(),
            "os_release": read_os_release(),
            "cpu_model": read_cpu_model(),
            "logical_cpus": os.cpu_count(),
            "memory_total_bytes": read_memory_total(),
            "load_average_at_start": [round(value, 3) for value in os.getloadavg()],
        },
        "filesystem": {
            "benchmark_root": str(bench_root),
            "type": filesystem,
            "source": source,
        },
        "source": {
            "git_head": command_output(["git", "rev-parse", "HEAD"], repo),
            "git_describe": command_output(["git", "describe", "--tags", "--always", "--dirty"], repo),
            "git_status": command_output(["git", "status", "--short"], repo),
            "rustc": command_output(["rustc", "--version"], repo),
            "cargo": command_output(["cargo", "--version"], repo),
        },
        "binary": binary_metadata(repo, binary),
    }


def verify_preflight(binary: Path, workspace: Path, state_root: Path, timeout: int) -> None:
    _elapsed, envelope, _process = invoke_json(
        binary, workspace, state_root, ["doctor", "--json"], timeout
    )
    ready = envelope.get("data", {}).get("ready")
    if ready is not True:
        raise BenchmarkError(f"sandbox preflight failed: {json.dumps(envelope, sort_keys=True)}")


def prepare_fixture(
    root: Path,
    files: int,
    binary: Path,
    timeout: int,
) -> tuple[Path, list[Path]]:
    workspace = root / f"workspace-{files}"
    workspace.mkdir(parents=True)
    (workspace / ".git").mkdir()
    changed = populate_fixture(workspace, files)
    with SampleState(root / "setup-states", f"init-{files}", 0) as state:
        invoke_json(binary, workspace, state, ["init", "--json"], timeout)
        verify_preflight(binary, workspace, state, timeout)
    return workspace, changed


def measure_first_run(
    binary: Path,
    workspace: Path,
    state_base: Path,
    label: str,
    samples: int,
    warmups: int,
    timeout: int,
) -> dict[str, Any]:
    counter = 0

    def sample() -> float:
        nonlocal counter
        counter += 1
        with SampleState(state_base, label, counter) as state:
            elapsed, envelope, _process = invoke_json(
                binary, workspace, state, ["run", "--json", "--", "true"], timeout
            )
            if envelope.get("data", {}).get("step") != 1:
                raise BenchmarkError(f"first run did not create step 1: {envelope}")
            return elapsed

    for _ in range(warmups):
        sample()
    return summarize([sample() for _ in range(samples)])


def paired_binary_order(pair_number: int) -> tuple[str, str]:
    if pair_number < 1:
        raise BenchmarkError("paired benchmark sample numbers start at 1")
    if pair_number % 2 == 1:
        return "baseline", "candidate"
    return "candidate", "baseline"


def measure_paired_first_run(
    candidate_binary: Path,
    baseline_binary: Path,
    workspace: Path,
    state_base: Path,
    samples: int,
    warmups: int,
    timeout: int,
) -> dict[str, Any]:
    binaries = {
        "candidate": candidate_binary,
        "baseline": baseline_binary,
    }

    def sample(side: str, phase: str, pair_number: int) -> float:
        with SampleState(state_base, f"{phase}-{side}", pair_number) as state:
            elapsed, envelope, _process = invoke_json(
                binaries[side],
                workspace,
                state,
                ["run", "--json", "--", "true"],
                timeout,
            )
            if envelope.get("data", {}).get("step") != 1:
                raise BenchmarkError(
                    f"{side} first run did not create step 1: {envelope}"
                )
            return elapsed

    for pair_number in range(1, warmups + 1):
        for side in paired_binary_order(pair_number):
            sample(side, "warmup", pair_number)

    pairs = []
    pair_timings = []
    for pair_number in range(1, samples + 1):
        order = paired_binary_order(pair_number)
        pair: dict[str, Any] = {
            "pair": pair_number,
            "order": list(order),
        }
        timings = {
            side: sample(side, "formal", pair_number)
            for side in order
        }
        for side in order:
            pair[side] = {
                "elapsed_ms": round(timings[side], 3),
                "step": 1,
            }
        pairs.append(pair)
        pair_timings.append(timings)

    candidate_samples = [timings["candidate"] for timings in pair_timings]
    baseline_samples = [timings["baseline"] for timings in pair_timings]
    paired_deltas = [
        candidate - baseline
        for candidate, baseline in zip(candidate_samples, baseline_samples, strict=True)
    ]
    return {
        "pairs": pairs,
        "candidate": summarize(candidate_samples),
        "baseline": summarize(baseline_samples),
        "paired_delta_candidate_minus_baseline": summarize(paired_deltas),
    }


def benchmark_paired_first_run(
    candidate_binary: Path,
    baseline_binary: Path,
    bench_root: Path,
    file_counts: Sequence[int],
    samples: int,
    warmups: int,
    timeout: int,
) -> list[dict[str, Any]]:
    results = []
    for files in file_counts:
        print(f"[paired] preparing shared {files:,}-file fixture", flush=True)
        workspace, _changed = prepare_fixture(
            bench_root / "fixtures", files, candidate_binary, timeout
        )
        with SampleState(
            bench_root / "setup-states", f"baseline-preflight-{files}", 0
        ) as state:
            verify_preflight(baseline_binary, workspace, state, timeout)
        state_base = bench_root / "sample-states" / f"paired-{files}"
        state_base.mkdir(parents=True)
        paired = measure_paired_first_run(
            candidate_binary,
            baseline_binary,
            workspace,
            state_base,
            samples,
            warmups,
            timeout,
        )
        results.append(
            {
                "files": files,
                **paired,
            }
        )
        shutil.rmtree(workspace)
        shutil.rmtree(state_base)
        print(
            f"[paired] {files:,}: baseline={paired['baseline']['median_ms']:.2f} ms "
            f"candidate={paired['candidate']['median_ms']:.2f} ms "
            f"delta={paired['paired_delta_candidate_minus_baseline']['median_ms']:.2f} ms",
            flush=True,
        )
    return results


def measure_subsequent_run(
    binary: Path,
    workspace: Path,
    state_base: Path,
    label: str,
    samples: int,
    warmups: int,
    timeout: int,
) -> dict[str, Any]:
    counter = 0
    measured: list[float] = []
    preparation: list[float] = []

    def sample() -> tuple[float, float]:
        nonlocal counter
        counter += 1
        with SampleState(state_base, label, counter) as state:
            prep_ms, first, _process = invoke_json(
                binary, workspace, state, ["run", "--json", "--", "true"], timeout
            )
            if first.get("data", {}).get("step") != 1:
                raise BenchmarkError(f"preparation run did not create step 1: {first}")
            elapsed, second, _process = invoke_json(
                binary, workspace, state, ["run", "--json", "--", "true"], timeout
            )
            if second.get("data", {}).get("step") != 2:
                raise BenchmarkError(f"subsequent run did not create step 2: {second}")
            return prep_ms, elapsed

    for _ in range(warmups):
        sample()
    for _ in range(samples):
        prep_ms, elapsed = sample()
        preparation.append(prep_ms)
        measured.append(elapsed)
    return {
        "parent_layers_before_step": 1,
        "step": summarize(measured),
        "preparation": summarize(preparation),
    }


def make_change_script(changed: Sequence[Path]) -> str:
    commands = [
        f"printf 'changed {index}\\n' > {shlex.quote(str(path))}"
        for index, path in enumerate(changed)
    ]
    return " && ".join(commands)


def measure_diff(
    binary: Path,
    workspace: Path,
    state_base: Path,
    label: str,
    changed: Sequence[Path],
    samples: int,
    warmups: int,
    timeout: int,
) -> dict[str, Any]:
    if len(changed) != 10:
        raise BenchmarkError(f"diff benchmark requires 10 changed paths, got {len(changed)}")
    originals = {path: (workspace / path).read_bytes() for path in changed}
    change_script = make_change_script(changed)
    counter = 0
    measured: list[float] = []
    preparation: list[float] = []

    def verify_host() -> None:
        for path, expected in originals.items():
            actual = (workspace / path).read_bytes()
            if actual != expected:
                raise BenchmarkError(f"host workspace changed during diff benchmark: {path}")

    def sample() -> tuple[float, float]:
        nonlocal counter
        counter += 1
        with SampleState(state_base, label, counter) as state:
            first_ms, first, _process = invoke_json(
                binary, workspace, state, ["run", "--json", "--", "true"], timeout
            )
            if first.get("data", {}).get("step") != 1:
                raise BenchmarkError(f"diff preparation did not create step 1: {first}")
            change_ms, change, _process = invoke_json(
                binary,
                workspace,
                state,
                ["run", "--json", "--", "sh", "-c", change_script],
                timeout,
            )
            if change.get("data", {}).get("step") != 2:
                raise BenchmarkError(f"diff preparation did not create step 2: {change}")
            verify_host()
            elapsed, diff, _process = invoke_json(
                binary, workspace, state, ["diff", "--json"], timeout
            )
            changes = diff.get("data", {}).get("changes", [])
            if len(changes) != 10:
                raise BenchmarkError(f"expected 10 diff changes, got {len(changes)}: {diff}")
            if any(change.get("kind") != "modified" for change in changes):
                raise BenchmarkError(f"expected only modified files in diff: {diff}")
            verify_host()
            return first_ms + change_ms, elapsed

    for _ in range(warmups):
        sample()
    for _ in range(samples):
        prep_ms, elapsed = sample()
        preparation.append(prep_ms)
        measured.append(elapsed)
    return {
        "changed_files": 10,
        "step": summarize(measured),
        "preparation": summarize(preparation),
    }


def benchmark_file_counts(
    binary: Path,
    bench_root: Path,
    file_counts: Sequence[int],
    samples: int,
    warmups: int,
    timeout: int,
) -> list[dict[str, Any]]:
    results = []
    for files in file_counts:
        print(f"[file-count] preparing {files:,} files", flush=True)
        workspace, changed = prepare_fixture(
            bench_root / "fixtures", files, binary, timeout
        )
        state_base = bench_root / "sample-states" / f"files-{files}"
        state_base.mkdir(parents=True)
        first = measure_first_run(
            binary,
            workspace,
            state_base,
            "first",
            samples,
            warmups,
            timeout,
        )
        subsequent = measure_subsequent_run(
            binary,
            workspace,
            state_base,
            "subsequent",
            samples,
            warmups,
            timeout,
        )
        diff = measure_diff(
            binary,
            workspace,
            state_base,
            "diff",
            changed,
            samples,
            warmups,
            timeout,
        )
        results.append(
            {
                "files": files,
                "first_run": first,
                "subsequent_run": subsequent,
                "diff_10_changes": diff,
            }
        )
        shutil.rmtree(workspace)
        shutil.rmtree(state_base)
        print(
            f"[file-count] {files:,}: first={first['median_ms']:.2f} ms "
            f"subsequent={subsequent['step']['median_ms']:.2f} ms "
            f"diff={diff['step']['median_ms']:.2f} ms",
            flush=True,
        )
    return results


def measure_depth(
    binary: Path,
    workspace: Path,
    state_base: Path,
    depth: int,
    samples: int,
    warmups: int,
    timeout: int,
) -> dict[str, Any]:
    counter = 0
    measured: list[float] = []
    preparation_totals: list[float] = []
    preparation_steps: list[float] = []

    def sample() -> tuple[list[float], float]:
        nonlocal counter
        counter += 1
        prep_steps = []
        with SampleState(state_base, f"depth-{depth}", counter) as state:
            for expected_step in range(1, depth + 1):
                elapsed, envelope, _process = invoke_json(
                    binary, workspace, state, ["run", "--json", "--", "true"], timeout
                )
                actual_step = envelope.get("data", {}).get("step")
                if actual_step != expected_step:
                    raise BenchmarkError(
                        f"depth {depth}: expected step {expected_step}, got {actual_step}"
                    )
                prep_steps.append(elapsed)
            elapsed, envelope, _process = invoke_json(
                binary, workspace, state, ["run", "--json", "--", "true"], timeout
            )
            actual_step = envelope.get("data", {}).get("step")
            if actual_step != depth + 1:
                raise BenchmarkError(
                    f"depth {depth}: expected measured step {depth + 1}, got {actual_step}"
                )
        return prep_steps, elapsed

    for _ in range(warmups):
        sample()
    for _ in range(samples):
        prep_steps, elapsed = sample()
        preparation_totals.append(sum(prep_steps))
        preparation_steps.extend(prep_steps)
        measured.append(elapsed)
    return {
        "parent_layers_before_step": depth,
        "head_after_step": depth + 1,
        "step": summarize(measured),
        "preparation_total": summarize(preparation_totals),
        "preparation_step": summarize(preparation_steps, include_samples=False),
    }


def benchmark_depths(
    binary: Path,
    workspace: Path,
    bench_root: Path,
    depths: Sequence[int],
    samples: int,
    warmups: int,
    timeout: int,
) -> list[dict[str, Any]]:
    state_base = bench_root / "sample-states" / "depths"
    state_base.mkdir(parents=True)
    results = []
    for depth in depths:
        print(f"[depth] preparing {depth} parent layers per sample", flush=True)
        result = measure_depth(
            binary, workspace, state_base, depth, samples, warmups, timeout
        )
        results.append(result)
        print(
            f"[depth] {depth}: step={result['step']['median_ms']:.2f} ms "
            f"prep={result['preparation_total']['median_ms']:.1f} ms",
            flush=True,
        )
    shutil.rmtree(state_base)
    return results


def probe_layer_limit(
    binary: Path,
    workspace: Path,
    state_base: Path,
    timeout: int,
) -> dict[str, Any]:
    print("[limit] preparing 150 active layers", flush=True)
    state_base.mkdir(parents=True)
    with SampleState(state_base, "limit", 0) as state:
        for expected_step in range(1, 151):
            _elapsed, envelope, _process = invoke_json(
                binary, workspace, state, ["run", "--json", "--", "true"], timeout
            )
            actual_step = envelope.get("data", {}).get("step")
            if actual_step != expected_step:
                raise BenchmarkError(
                    f"limit setup: expected step {expected_step}, got {actual_step}"
                )
        success_ms, success, _process = invoke_json(
            binary, workspace, state, ["run", "--json", "--", "true"], timeout
        )
        if success.get("data", {}).get("step") != 151:
            raise BenchmarkError(
                f"step from 150 parent layers did not create step 151: {success}"
            )
        rejected_ms, rejected, failed_process = invoke_json(
            binary,
            workspace,
            state,
            ["run", "--json", "--", "true"],
            timeout,
            expect_success=False,
        )
        error = rejected.get("error", {})
        if failed_process.returncode != 2 or rejected.get("ok") is not False:
            raise BenchmarkError(
                "step from 151 parent layers did not fail with a JSON error: "
                f"exit={failed_process.returncode} output={rejected}"
            )
        if error.get("code") != "CONFIG_ERROR":
            raise BenchmarkError(f"unexpected layer-limit error: {rejected}")
        _log_ms, log, _process = invoke_json(
            binary, workspace, state, ["log", "--json"], timeout
        )
        dag = log.get("data", {}).get("dag", {})
        session_id = dag.get("session_id")
        head_step = dag.get("head_step")
        if head_step != 151 or not isinstance(session_id, str):
            raise BenchmarkError(f"session head changed after rejected step: {log}")
        step_152 = state / "sessions" / session_id / "steps" / "152"
        if step_152.exists():
            raise BenchmarkError(f"rejected step left state behind: {step_152}")
        result = {
            "active_layers_before_success": 150,
            "success_from_150_parent_layers": {
                "elapsed_ms": round(success_ms, 3),
                "head_after_step": 151,
            },
            "rejection_from_151_parent_layers": {
                "elapsed_ms": round(rejected_ms, 3),
                "exit_code": failed_process.returncode,
                "error_code": error.get("code"),
                "error_message": error.get("message"),
                "head_after_rejection": head_step,
                "step_152_created": step_152.exists(),
            },
        }
    shutil.rmtree(state_base)
    return result


def paired_result_markdown(result: dict[str, Any]) -> str:
    lines = [
        "# cowbox M2 paired first-run benchmark",
        "",
        f"Generated: `{result['generated_at']}`",
        "",
        "## Configuration",
        "",
        f"- Candidate binary: `{result['binary']['path']}`",
        f"- Candidate version: `{result['binary']['version']}`",
        f"- Candidate SHA-256: `{result['binary']['sha256']}`",
        f"- Baseline binary: `{result['baseline_binary']['path']}`",
        f"- Baseline version: `{result['baseline_binary']['version']}`",
        f"- Baseline SHA-256: `{result['baseline_binary']['sha256']}`",
        f"- Git HEAD: `{result['source']['git_head']}`",
        f"- Kernel: `{result['machine']['kernel']}`",
        f"- Filesystem: `{result['filesystem']['type']}`",
        f"- Paired samples per file count: {result['settings']['samples']}",
        f"- Warm-up pairs per file count: {result['settings']['warmups']}",
        "- Scope: first run only; every side starts with a fresh isolated state",
        "- Pair order: odd pairs run baseline then candidate; even pairs run candidate then baseline",
        "- Paired delta: candidate minus baseline",
        "",
        "## First-run pairs",
        "",
        "| Files | Baseline median (P95) | Candidate median (P95) | Paired delta median (P95) |",
        "| ---: | ---: | ---: | ---: |",
    ]
    for group in result["paired_first_run"]:
        baseline = group["baseline"]
        candidate = group["candidate"]
        delta = group["paired_delta_candidate_minus_baseline"]
        lines.append(
            f"| {group['files']:,} "
            f"| {baseline['median_ms']:.2f} ms ({baseline['p95_ms']:.2f} ms) "
            f"| {candidate['median_ms']:.2f} ms ({candidate['p95_ms']:.2f} ms) "
            f"| {delta['median_ms']:+.2f} ms ({delta['p95_ms']:+.2f} ms) |"
        )
    return "\n".join(lines) + "\n"


def result_markdown(result: dict[str, Any]) -> str:
    if "paired_first_run" in result:
        return paired_result_markdown(result)
    lines = [
        "# cowbox M2 benchmark run",
        "",
        f"Generated: `{result['generated_at']}`",
        "",
        "## Configuration",
        "",
        f"- Binary: `{result['binary']['path']}`",
        f"- Version: `{result['binary']['version']}`",
        f"- SHA-256: `{result['binary']['sha256']}`",
        f"- Git HEAD: `{result['source']['git_head']}`",
        f"- Kernel: `{result['machine']['kernel']}`",
        f"- Filesystem: `{result['filesystem']['type']}`",
        f"- Samples per scenario: {result['settings']['samples']}",
        f"- Warm-up samples per scenario: {result['settings']['warmups']}",
        "",
        "## File counts",
        "",
        "| Files | First run median (P95) | Subsequent run at 1 parent layer median (P95) | 10-file diff median (P95) |",
        "| ---: | ---: | ---: | ---: |",
    ]
    for group in result.get("file_counts", []):
        first = group["first_run"]
        subsequent = group["subsequent_run"]["step"]
        diff = group["diff_10_changes"]["step"]
        lines.append(
            f"| {group['files']:,} | {first['median_ms']:.2f} ms ({first['p95_ms']:.2f} ms) "
            f"| {subsequent['median_ms']:.2f} ms ({subsequent['p95_ms']:.2f} ms) "
            f"| {diff['median_ms']:.2f} ms ({diff['p95_ms']:.2f} ms) |"
        )
    lines.extend(
        [
            "",
            "## Layer depths",
            "",
            "| Parent layers before step | Preparation median | Preparation step median | Measured step median (P95) |",
            "| ---: | ---: | ---: | ---: |",
        ]
    )
    for group in result.get("depths", []):
        prep_total = group["preparation_total"]
        prep_step = group["preparation_step"]
        step = group["step"]
        lines.append(
            f"| {group['parent_layers_before_step']} | {prep_total['median_ms'] / 1000:.2f} s "
            f"| {prep_step['median_ms']:.2f} ms "
            f"| {step['median_ms']:.2f} ms ({step['p95_ms']:.2f} ms) |"
        )
    limit = result.get("layer_limit")
    if limit:
        success = limit["success_from_150_parent_layers"]
        rejected = limit["rejection_from_151_parent_layers"]
        lines.extend(
            [
                "",
                "## Layer limit probe",
                "",
                f"- A step with 150 parent layers succeeded in {success['elapsed_ms']:.2f} ms "
                f"and created head step {success['head_after_step']}.",
                f"- The next step, with 151 parent layers, exited {rejected['exit_code']} "
                f"with `{rejected['error_code']}` after {rejected['elapsed_ms']:.2f} ms; "
                f"head remained {rejected['head_after_rejection']} and step 152 was not created.",
                f"- Error: `{rejected['error_message']}`",
            ]
        )
    return "\n".join(lines) + "\n"


def write_outputs(result: dict[str, Any], args: argparse.Namespace) -> None:
    payload = json.dumps(result, indent=2, sort_keys=True) + "\n"
    if args.json_output:
        args.json_output.parent.mkdir(parents=True, exist_ok=True)
        args.json_output.write_text(payload, encoding="utf-8")
    if args.markdown_output:
        args.markdown_output.parent.mkdir(parents=True, exist_ok=True)
        args.markdown_output.write_text(result_markdown(result), encoding="utf-8")


def main() -> int:
    args = parse_args()
    repo = Path(__file__).resolve().parents[1]
    binary = args.binary.resolve()
    if not binary.is_file() or not os.access(binary, os.X_OK):
        raise BenchmarkError(f"cowbox binary is missing or not executable: {binary}")
    baseline_binary = None
    if args.baseline_binary is not None:
        baseline_binary = args.baseline_binary.resolve()
        if not baseline_binary.is_file() or not os.access(baseline_binary, os.X_OK):
            raise BenchmarkError(
                "--baseline-binary is missing or not executable: "
                f"{baseline_binary}"
            )
    if args.root:
        bench_root = args.root.resolve()
        if bench_root.exists() and any(bench_root.iterdir()):
            raise BenchmarkError(f"benchmark root must be empty: {bench_root}")
        bench_root.mkdir(parents=True, exist_ok=True)
    else:
        bench_root = Path(tempfile.mkdtemp(prefix="cowbox-m2-", dir="/tmp"))
    for output in (args.json_output, args.markdown_output):
        if output is None:
            continue
        resolved = output.resolve()
        if resolved == bench_root or bench_root in resolved.parents:
            raise BenchmarkError(
                f"output path must be outside the disposable benchmark root: {resolved}"
            )
    filesystem, source = filesystem_for(bench_root)
    if filesystem != args.require_fs:
        raise BenchmarkError(
            f"benchmark root {bench_root} is on {filesystem} ({source}), "
            f"expected {args.require_fs}"
        )
    result: dict[str, Any] = {
        "schema_version": 1,
        "settings": {
            "samples": args.samples,
            "warmups": args.warmups,
            "file_counts": args.file_counts,
            "depths": args.depths,
            "require_fs": args.require_fs,
            "timeout_seconds": args.timeout_seconds,
            "clock": "perf_counter around each CLI subprocess; fixture generation and cleanup excluded",
            "percentile": "nearest-rank",
        },
    }
    if baseline_binary is not None:
        result["settings"]["mode"] = "paired_first_run"
    result.update(collect_environment(repo, binary, bench_root))
    if baseline_binary is not None:
        result["baseline_binary"] = binary_metadata(repo, baseline_binary)
        print(
            "cowbox M2 paired first-run benchmark: "
            f"candidate_sha256={result['binary']['sha256'][:12]} "
            f"baseline_sha256={result['baseline_binary']['sha256'][:12]} "
            f"fs={filesystem}",
            flush=True,
        )
    else:
        print(
            f"cowbox M2 benchmark: {result['binary']['version']} "
            f"sha256={result['binary']['sha256'][:12]} fs={filesystem}",
            flush=True,
        )

    succeeded = False
    try:
        if baseline_binary is not None:
            result["paired_first_run"] = benchmark_paired_first_run(
                binary,
                baseline_binary,
                bench_root,
                args.file_counts,
                args.samples,
                args.warmups,
                args.timeout_seconds,
            )
        else:
            result["file_counts"] = benchmark_file_counts(
                binary,
                bench_root,
                args.file_counts,
                args.samples,
                args.warmups,
                args.timeout_seconds,
            )
            depth_file_count = 10_000
            print(
                f"[depth] preparing shared {depth_file_count:,}-file fixture",
                flush=True,
            )
            fixture_workspace, _changed = prepare_fixture(
                bench_root / "fixtures",
                depth_file_count,
                binary,
                args.timeout_seconds,
            )
            result["depths"] = benchmark_depths(
                binary,
                fixture_workspace,
                bench_root,
                args.depths,
                args.samples,
                args.warmups,
                args.timeout_seconds,
            )
            result["layer_limit"] = probe_layer_limit(
                binary,
                fixture_workspace,
                bench_root / "sample-states" / "limit",
                args.timeout_seconds,
            )
        result["machine"]["load_average_at_end"] = [
            round(value, 3) for value in os.getloadavg()
        ]
        write_outputs(result, args)
        print(result_markdown(result), end="")
        succeeded = True
        return 0
    except Exception:
        print(f"benchmark root kept for inspection: {bench_root}", file=sys.stderr)
        raise
    finally:
        if not args.keep and succeeded:
            shutil.rmtree(bench_root)


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except BenchmarkError as error:
        print(f"benchmark failed: {error}", file=sys.stderr)
        raise SystemExit(1)
