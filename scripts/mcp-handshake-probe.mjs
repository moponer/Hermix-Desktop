// MCP 直连握手实测：node 入口直连（不经 npx/cmd），发 initialize + tools/list，
// 记录首响时延与工具数。2026-11-14 治标验证用。
import { spawn } from "node:child_process";

const ENTRIES = [
  {
    name: "bing-cn-mcp-enhanced",
    entry: "C:\\Users\\<user>\\AppData\\Roaming\\npm\\node_modules\\bing-cn-mcp-enhanced\\build\\index.js",
  },
  {
    name: "i18n-mcp-server",
    entry: "C:\\Users\\<user>\\AppData\\Roaming\\npm\\node_modules\\i18n-mcp-server\\dist\\index.js",
  },
];

function frame(obj) {
  const body = Buffer.from(JSON.stringify(obj), "utf8");
  return Buffer.concat([Buffer.from(`Content-Length: ${body.length}\r\n\r\n`, "ascii"), body]);
}

function probe(name, entry, timeoutMs = 20000) {
  return new Promise((resolve) => {
    const t0 = Date.now();
    const child = spawn("node", [entry], { stdio: ["pipe", "pipe", "pipe"] });
    let buf = Buffer.alloc(0);
    let initMs = null;
    let tools = null;
    let stderrTail = "";
    const fail = (why) => {
      try { child.kill(); } catch {}
      resolve({ name, ok: false, why, initMs, elapsed: Date.now() - t0, stderrTail: stderrTail.slice(-300) });
    };
    const timer = setTimeout(() => fail("timeout"), timeoutMs);

    child.stderr.on("data", (d) => { stderrTail += d.toString(); });
    child.on("error", (e) => { clearTimeout(timer); fail(`spawn error: ${e.message}`); });
    child.on("exit", (code) => {
      if (initMs === null) { clearTimeout(timer); fail(`exited(${code}) before response`); }
    });
    child.stdout.on("data", (d) => {
      buf = Buffer.concat([buf, d]);
      // 解析所有完整的 Content-Length 帧
      for (;;) {
        const headerEnd = buf.indexOf("\r\n\r\n");
        if (headerEnd < 0) break;
        const header = buf.subarray(0, headerEnd).toString("ascii");
        const m = header.match(/Content-Length:\s*(\d+)/i);
        if (!m) { buf = buf.subarray(headerEnd + 4); continue; }
        const len = parseInt(m[1], 10);
        const start = headerEnd + 4;
        if (buf.length < start + len) break;
        const body = buf.subarray(start, start + len).toString("utf8");
        buf = buf.subarray(start + len);
        let msg;
        try { msg = JSON.parse(body); } catch { continue; }
        if (msg.id === 1 && msg.result) {
          initMs = Date.now() - t0;
          // 发 initialized 通知 + tools/list
          child.stdin.write(frame({ jsonrpc: "2.0", method: "notifications/initialized" }));
          child.stdin.write(frame({ jsonrpc: "2.0", id: 2, method: "tools/list", params: {} }));
        } else if (msg.id === 2 && msg.result) {
          tools = (msg.result.tools || []).length;
          clearTimeout(timer);
          try { child.kill(); } catch {}
          resolve({ name, ok: true, initMs, tools, elapsed: Date.now() - t0, protocol: msg.result?.protocolVersion ?? (JSON.parse(body)?.protocolVersion), stderrTail: "" });
        }
      }
    });
    // 发 initialize
    child.stdin.write(frame({
      jsonrpc: "2.0", id: 1, method: "initialize",
      params: {
        protocolVersion: "2024-11-05",
        capabilities: {},
        clientInfo: { name: "hermix-probe", version: "0" },
      },
    }));
  });
}

const out = [];
for (const e of ENTRIES) {
  out.push(await probe(e.name, e.entry));
}
console.log(JSON.stringify(out, null, 1));
process.exit(out.every((r) => r.ok) ? 0 : 1);
