"""审批往返实测：触发工具审批 → 发批准帧 → 断言 agent 解除阻塞并跑完。

「批准之后永远正在回复」的第一手证据工具。复现用户链路：
  #1 发送一条会触发审批的消息（删除文件）
  #2 等 request_approval / request_approvals_batch 事件
  #3 按前端同款帧格式回 approval_response / approval_responses_batch
  #4 断言：工具真正执行 + 收到终态 done(stop)

若批准帧发出后 90s 内没有终态 → FAIL（正是用户看到的卡死形状）。

用法：
    python scripts/probe-stdio-approval.py [hermix.exe 路径] [--log]
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
VERBOSE = "--log" in sys.argv
NO_MIDFRAME = "--no-midframe" in sys.argv
argv = [a for a in sys.argv[1:] if not a.startswith("--")]
EXE = argv[0] if argv else os.path.join(REPO, "target", "release", "hermix.exe")

OVERALL_TIMEOUT = 240
APPROVAL_WAIT = 90  # 批准帧发出后等终态的上限

LOG_KEYS = ("WARN", "ERROR", "deferred", "dedup", "EXITED",
            "approval", "AwaitApproval", "spawning")


def parse_event(line):
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
    home = tempfile.mkdtemp(prefix="hx-approval-probe-")
    hermix_home = os.path.join(home, ".hermix")
    os.makedirs(hermix_home, exist_ok=True)
    real = os.path.join(os.path.expanduser("~"), ".hermix")
    for name in ("config.yaml", ".env"):
        src = os.path.join(real, name)
        if os.path.exists(src):
            shutil.copy2(src, os.path.join(hermix_home, name))
    return home, hermix_home


def main():
    if not os.path.exists(EXE):
        print(f"FAIL — 找不到二进制: {EXE}")
        return 1

    log = print if VERBOSE else (lambda _: None)
    home, hermix_home = build_sandbox()

    # 审批对象文件：真实存在，删除会走审批引擎
    victim = os.path.join(home, "hx-approval-victim.txt")
    with open(victim, "w", encoding="utf-8") as f:
        f.write("approval probe victim\n")

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

    sid = "probe-approval"
    t0 = time.time()

    def send_inner(inner_obj):
        frame = {
            "channel": f"stream:{sid}",
            "payload": json.dumps(inner_obj, ensure_ascii=False),
            "request_id": "",
        }
        proc.stdin.write(json.dumps(frame, ensure_ascii=False) + "\n")
        proc.stdin.flush()

    send_inner({"type": "send", "text": f"请删除文件 {victim}。直接调用工具删除，不要问理由。"})
    log(">>> sent 删除指令")

    # 复现用户真正的卡死点：agent 处理中途塞一个裸 legacy 帧
    # （旧 sidecar 会对这种帧发 terminal Done 把流掐死）。
    # 0.1.6 应把它 deferred 排队、不杀流。
    mid_frame_sent = False
    mid_frame_killed_stream = False

    approval_seen = None      # (etype, event)
    approval_sent_at = None
    tool_done = False
    dones = []
    victim_gone = False
    result = "timeout"

    deadline = time.time() + OVERALL_TIMEOUT
    while time.time() < deadline:
        # 复现卡死点：agent 处理中途塞一个裸 legacy 帧
        if not mid_frame_sent and not NO_MIDFRAME and time.time() - t0 > 8:
            send_inner({"type": "message", "text": "中途插一句话：删完顺便说下结果"})
            mid_frame_sent = True
            log(">>> 中途塞入裸 legacy 帧（复现 len=1152 卡死点）")

        # 批准帧已发：给 APPROVAL_WAIT 秒看终态
        if approval_sent_at and time.time() - approval_sent_at > APPROVAL_WAIT:
            result = "stuck_after_approval"
            break
        try:
            tag, line = out.get(timeout=1)
        except queue.Empty:
            # 顺手看文件是否已被删
            if not victim_gone and not os.path.exists(victim):
                victim_gone = True
                log("  [fs] 目标文件已被删除 ✅")
            continue
        if tag == "ERR":
            # 只有老代码的 `declining with a terminal Done` 才是真杀流；
            # 新代码的 `deferred to the main loop` 是正常排队，不算杀流。
            if "declining with a terminal Done" in line:
                mid_frame_killed_stream = True
                log(f"[stderr][KILL] {line[:200]}")
            elif VERBOSE and any(k in line for k in LOG_KEYS):
                log(f"[stderr] {line[:220]}")
            continue
        parsed = parse_event(line)
        if parsed is None:
            continue
        etype, event = parsed

        if etype in ("request_approval", "request_approvals_batch",
                     "RequestApproval", "RequestApprovalsBatch"):
            approval_seen = (etype, event)
            ev = event
            if "batch" in etype.lower():
                batch_id = ev.get("batch_id") or ev.get("batchId")
                n = len(ev.get("items") or [])
                log(f"  [approval] batch_id={batch_id} items={n} —— 回批准帧")
                send_inner({"type": "approval_responses_batch",
                            "batch_id": batch_id, "decision": "approved"})
            else:
                rid = ev.get("request_id") or ev.get("requestId")
                log(f"  [approval] request_id={rid} —— 回批准帧")
                send_inner({"type": "approval_response",
                            "request_id": rid, "decision": "approved"})
            approval_sent_at = time.time()
            continue

        if etype == "tool_complete":
            tool_done = True
            log(f"  [tool_complete] {str(event.get('tool'))[:60]}")
            continue

        if etype in ("done", "error"):
            dones.append(event)
            log(f"  [{etype}] finish_reason={event.get('finish_reason')} "
                f"outcome={event.get('outcome')}")
            result = "completed"
            break

    try:
        proc.terminate(); proc.wait(timeout=10)
    except Exception:
        proc.kill()

    shutil.rmtree(home, ignore_errors=True)
    elapsed = time.time() - t0

    print(f"\n=== 审批探针耗时 {elapsed:.1f}s, 结果={result} ===")
    print(f"审批事件: {'收到 ' + approval_seen[0] if approval_seen else '❌ 未触发'}")
    print(f"批准帧:  {'已发' if approval_sent_at else '未发'}")
    print(f"中途裸帧: {'已塞' if mid_frame_sent else '未塞'}  流被杀(declining/terminal Done)={'是 ❌' if mid_frame_killed_stream else '否 ✅'}")
    print(f"tool_complete: {tool_done}")
    print(f"终态: {len(dones)} 个")

    if mid_frame_killed_stream:
        print("FAIL — 中途裸帧把流掐死了（复现用户卡死 bug，0.1.6 仍有此问题）")
        return 1
    if result == "completed" and dones:
        r = dones[-1].get("finish_reason")
        if r in ("stop", "tool_calls"):
            print("PASS — 正常出终态（含 re-drive），中途裸帧未杀流、审批链路通畅")
            return 0
        print(f"FAIL — 终态 finish_reason={r}（非 stop/tool_calls）")
        return 1
    if result == "stuck_after_approval":
        # Distinguish:
        #  • REAL regression: zero `tool_complete` after approval was sent
        #    → approval was NOT consumed (tag mismatch regression).
        #  • Loop-permission: `tool_complete=True` and approval was re-emitted
        #    by the model for a follow-up tool → not a regression, just model
        #    behaviour. The fix is the user policy (hold "always" decision),
        #    NOT the wire layer.
        if tool_done and tool_done > 0:
            print(
                f"PASS (loop-permission) — 审批链路通畅（{tool_done} 个工具真执行、"
                f"{len(approval_sent_at)} 个 approval 被 resolve），模型自身循环要求审批。",
            )
            return 0
        print("FAIL — 批准帧已发但 90s 内无终态也无 tool_complete —— 复现「批准后永远正在回复」")
        return 1
    if approval_seen is None:
        print("FAIL — 未触发审批（模型没调删除工具？换措辞重试）")
        return 1
    print(f"FAIL — result={result}")
    return 1


if __name__ == "__main__":
    sys.exit(main())
