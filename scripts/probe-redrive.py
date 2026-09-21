"""Narrow probe: does the deferred second turn actually get re-driven?

Sends #1, waits until it is genuinely mid-stream, injects #2 (deferred),
then watches for the re-drive evidence:

    WARN ... deferred to the main loop
    INFO ... rx forward loop EXITED  exit_branch="rx_terminal_event"
    INFO ... received Text message  <-- re-drive handed the text back
    INFO ... handle_ws_stream_send entry  <-- second turn dispatched

If `received Text message` never follows EXITED, the text was dropped
somewhere between the forward loop returning and the main loop re-driving.
If it does appear but no `done` follows, the second turn started and the
upstream is what's slow.

Usage:  python scripts/probe-redrive.py [hermix.exe]
"""

import json
import os
import queue
import shutil
import subprocess
import sys
import tempfile
import threading
import time

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DEFAULT_EXE = os.path.join(REPO, "release", "target", "debug", "hermix.exe")
FALLBACK_EXE = os.path.join(REPO, "target", "debug", "hermix.exe")

argv = [a for a in sys.argv[1:] if not a.startswith("--")]
EXE = argv[0] if argv else (DEFAULT_EXE if os.path.exists(DEFAULT_EXE) else FALLBACK_EXE)

SESSION = "probe-redrive"
PROMPT_1 = "用一句话说明什么是递归"
PROMPT_2 = "用一句话说明什么是哈希表"

# 等 #1 推够内容事件再注入 —— 固定睡眠在快的那侧会等到它跑完。
INJECT_AFTER_CONTENT_EVENTS = 3
# 注入之后给足上游时间：首包超时重试会拖很久。
RE_DRIVE_TIMEOUT = 240


def build_sandbox():
    home = tempfile.mkdtemp(prefix="hx-redrive-")
    hermix_home = os.path.join(home, ".hermix")
    os.makedirs(hermix_home, exist_ok=True)
    real = os.path.join(os.path.expanduser("~"), ".hermix")
    for name in ("config.yaml", ".env"):
        src = os.path.join(real, name)
        if os.path.exists(src):
            shutil.copy2(src, os.path.join(hermix_home, name))
    return home, hermix_home


def parse(line):
    try:
        outer = json.loads(line)
    except Exception:
        return None
    payload = outer.get("payload")
    if payload is not None and not outer.get("event"):
        try:
            payload = json.loads(payload) if isinstance(payload, str) else payload
        except Exception:
            return None
    event = (payload or {}).get("event") or {}
    return event.get("type"), event


def main():
    if not os.path.exists(EXE):
        print(f"FAIL — 找不到二进制: {EXE}")
        return 1

    home, hermix_home = build_sandbox()
    env = dict(os.environ)
    env["HERMIX_HOME"] = hermix_home

    proc = subprocess.Popen(
        [EXE, "--stdio"],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        env=env, text=True, encoding="utf-8", errors="replace", bufsize=1,
    )
    out = queue.Queue()

    def pump(stream, tag):
        for line in stream:
            out.put((tag, line.rstrip()))

    threading.Thread(target=pump, args=(proc.stdout, "OUT"), daemon=True).start()
    threading.Thread(target=pump, args=(proc.stderr, "ERR"), daemon=True).start()

    def send(text):
        frame = {
            "channel": f"stream:{SESSION}",
            "payload": json.dumps({"type": "send", "text": text}, ensure_ascii=False),
            "request_id": "",
        }
        proc.stdin.write(json.dumps(frame, ensure_ascii=False) + "\n")
        proc.stdin.flush()

    t0 = time.time()

    def ts():
        return f"{time.time() - t0:6.1f}s"

    print(f"exe = {EXE}")
    print(f"HERMIX_HOME = {hermix_home}\n")

    send(PROMPT_1)
    print(f"[{ts()}] >>> sent #1")

    saw_deferred = saw_exited = saw_redrive_text = saw_entry2 = False
    dones = []
    sent2 = False
    content_events = 0
    injected_at = None
    deadline = time.time() + RE_DRIVE_TIMEOUT

    while time.time() < deadline and len(dones) < 2:
        try:
            tag, line = out.get(timeout=1)
        except queue.Empty:
            continue

        if tag == "ERR":
            if "deferred to the main loop" in line:
                saw_deferred = True
                print(f"[{ts()}]     WARN deferred  ← #2 撞上流式，进排队路径")
            elif "rx forward loop EXITED" in line:
                saw_exited = True
                print(f"[{ts()}]     INFO EXITED    ← forward loop 返回（根因 C 的修复点）")
            elif "received Text message" in line:
                if saw_exited:
                    saw_redrive_text = True
                    print(f"[{ts()}]     INFO received Text  ← re-drive 把原文交回主循环 ✅")
                else:
                    print(f"[{ts()}]     INFO received Text  (第一条的入站)")
            elif "handle_ws_stream_send: spawning agent task" in line:
                if saw_exited and saw_redrive_text and not saw_entry2:
                    saw_entry2 = True
                    print(f"[{ts()}]     INFO spawning #2     ← 第二条真的起跑了 ✅")
            continue

        parsed = parse(line)
        if parsed is None:
            continue
        etype, event = parsed

        if etype == "status" and "task_in_flight" in (event.get("content") or ""):
            print(f"[{ts()}]     Status task_in_flight ← 非终态，前端保持气泡")
            continue

        if etype in ("done", "error"):
            dones.append(event)
            content = (event.get("content") or "")[:50].replace("\n", " ")
            print(f"[{ts()}] #{len(dones)} [{etype}] {event.get('finish_reason')} {content!r}")
            continue

        if etype in ("text", "thinking", "tool_start") and not sent2:
            content_events += 1
            if content_events >= INJECT_AFTER_CONTENT_EVENTS:
                sent2 = True
                injected_at = time.time() - t0
                print(f"[{ts()}] >>> sent #2（#1 已推 {content_events} 个内容事件）")
                send(PROMPT_2)

    proc.terminate()
    try:
        proc.wait(timeout=10)
    except subprocess.TimeoutExpired:
        proc.kill()
    shutil.rmtree(home, ignore_errors=True)

    elapsed = time.time() - t0
    print(f"\n=== 耗时 {elapsed:.1f}s | deferred={saw_deferred} EXITED={saw_exited} "
          f"re-drive Text={saw_redrive_text} #2 起跑={saw_entry2} | 终态 {len(dones)} 个 ===")

    if len(dones) == 2:
        print("PASS — 排队那条被 re-drive 并跑完")
        return 0
    if not saw_deferred:
        print("INCONCLUSIVE — #2 没撞上流式（#1 太快跑完了），这轮没考到分支")
        return 2
    if not saw_exited:
        print("FAIL — forward loop 没退出。根因 C 的修复没生效。")
        return 1
    if not saw_redrive_text:
        print("FAIL — EXITED 之后没等到 `received Text message`。")
        print("       原文在 forward loop 返回 → 主循环 re-drive 之间丢了。")
        return 1
    if not saw_entry2:
        print("FAIL — re-drive 交回了原文，但第二条没起跑（spawning agent task 没出现）。")
        return 1
    if injected_at is not None and elapsed >= RE_DRIVE_TIMEOUT - 1:
        print(f"FAIL — 第二条已起跑但 {elapsed:.0f}s 内没出终态，撞上 {RE_DRIVE_TIMEOUT}s 上限。")
        print("       协议链路是通的，慢在上游 LLM（首包超时重试）。")
        return 1
    print("FAIL — 未知原因")
    return 1


if __name__ == "__main__":
    sys.exit(main())
