"""stdio 帧层实测：流式期间重发同一句，断言拿到两个终态事件。

这是「聊天卡住 / 不回复」的第一手证据工具 —— 绕开前端、绕开 HTTP，
直接用 stdin/stdout 跟 sidecar 说协议，看它到底回不回。

背景：`handle_ws_stream_send` 内部有独立的 select 循环（为流式中接收
approval_response 而加），它会消费同一条入站队列。历史上该循环对
Send 类消息只打 `tracing::debug!` 就丢弃，默认日志级别下完全不可见，
前端 promise 永不 settle → UI 永久停在「正在思考…」。

修复后行为（本脚本断言的就是这个）：
  #1 [done] finish_reason=stop            ← 第一条正常完成
  #2 [done] finish_reason=task_in_flight  ← 第二句被明确拒绝，但给了终态

用法：
    python scripts/probe-stdio-frames.py [hermix.exe 路径] [--log]

    不加 --log 时只看终态事件（干净输出）；加 --log 会打印 WARN/ERROR
    行，便于确认 decline 路径确实被走到。

默认用隔离的 HERMIX_HOME（临时目录 + 拷真实 config/.env），
不会污染 ~/.hermix。
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
DEFAULT_EXE = os.path.join(REPO, "target", "debug", "hermix.exe")

VERBOSE = "--log" in sys.argv
argv = [a for a in sys.argv[1:] if not a.startswith("--")]
EXE = argv[0] if argv else DEFAULT_EXE

# 等第一条真正进入流式状态再撞第二句。太早发会让第二句排在第一条之前，
# 考不到 select 循环的 decline 分支。
FIRST_STREAM_DELAY = 6
OVERALL_TIMEOUT = 60

PROMPT = "用一句话说明什么是递归"


def build_sandbox():
    """隔离 HERMIX_HOME，拷真实配置以便能真的调起 provider。"""
    home = tempfile.mkdtemp(prefix="hx-stdio-probe-")
    hermix_home = os.path.join(home, ".hermix")
    os.makedirs(hermix_home, exist_ok=True)
    real = os.path.join(os.path.expanduser("~"), ".hermix")
    for name in ("config.yaml", ".env"):
        src = os.path.join(real, name)
        if os.path.exists(src):
            shutil.copy2(src, os.path.join(hermix_home, name))
    return home, hermix_home


def stream_probe(exe, hermix_home, session_id, log):
    env = dict(os.environ)
    env["HERMIX_HOME"] = hermix_home

    proc = subprocess.Popen(
        [exe, "--stdio"],
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
        """入站帧必须包 stdio 信封，裸帧会被 parse_inbound 静默丢弃。"""
        frame = {
            "channel": f"stream:{session_id}",
            "payload": json.dumps({"type": "send", "text": text}, ensure_ascii=False),
            "request_id": "",
        }
        proc.stdin.write(json.dumps(frame, ensure_ascii=False) + "\n")
        proc.stdin.flush()

    t0 = time.time()
    send(PROMPT)
    log(">>> sent #1")
    time.sleep(FIRST_STREAM_DELAY)
    log(">>> sent #2 (流式中重发)")
    send(PROMPT)

    events = []
    deadline = time.time() + OVERALL_TIMEOUT
    while time.time() < deadline and len(events) < 2:
        try:
            tag, line = out.get(timeout=1)
        except queue.Empty:
            continue
        if tag == "ERR":
            if VERBOSE and any(k in line for k in ("WARN", "ERROR", "ws_stream")):
                log(f"[stderr] {line[:200]}")
            continue
        # 出站同样是信封：{channel, payload, request_id}，
        # StreamEvent 在 payload 字符串里 —— 不是顶层 event 字段。
        try:
            outer = json.loads(line)
        except Exception:
            continue
        payload = outer.get("payload")
        if payload is not None and not outer.get("event"):
            try:
                envelope = json.loads(payload) if isinstance(payload, str) else payload
            except Exception:
                continue
        else:
            envelope = outer
        event = envelope.get("event") or {}
        etype = event.get("type")
        if etype in ("done", "error"):
            events.append(event)
            content = (event.get("content") or "")[:70].replace("\n", " ")
            log(
                f"  #{len(events)} [{etype}] finish_reason={event.get('finish_reason')} "
                f"outcome={event.get('outcome')} content={content!r}"
            )

    proc.terminate()
    try:
        proc.wait(timeout=10)
    except subprocess.TimeoutExpired:
        proc.kill()
    return events, time.time() - t0


def main():
    if not os.path.exists(EXE):
        print(f"FAIL — 找不到二进制: {EXE}")
        print("       先跑 `cargo build -p hermix-server --bin hermix`")
        return 1

    quiet = (lambda _: None) if not VERBOSE else print
    home, hermix_home = build_sandbox()
    try:
        print(f"exe = {EXE}")
        print(f"HERMIX_HOME = {hermix_home}\n")
        events, elapsed = stream_probe(EXE, hermix_home, "probe-stdio", quiet)
    finally:
        shutil.rmtree(home, ignore_errors=True)

    print(f"\n=== {len(events)} 个终态事件, 耗时 {elapsed:.1f}s ===")
    reasons = [e.get("finish_reason") for e in events]
    if len(events) < 2:
        print("FAIL — 第二条消息没有拿到终态事件（前端会永久停在「正在思考…」）")
        print("       下一步查 ws.rs 的 decline 分支是否发了 Done，")
        print("       以及 useStreamParser 的 status 分支是否 settle。")
        return 1
    if "task_in_flight" not in reasons:
        print(f"FAIL — 拿到了两个终态，但没有 task_in_flight: {reasons}")
        print("       说明第二句被当成了全新任务（而非拒绝），不符合预期语义。")
        return 1
    print("PASS — 第二条消息被明确拒绝且给出终态（task_in_flight）")
    return 0


if __name__ == "__main__":
    sys.exit(main())
