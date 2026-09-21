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

    不加 --log 时只看事件流（干净输出）；加 --log 会额外打印 stderr 里的
    WARN/ERROR 和生命周期锚点（`deferred` / `dedup` / `EXITED`），便于区分
    「排队路径没走到」和「forward loop 没退出」。

默认用隔离的 HERMIX_HOME（临时目录 + 拷真实 config/.env），
不会污染 ~/.hermix。

注意：干净 home 的第二条会明显更慢 —— 没有既有的 memory / skill 索引，
sub-agent 与上下文构建要从头做一遍。判 FAIL 先看耗时有没有撞 OVERALL_TIMEOUT。
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

# 第一条进入流式后再注入第二条 —— 不用固定睡眠。理由见 stream_probe()：
# 第一条耗时随上游抖动（实测 2s ~ 8s），任何固定睡眠都会在一侧出错
# （睡短了 #1 已跑完，两条纯串行；睡长了白等）。
# 一条 LLM 串行跑，隔离 home 下实测 ~9s（快）到 ~110s（上游首包超时重试）。
# 给 300 是留足余量 —— 判 FAIL 前先看耗时有没有真的撞到上限，别把
# 「脚本先到期」当成「re-drive 没生效」。
OVERALL_TIMEOUT = 300

PROMPT_1 = "用一句话说明什么是递归"
PROMPT_2 = "用一句话说明什么是哈希表"

# --log 只看这些串。前四个是排队/re-drive 路径的锚点，后两个是终态退出
# 的锚点（见 ws.rs 的 exit_branch 日志）。
LOG_KEYS = (
    "WARN", "ERROR",
    "deferred", "dedup",
    "EXITED", "rx_terminal_event",
)

# #1 推够这么多个内容事件后才注入 #2。不能是 1：首个 delta 之后 #1 可能
# 立刻收尾（短回答），第 1 个事件就注入仍会撞在尾巴上。攒几个既保证
# #1 正在活跃流式，又不必等它整个跑完。
CONTENT_EVENTS_BEFORE_INJECT = 3


def parse_event(line):
    """出站是 stdio 信封 `{channel, payload, request_id}`，真正的
    `StreamEvent` 在 `payload` 字符串里 —— 不是顶层 `event` 字段。
    返回 `(etype, event_dict)`，非事件行返回 `None`。"""
    try:
        outer = json.loads(line)
    except Exception:
        return None
    payload = outer.get("payload")
    if payload is not None and not outer.get("event"):
        try:
            envelope = json.loads(payload) if isinstance(payload, str) else payload
        except Exception:
            return None
    else:
        envelope = outer
    event = envelope.get("event") or {}
    return event.get("type"), event


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

    dones = []
    statuses = []
    # #2 只在 #1 真的开始流式之后才发。判据是「#1 已经推过内容类事件」
    # （text / thinking / tool_start）—— 那之后 queue 里还有东西要吐，
    # #2 必然撞上流式。不能用固定睡眠：第一条的耗时随上游抖动，睡短了
    # #1 早已跑完（两条变串行、考不到分支），睡长了白等。
    stream_started = False
    sent2_at = None
    drained = 0  # #1 累计推过多少个内容类事件
    deadline = time.time() + OVERALL_TIMEOUT
    while time.time() < deadline and len(dones) < 2:
        try:
            tag, line = out.get(timeout=1)
        except queue.Empty:
            continue
        if tag == "ERR":
            # 过滤串必须覆盖 INFO 级的生命周期锚点，不能只留 WARN/ERROR：
            # `EXITED` 是 `handle_ws_stream_send` 返回的唯一证据，而它是
            # `tracing::info!`。漏掉它会让「函数没退出」和「脚本只显示了
            # 一部分日志」看起来一模一样 —— 之前就是靠它误判过一轮。
            if VERBOSE and any(k in line for k in LOG_KEYS):
                log(f"[stderr] {line[:220]}")
            continue
        text = parse_event(line)
        if text is None:
            continue
        etype, event = text

        if etype == "status":
            content = event.get("content") or ""
            if "task_in_flight" in content:
                statuses.append(event)
                log("  [status] task_in_flight（非终态，前端应保持气泡）")
            continue

        if etype in ("done", "error"):
            dones.append(event)
            content = (event.get("content") or "")[:60].replace("\n", " ")
            log(
                f"  #{len(dones)} [{etype}] finish_reason={event.get('finish_reason')} "
                f"outcome={event.get('outcome')} content={content!r}"
            )
            continue

        # 内容类事件：#1 还在产东西，此刻撞进去才算考到那个分支。
        if etype in ("text", "thinking", "tool_start"):
            drained += 1
            if drained >= CONTENT_EVENTS_BEFORE_INJECT and not stream_started:
                stream_started = True
                sent2_at = time.time() - t0
                log(f">>> sent #2（#1 已推 {drained} 个内容事件，仍会继续流式）")
                send(PROMPT_2)

    if not stream_started:
        log(">>> 警告：#1 始终没推出内容事件，#2 根本没发出去")

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
        print("       说明流式期间那条消息没有走「排队」路径。加 --log 看有没有")
        print("       `deferred to the main loop`；没有就是 #1 跑太快、#2 没撞上流式")
        print("       （这轮没考到分支，不是代码问题）。")
        return 1

    if len(dones) < 2:
        print("FAIL — 排队的那条没有跑完（只看到 %d 个终态）。" % len(dones))
        print("       判 FAIL 前先加 --log 看三行锚点（缺哪行就对应哪个断点）：")
        print("       · `deferred to the main loop`  —— 没出现：#2 没撞上流式，这轮没考到分支")
        print("       · `rx forward loop EXITED`     —— 没出现：forward loop 没退出")
        print("       · EXITED 之后 `received Text message` —— 没出现：原文在")
        print("         handle_inbound_frame 的 return 链上被吞了（dispatch 分支没透传")
        print("         StreamSendOutcome，见 ws.rs 的 every_dispatch_arm_returns_its_outcome）")
        print("       三行都在但迟迟没有第二个终态 → 第二条已起跑，慢在上游 LLM。")
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
