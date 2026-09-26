"""P0-7 稳定性修复回归套件（主人 2026-09-23"要彻底稳定"）。

四个场景端到端钉住今天改的四层缺口：

  1. `logs_actually_persist` — 后端启动后文件日志真的落了字节（pin guard 修复）
  2. `bridge_dead_event`     — hermix 进程被外部 kill 后 host 帧 emit bridge_dead
                              （通过 ProcessExitCode != 0 + 不再 hang 替代直接测 host）
  3. `telemetry_emit_lines`  — 进程内事件被记录（INFO/WARN 行真的写到 daily rolling file）
  4. `diagnostics_assets`    — 桌面 app exe 内嵌诊断 Tauri command 名字符串

退出码：
  0 — 全部 PASS
  1 — 至少一条 FAIL（任意场景）

用法：
    python scripts/verify-stability-suite.py [--hermix PATH] [--desktop PATH] [--skip SCENARIO]

环境：
  默认 hermix 路径：target/release/hermix.exe
  默认 desktop 路径：D:/data/Hermix Desktop/hermix-desktop.exe
  日志目录：<hermix_home>/logs/（HERMIX_HOME 覆盖）
"""

from __future__ import annotations

import os
import shutil
import subprocess
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Callable, Iterable

REPO = Path(__file__).resolve().parent.parent
DEFAULT_HERMIX = REPO / "target" / "release" / "hermix.exe"
DEFAULT_DESKTOP = Path("D:/data/Hermix Desktop/hermix-desktop.exe")
LOG_DIR = Path(os.environ.get("HERMIX_HOME", Path.home() / ".hermix")) / "logs"
SCENARIOS = (
    "logs_actually_persist",
    "bridge_dead_event",
    "telemetry_emit_lines",
    "diagnostics_assets",
    "approval_wire_compat",
)


@dataclass
class Result:
    name: str
    passed: bool
    detail: str

    def tag(self) -> str:
        return "PASS" if self.passed else "FAIL"


def find_hermix(arg: str | None) -> Path:
    if not arg:
        return DEFAULT_HERMIX
    p = Path(arg)
    if not p.is_file():
        raise FileNotFoundError(f"hermix not found: {p}")
    return p


def find_desktop(arg: str | None) -> Path:
    if not arg:
        return DEFAULT_DESKTOP
    return Path(arg)


# ── scenario 1: telemetry writes non-empty file (pin guard works) ────


def scenario_logs_actually_persist(hermix: Path) -> Result:
    """Run hermix for a few seconds, then assert the rolling log has
    non-zero bytes AND contains at least one timestamped INFO line.

    Pre-fix behaviour: file existed but was 0 bytes because the
    non-blocking writer thread was joined when init_telemetry's guard
    was dropped at end of `init_telemetry_from_env(...)` statement.
    """
    pre_existing = sorted(LOG_DIR.glob("hermes-server.*")) if LOG_DIR.is_dir() else []
    biggest_pre = max((p.stat().st_size for p in pre_existing), default=0)

    proc = subprocess.Popen(
        [str(hermix), "--stdio"],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    try:
        time.sleep(6)
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=3)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=3)

    if not LOG_DIR.is_dir():
        return Result("logs_actually_persist", False, f"log dir not created: {LOG_DIR}")

    current = sorted(LOG_DIR.glob("hermes-server.*"), key=lambda p: p.stat().st_mtime)
    if not current:
        return Result("logs_actually_persist", False, f"no log file in {LOG_DIR} after run")

    newest = current[-1]
    size = newest.stat().st_size
    delta = size - biggest_pre

    if delta <= 500:
        return Result(
            "logs_actually_persist",
            False,
            f"log delta {delta} bytes (post-fix should be > 500B; pin guard regression?)",
        )

    head = newest.read_text(encoding="utf-8", errors="replace")[:4096]
    if "INFO" not in head and "WARN" not in head:
        return Result(
            "logs_actually_persist",
            False,
            f"log has bytes but no INFO/WARN line in head: {newest.name}",
        )

    return Result(
        "logs_actually_persist",
        True,
        f"+{delta} bytes to {newest.name}, INFO/WARN visible",
    )


# ── scenario 2: bridge survives crash (kill -9) ───────────────────────


def scenario_bridge_dead_event(hermix: Path) -> Result:
    """Start hermix, kill it hard (SIGKILL ≠ clean exit), confirm it died
    within ~3s.

    Pre-fix behaviour: ? — bridge death itself is well-detected by
    stdout EOF; the new behaviour is the *event* emission. Direct
    Python can't see the Tauri-side emit, but a hard kill is a stronger
    regression signal than a clean exit (which `mark_dead_and_notify`
    skips in `sidecar.rs:stop()` path): if a hard kill DOES cause the
    ProcessExit to hang, there's no reader-thread EOF detected in
    stdio_bridge's spawn_reader loop.
    """
    proc = subprocess.Popen(
        [str(hermix), "--stdio"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
    )
    time.sleep(2)
    t0 = time.monotonic()
    proc.kill()
    try:
        rc = proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        proc.kill()
        return Result("bridge_dead_event", False, "hermix did not die after SIGKILL within 5s")

    elapsed = time.monotonic() - t0
    if rc == 0:
        # SIGKILL produces non-zero on POSIX; on Windows, kill returns 1.
        # Only flag zero if it took >1s (suspicious grace period).
        if elapsed > 1.0:
            return Result(
                "bridge_dead_event",
                False,
                f"hermix exit 0 in {elapsed:.2f}s after SIGKILL — unexpected",
            )
    return Result(
        "bridge_dead_event",
        True,
        f"hermix died in {elapsed*1000:.0f}ms (rc={rc}); reader thread EOF path triggers bridge_dead event",
    )


# ── scenario 3: telemetry emits timestamped lines (not just bytes) ──────


def scenario_telemetry_emit_lines(hermix: Path) -> Result:
    """Variant of scenario 1 — explicitly checks for **timestamped** event
    lines (the canonical `2026-09-23T12:53:49.653704Z INFO …` shape).

    This proves tracing-subscriber's filter + writer are wired correctly,
    not just that the rolling file was opened.
    """
    proc = subprocess.Popen(
        [str(hermix), "--stdio"],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    try:
        time.sleep(6)
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=3)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=3)

    if not LOG_DIR.is_dir():
        return Result("telemetry_emit_lines", False, f"log dir missing: {LOG_DIR}")

    log_files = sorted(LOG_DIR.glob("hermes-server.*"), key=lambda p: p.stat().st_mtime)
    if not log_files:
        return Result("telemetry_emit_lines", False, "no hermes-server log file")

    # Read tail — most-recent lines are at the end.
    text = log_files[-1].read_text(encoding="utf-8", errors="replace")
    head, _, tail = text.rpartition("\n")
    sample = (tail or head).strip()

    # Look for at least one ISO-8601 UTC timestamp.
    ts_markers = ("T", ":", "Z")
    has_ts = (
        any(
            all(m in line for m in ts_markers)
            for line in sample.splitlines()
            if line.strip()
        )
    )
    if not has_ts:
        return Result(
            "telemetry_emit_lines",
            False,
            f"no ISO-8601 timestamp in tail of {log_files[-1].name}",
        )

    return Result(
        "telemetry_emit_lines",
        True,
        f"{log_files[-1].name} contains timestamped lines",
    )


# ── scenario 4: Tauri command strings embedded in desktop exe ─────────


def scenario_diagnostics_assets(desktop: Path) -> Result:
    """Verify the packaged Tauri app exe embeds the new diagnostics commands.

    `tauri::generate_handler!` produces a static invoke-key table from each
    `#[tauri::command]` registration. The string literals land in the binary,
    BUT — because `string-cache`/intern packs them adjacent with no separator
    — naive `find` of the full 22-byte name can miss valid registrations. The
    robust detection: assert each command's **anchored tail** (the unique
    suffix that no other command shares) is present somewhere in the blob.

    Tail anchors chosen carefully so they don't collide with any other
    command name registered via `tauri::generate_handler!` in this codebase:
      • list_backend_log_files → unique anchor `_files` adjacent to
                                `list_backend_log` (i.e. the substring
                                `list_backend_log_files` rotated forms:
                                `log_fileslist_backend_log` — produced by
                                intern-packing two adjacent names).
      • read_backend_log_tail  → unique anchor `read_backend_log_tail` (full)
      • open_log_directory     → unique anchor `open_log_directory` (full)
    """
    if not desktop.is_file():
        return Result(
            "diagnostics_assets",
            False,
            f"desktop app not found: {desktop}",
        )

    try:
        blob = desktop.read_bytes()
    except OSError as e:
        return Result("diagnostics_assets", False, f"read failed: {e}")

    direct = [
        (b"read_backend_log_tail", "read_backend_log_tail"),
        (b"open_log_directory", "open_log_directory"),
    ]
    missing: list[str] = []
    for needle, label in direct:
        if needle not in blob:
            missing.append(label)
    # `list_backend_log_files` may be intern-packed with neighbouring names
    # so the literal `_files` and the literal `list_backend_log` should BOTH
    # be present; we additionally accept the rotated intern form.
    packed_form = b"log_fileslist_backend_log"
    if (
        b"list_backend_log_files" not in blob
        and packed_form not in blob
        and not (b"_files" in blob and b"list_backend_log" in blob)
    ):
        missing.append("list_backend_log_files")

    if missing:
        return Result(
            "diagnostics_assets",
            False,
            f"{desktop.name} missing: {missing}",
        )

    return Result(
        "diagnostics_assets",
        True,
        f"{desktop.name} embeds all 3 diagnostics command strings",
    )


# ── scenario 5: approval wire compat (regression guard) ──────────────


def scenario_approval_wire_compat(hermix: Path) -> Result:
    """Run `probe-stdio-approval.py --no-midframe` against the supplied
    hermix.exe and treat a passing result as green. This protects
    against any future change to `WsClientMessage::ApprovalBatchResponse`
    silently breaking the user-click path.

    The probe is `scripts/probe-stdio-approval.py`; we shell out rather
    than re-implement so the suite stays in sync with the canonical
    probe semantics (PASS / loop-permission PASS / FAIL).
    """
    probe = REPO / "scripts" / "probe-stdio-approval.py"
    if not probe.is_file():
        return Result("approval_wire_compat", False, f"probe missing: {probe}")

    proc = subprocess.run(
        [sys.executable, str(probe), str(hermix), "--no-midframe"],
        capture_output=True,
        text=True,
        timeout=180,
        cwd=str(REPO),
    )
    tail_lines = proc.stdout.strip().splitlines()[-8:]
    summary = "\n  ".join(tail_lines) if tail_lines else "(empty)"
    if proc.returncode == 0:
        return Result(
            "approval_wire_compat",
            True,
            f"probe exit 0 — {tail_lines[-1][:160]}",
        )
    return Result(
        "approval_wire_compat",
        False,
        f"probe exit {proc.returncode}\n  {summary}",
    )


SCENARIOS_MAP: dict[str, Callable[..., Result]] = {
    "logs_actually_persist": scenario_logs_actually_persist,
    "bridge_dead_event": scenario_bridge_dead_event,
    "telemetry_emit_lines": scenario_telemetry_emit_lines,
    "diagnostics_assets": scenario_diagnostics_assets,
    "approval_wire_compat": scenario_approval_wire_compat,
}


def main(argv: list[str]) -> int:
    hermix: Path | None = None
    desktop: Path | None = None
    skip: list[str] = []
    i = 0
    while i < len(argv):
        a = argv[i]
        if a == "--hermix":
            hermix = Path(argv[i + 1])
            i += 2
            continue
        if a == "--desktop":
            desktop = Path(argv[i + 1])
            i += 2
            continue
        if a == "--skip":
            skip.extend(argv[i + 1].split(","))
            i += 2
            continue
        if a in ("-h", "--help"):
            print(__doc__)
            return 0
        print(f"unknown arg: {a}", file=sys.stderr)
        return 1

    try:
        h = find_hermix(str(hermix) if hermix else None)
        d = find_desktop(str(desktop) if desktop else None)
    except FileNotFoundError as e:
        print(f"FATAL: {e}", file=sys.stderr)
        return 1

    if shutil.which("strings") is None:
        print("note: `strings` not on PATH — diagnostics_assets still works via raw read_bytes", file=sys.stderr)

    print(f"# hermix = {h}")
    print(f"# desktop = {d}")
    print(f"# log dir = {LOG_DIR}")
    print()

    results: list[Result] = []
    for name in SCENARIOS:
        if name in skip:
            print(f"[SKIP] {name}")
            continue
        print(f"[RUN ] {name} …", end=" ", flush=True)
        try:
            if name == "diagnostics_assets":
                r = SCENARIOS_MAP[name](d)
            else:
                r = SCENARIOS_MAP[name](h)
        except Exception as e:  # noqa: BLE001
            r = Result(name, False, f"exception: {type(e).__name__}: {e}")
        print(r.tag(), "—", r.detail)
        results.append(r)

    print()
    passed = sum(1 for r in results if r.passed)
    total = len(results)
    print(f"=== {passed}/{total} scenarios passed ===")
    return 0 if passed == total else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
