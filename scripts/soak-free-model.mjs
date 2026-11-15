// 免费模型小型长任务稳定性观察（2026-11-11）
// 3 次同任务一致性 + 1 次中途断连扰动；任务 = 建文件写斐波那契 + 读回验证（多步、带工具）。
const TOKEN = process.env.HERMIX_TEST_TOKEN || "";
const TASK = "分三步完成：① 在 %TEMP%/hermix-eval/ 下创建 fib.txt 写入斐波那契前10项（每行一个数）；② 用读取工具读回 fib.txt 验证第10项是55；③ 回复一行结论。不要跳步。";

function wsRun(message, { tag, killAfter = null, timeoutMs = 300000 }) {
  return new Promise((resolve) => {
    const t0 = Date.now();
    const ws = new WebSocket("ws://127.0.0.1:3000/v1/ws-stream/soak-free-1", {
      headers: { Authorization: `Bearer ${TOKEN}` },
    });
    let firstEvent = null, done = false, frames = 0, bytes = 0, toolCalls = 0, tail = "";
    const finish = (status) => {
      if (done) return; done = true;
      try { ws.close(); } catch {}
      resolve({ tag, status, firstEventMs: firstEvent, totalMs: Date.now() - t0, frames, bytes, toolCalls, tail: tail.slice(0, 200) });
    };
    const timer = setTimeout(() => finish("timeout"), timeoutMs);
    ws.onopen = () => {
      ws.send(JSON.stringify({ message, session_id: "soak-free-1" }));
      if (killAfter) setTimeout(() => { try { ws.close(); } catch {} finish("aborted"); }, killAfter);
    };
    ws.onmessage = (ev) => {
      frames++; bytes += (ev.data || "").length;
      if (!firstEvent) firstEvent = Date.now() - t0;
      const s = String(ev.data);
      if (s.includes("tool_call")) toolCalls++;
      tail = s.slice(-300);
      if (s.includes('"event":"turn_complete"') || s.includes('turn_complete')) {
        clearTimeout(timer); finish("ok");
      }
    };
    ws.onerror = () => { clearTimeout(timer); finish("error"); };
    ws.onclose = () => { if (!done && !killAfter) { clearTimeout(timer); finish("closed_early"); } };
  });
}

(async () => {
  const out = [];
  for (let i = 1; i <= 3; i++) {
    out.push(await wsRun(`（第${i}遍）` + TASK, { tag: `run${i}`, timeoutMs: 300000 }));
  }
  // 扰动：长任务中途断连（15s 后杀连接），随后重连续跑同一 session
  out.push(await wsRun(TASK, { tag: "disconnect-mid", killAfter: 15000, timeoutMs: 300000 }));
  out.push(await wsRun("继续完成任务并给出结论。", { tag: "reconnect-after-disconnect", timeoutMs: 300000 }));
  console.log(JSON.stringify(out, null, 1));
})();
