// 协议级实测驱动：延迟 / 重复一致性 / 异常恢复（2026-11-10 商用评测）
// 用 node 原生 WebSocket（v26）直连 /v1/ws-stream/{session_id}。
const BASE = "http://127.0.0.1:3000";
const SID = "eval-" + Date.now();
const TOKEN = process.env.HERMIX_TEST_TOKEN || "";

function wsChat(message, { timeoutMs = 120000, killAfter = null } = {}) {
  return new Promise((resolve) => {
    const t0 = Date.now();
    const ws = new WebSocket(`ws://127.0.0.1:3000/v1/ws-stream/${SID}`, {
      headers: TOKEN ? { Authorization: `Bearer ${TOKEN}` } : undefined,
    });
    let first = null, done = false, frames = 0, bytes = 0;
    const finish = (status) => {
      if (done) return; done = true;
      try { ws.close(); } catch {}
      resolve({ status, ms_connect: first ? -1 : Date.now() - t0, ms_total: Date.now() - t0, frames, bytes });
    };
    const timer = setTimeout(() => finish("timeout"), timeoutMs);
    ws.onopen = () => {
      const c = Date.now() - t0;
      ws.send(JSON.stringify({ message, session_id: SID, auth: TOKEN ? { bearer: TOKEN } : undefined }));
      resolve._connect_ms = c;
      if (killAfter) setTimeout(() => { try { ws.close(); } catch {} }, killAfter);
    };
    ws.onmessage = (ev) => {
      frames++; bytes += (ev.data || "").length;
      if (String(ev.data).includes('"event":"turn_complete"') || String(ev.data).includes('"done"')) {
        clearTimeout(timer); finish("ok");
      }
    };
    ws.onerror = () => { clearTimeout(timer); finish("error"); };
    ws.onclose = () => { if (!done && killAfter) { clearTimeout(timer); finish("aborted"); } };
  });
}

(async () => {
  const out = { cases: [] };
  // 1. 健康探针（HTTP）
  try {
    const t = Date.now();
    const r = await fetch(BASE + "/api/env/reveal", { method: "POST", headers: { "content-type": "application/json" }, body: "{}" }).catch(() => null);
    out.health = { reachable: !!r, ms: Date.now() - t, code: r ? r.status : null };
  } catch (e) { out.health = { reachable: false, err: String(e) }; }

  // 2. 延迟 + 重复一致性：同任务跑 3 次
  for (let i = 0; i < 3; i++) {
    const r = await wsChat("用一句话回答：1+1 等于几？");
    r.connect_ms = r.ms_connect >= 0 ? r.connect_ms : (resolveMs(r)); out.cases.push({ case: `consistency#${i + 1}`, ...r });
  }
  // 3. 异常恢复：畸形 JSON 帧
  await new Promise((res) => {
    const ws = new WebSocket(`ws://127.0.0.1:3000/v1/ws-stream/${SID}`, {
      headers: TOKEN ? { Authorization: `Bearer ${TOKEN}` } : undefined,
    });
    ws.onopen = () => { ws.send("{{{not-json"); setTimeout(() => { const alive = ws.readyState <= 1; ws.close(); out.cases.push({ case: "malformed_frame", conn_alive_after: alive }); res(); }, 1500); };
    ws.onerror = () => { out.cases.push({ case: "malformed_frame", conn_alive_after: false }); res(); };
  });
  // 4. 异常恢复：中途断连（250ms 后杀连接）
  const r4 = await wsChat("数到 20，每个数一行", { killAfter: 250 });
  out.cases.push({ case: "midstream_disconnect", ...r4 });
  // 5. 断连后同 session 再连（会话恢复能力）
  const r5 = await wsChat("继续");
  out.cases.push({ case: "reconnect_same_session", ...r5 });

  console.log(JSON.stringify(out, null, 1));
  function resolveMs() { return -1; }
})();
