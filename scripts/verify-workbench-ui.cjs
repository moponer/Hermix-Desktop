// 工作台页面三种状态的浏览器验证：主台直达 / 多台 tab / 空态。
// 纯浏览器里 __TAURI_INTERNALS__ 不存在会崩，先注入伪实现。
const scenario = process.argv[2] || "primary";

const FAKE = {
  primary: [
    { id: "safety", name: "安全管理工作台", session_id: "sess-1", created_at: 1, updated_at: 300, deps: [], is_primary: true },
    { id: "address", name: "地址簿工作台", session_id: "sess-2", created_at: 2, updated_at: 200, deps: [], is_primary: false },
    { id: "ledger", name: "台账工作台", session_id: null, created_at: 3, updated_at: 100, deps: [], is_primary: false },
  ],
  empty: [],
  // 没有显式主台标记：必须回落到最近更新的那个，而不是给空态。
  fallback: [
    { id: "old", name: "老工作台", session_id: "sess-9", created_at: 1, updated_at: 100, deps: [], is_primary: false },
    { id: "new", name: "新工作台", session_id: "sess-8", created_at: 2, updated_at: 900, deps: [], is_primary: false },
  ],
};

const LIST = FAKE[scenario];

module.exports = async (page) => {
  await page.addInitScript((list) => {
    localStorage.setItem("hermix.desktop.active_session_id", "sess-1");
    Object.defineProperty(window, "__TAURI_INTERNALS__", {
      configurable: true,
      value: {
        metadata: {
          currentWindow: { label: "main" },
          currentWebview: { label: "main", windowLabel: "main" },
        },
        transformCallback: () => 1,
        invoke: (cmd, args) => {
          if (cmd === "workbench_list") return Promise.resolve(list);
          if (cmd === "workbench_entry_path") {
            return Promise.resolve("<repo>/.workbuddy/memory/nonexistent.html");
          }
          if (cmd === "workbench_ensure_session") return Promise.resolve(args.sessionId);
          if (cmd === "workbench_set_primary") return Promise.resolve(undefined);
          if (cmd === "backend_http_request") {
            const path = (args && args.path) || "";
            if (path === "/v1/sessions") {
              return Promise.resolve(JSON.stringify({
                status: 200, enc: "utf8",
                body: JSON.stringify({
                  sessions: [
                    { id: "sess-1", title: "安全工作台会话", message_count: 2, created_at: "2026-09-21T00:00:00Z", updated_at: "2026-09-21T00:00:00Z" },
                    { id: "sess-2", title: "地址簿会话", message_count: 0, created_at: "2026-09-21T00:00:00Z", updated_at: "2026-09-21T00:00:00Z" },
                  ],
                }),
              }));
            }
            if (path.endsWith("/messages")) {
              return Promise.resolve(JSON.stringify({
                status: 200, enc: "utf8",
                body: JSON.stringify({ messages: [], context_usage: null }),
              }));
            }
            return Promise.resolve(JSON.stringify({ status: 200, enc: "utf8", body: "{}" }));
          }
          return Promise.resolve("{}");
        },
        unregisterListener: () => {},
        plugins: {},
      },
    });
  }, LIST);

  const logs = [];
  page.on("console", (m) => { if (m.type() === "error") logs.push(m.text().slice(0, 160)); });

  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto("http://localhost:1420");
  await page.waitForTimeout(2500);

  // 侧栏里点到工作台
  const nav = page.locator('nav, [class*="sidebar"]').getByText("工作台", { exact: true }).first();
  await nav.click();
  await page.waitForTimeout(2000);

  const probe = await page.evaluate(() => {
    const t = (s) => document.querySelector(s);
    const all = (s) => Array.from(document.querySelectorAll(s));
    return {
      tabs: all(".wb-tab").map((e) => e.textContent.trim()),
      activeTab: t(".wb-tab-active")?.textContent.trim() ?? null,
      // 主台标记点是否出现
      dots: all(".wb-tab-dot").length,
      hasShell: !!t(".wb-shell"),
      hasFrameZone: !!t(".wb-canvas"),
      chatWidth: t(".wb-chat") ? Math.round(t(".wb-chat").getBoundingClientRect().width) : 0,
      // 空态
      emptyTitle: t(".wb-empty-title")?.textContent.trim() ?? null,
      promptText: t(".wb-empty-prompt")?.textContent.trim().slice(0, 60) ?? null,
      copyBtn: all(".wb-empty-block-head .wb-btn").map((e) => e.textContent.trim()),
      createInput: !!t(".wb-empty-create .wb-input"),
      // 老列表页的痕迹必须彻底消失
      legacyList: all(".wb-list, .wb-card-row, .wb-card-name").length,
      bodyText: document.body.innerText.replace(/\s+/g, " ").slice(0, 260),
    };
  });

  const shot = `<repo>/outputs/wb-${scenario}.png`;
  await page.screenshot({ path: shot });
  return JSON.stringify({ scenario, ...probe, errors: logs, shot }, null, 2);
};
