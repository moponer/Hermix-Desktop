"""stdio 帧层实测：流式期间再发一条，断言它被「排队并跑完」而不是被丢掉。

这是「聊天卡住 / 不回复」的第一手证据工具 —— 绕开前端、绕开 HTTP，
直接用 stdin/stdout 跟 sidecar 说协议，看它到底回不回。

背景：`handle_ws_stream_send` 内部有独立的 select 循环（为流式中接收
approval_response 而加），它消费的是同一条入站队列。历史行为是：流式
期间到达的用户消息被这个循环吃掉，回一个终态 `Done{task_in_flight}`
然后丢弃 —— 用户的原文彻底消失，前端 promise 空 settle，聊天表现为
「发了没反应」。

现在的行为（本脚本断言的就是这个）：
  #1 第一条正常跑完                      → done finish_reason=stop
  #2 第二条撞上流式，回非终态 Status      → status task_in_flight（不 settle）
  #3 第一条结束后，被排队的那条自动开跑    → done finish_reason=stop

断言三条都成立才算 PASS。#2 的 Status 必须是**非终态**：如果它是一
个 Done，前端会在排队那条真正跑出结果之前就结束气泡，答案无处渲染。

用法：
    python scripts/probe-stdio-frames.py [hermix.exe 路径] [--log]

    不加 --log 时只看事件流（干净输出）；加 --log 会打印 stderr 里
    的 WARN/ERROR，便于确认排队路径确实被走到。

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
# dev 模式下 sidecar 解析优先级是 release/target/debug 优先于 target/debug，
# 默认值必须跟 `sidecar.rs::hermix_bin_path()` 对齐，否则会测一个用户根本
# 没在跑的二进制。
DEFAULT_EXE = os.path.join(REPO, "release", "target", "debug", "hermix.exe")
FALLBACK_EXE = os.path.join(REPO, "target", "debug", "hermix.exe")

VERBOSE = "--log" in sys.argv
argv = [a for a in sys.argv[1:] if not a.startswith("--")]
if argv:
    EXE = argv[0]
elif os.path.exists(DEFAULT_EXE):
    EXE = DEFAULT_EXE
else:
    EXE = FALLBACK_EXE

# 等第一条真正进入流式状态再撞第二句。太早发会让第二句排在第一条之前，
# 考不到那个 select 循环的分支。
FIRST_STREAM_DELAY = 6
# 排队的那条要在第一条跑完之后才开跑，整体给足时间。
OVERALL_TIMEOUT = 180

PROMPT_1 = "用一句话说明什么是递归"
PROMPT_2 = "用一句话说明什么是哈希表"


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
    send(PROMPT_1)
    log(">>> sent #1")
    time.sleep(FIRST_STREAM_DELAY)
    log(">>> sent #2（第一条还在流式）")
    send(PROMPT_2)

    dones = []
    statuses = []
    deadline = time.time() + OVERALL_TIMEOUT
    while time.time() < deadline and len(dones) < 2:
        try:
            tag, line = out.get(timeout=1)
        except queue.Empty:
            continue
        if tag == "ERR":
            if VERBOSE and any(k in line for k in ("WARN", "ERROR", "deferred")):
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

        if etype == "status":
            content = event.get("content") or ""
            if "task_in_flight" in content:
                statuses.append(event)
                log(f"  [status] task_in_flight（非终态，前端应保持气泡）")
            continue

        if etype in ("done", "error"):
            dones.append(event)
            content = (event.get("content") or "")[:60].replace("\n", " ")
            log(
                f"  #{len(dones)} [{etype}] finish_reason={event.get('finish_reason')} "
                f"outcome={event.get('outcome')} content={content!r}"
            )

    proc.terminate()
    try:
        proc.wait(timeout=10)
    except subprocess.TimeoutExpired:
        proc.kill()
    return dones, statuses, time.time() - t0


def main():
    if not os.path.exists(EXE):
        print(f"FAIL — 找不到二进制: {EXE}")
        print("       先跑 `cargo build -p hermix-server --target-dir release/target`")
        return 1

    quiet = (lambda _: None) if not VERBOSE else print
    home, hermix_home = build_sandbox()
    try:
        print(f"exe = {EXE}")
        print(f"HERMIX_HOME = {hermix_home}\n")
        dones, statuses, elapsed = stream_probe(EXE, hermix_home, "probe-stdio", quiet)
    finally:
        shutil.rmtree(home, ignore_errors=True)

    print(f"\n=== {len(dones)} 个终态 + {len(statuses)} 个 task_in_flight Status, 耗时 {elapsed:.1f}s ===")

    if not statuses:
        print("FAIL — 没等到 task_in_flight Status。")
        print("       说明流式期间那条消息没有走「排队」路径。")
        return 1

    if len(dones) < 2:
        print("FAIL — 排队的那条没有跑完（只看到 %d 个终态）。" % len(dones))
        print("       说明 handle_inbound_frame 的 re-drive 没生效，消息仍然丢了。")
        return 1

    reasons = [e.get("finish_reason") for e in dones]
    if any(r == "task_in_flight" for r in reasons):
        print(f"FAIL — 出现了终态 task_in_flight: {reasons}")
        print("       排队路径不该回终态 Done，那会让前端在答案出来前就结束气泡。")
        return 1

    if not any(r == "stop" for r in reasons):
        print(f"FAIL — 没有正常的 stop 终态: {reasons}")
        return 1

    print("PASS — 消息被排队（非终态 Status），随后自动跑完（stop）")
    return 0


if __name__ == "__main__":
    sys.exit(main())
