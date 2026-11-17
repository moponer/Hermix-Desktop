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
        // workbenchEntryUrl() 会调 convertFileSrc(path, "asset") 把磁盘路径
        // 转成 asset:// URL。伪实现里少了它 → 抛错 → entryUrl 落成 null →
        // 画布永远显示「工作台入口未就绪」。这不是应用的 bug，是 harness
        // 缺口。dev 下 vite 的 public/ 直接可服务，转成 http 路径才能真的
        // 加载进 iframe。
        convertFileSrc: (filePath, protocol = "asset") => {
          const pub = "apps/desktop-tauri/public/";
          const norm = String(filePath).replace(/\\/g, "/");
          const idx = norm.indexOf(pub);
          if (protocol === "asset" && idx >= 0) {
            return `http://[::1]:1420/${norm.slice(idx + pub.length)}`;
          }
          return `http://[::1]:1420/${norm.split("/").pop()}`;
        },
        invoke: (cmd, args) => {
          if (cmd === "workbench_list") return Promise.resolve(list);
          if (cmd === "workbench_entry_path") {
            // 指向真实存在的文件：不存在的路径只会让画布走降级分支
            // （显示 "工作台入口未就绪"），那验不到画布真的渲染。
            return Promise.resolve(
              "<workspace>/hermix-desktop/apps/desktop-tauri/public/demo-render.html",
            );
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
        plugins: {},
      },
    });
    // `unregisterListener` 挂在独立对象上，不在 `__TAURI_INTERNALS__` 里。
    // 放错位置会报 unhandledrejection: Cannot read properties of undefined
    // (reading 'unregisterListener')，并把工作台的 effect 打断在半路。
    Object.defineProperty(window, "__TAURI_EVENT_PLUGIN_INTERNALS__", {
      configurable: true,
      value: { unregisterListener: () => {}, registerListener: () => 1 },
    });
  }, LIST);

  const logs = [];
  page.on("console", (m) => { if (m.type() === "error") logs.push(m.text().slice(0, 160)); });

  await page.setViewportSize({ width: 1440, height: 900 });
  // vite dev server 只监听 IPv6 `[::1]`（netstat 里没有 127.0.0.1 那条）。
  // 写 `localhost` 时 chromium 会先试 IPv4 并在那里挂住 → goto 超时。
  // 直接钉 IPv6 字面量。
  await page.goto(process.env.HERMIX_DEV_URL || "http://[::1]:1420");
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
      // 画布真的渲染出 iframe 了吗？「工作台入口未就绪」是 entryUrl 为空的
      // 降级文案，跟 hasFrameZone 同时为真也说明画布是空的 —— 分开报。
      hasFrame: !!t(".wb-frame"),
      canvasEmpty: t(".wb-canvas-empty")?.textContent.trim() ?? null,
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

  const shot = `<workspace>/hermix-desktop/outputs/wb-${scenario}.png`;
  await page.screenshot({ path: shot });
  return JSON.stringify({ scenario, ...probe, errors: logs, shot }, null, 2);
};

// 直接 `node scripts/verify-workbench-ui.cjs [场景]` 就能跑。之前这个文件
// 只导出函数、没有 runner，`node` 跑它只会静默退出（exit 0、零输出）——
// 看起来像「全绿」，其实一个断言都没执行。
if (require.main === module) {
  (async () => {
    const PW = process.env.PLAYWRIGHT_MODULE
      || "C:/Users/<user>/AppData/Roaming/npm/node_modules/playwright";
    let chromium;
    try {
      ({ chromium } = require(PW));
    } catch {
      console.error(`找不到 playwright（试过 ${PW}）。设 PLAYWRIGHT_MODULE 指向其安装目录。`);
      process.exit(1);
    }

    const browser = await chromium.launch({
      // 本机没下过 playwright 自带的 chromium（ms-playwright 里只有 daemon
      // 目录），直接用系统 Chrome。没有就退到 Edge —— 两个都装了的机器上
      // 这一层不该让脚本跑不起来。
      channel: process.env.PW_CHANNEL || "chrome",
    }).catch(() => chromium.launch({ channel: "msedge" }));
    try {
      const page = await browser.newPage();
      console.log(await module.exports(page));
    } finally {
      await browser.close();
    }
  })().catch((e) => {
    console.error(e);
    process.exit(1);
  });
}
