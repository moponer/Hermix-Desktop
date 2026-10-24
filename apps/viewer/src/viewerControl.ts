// 查看器的 **AI 自控面**（前端侧）。
//
// 目标：让 agent 能**看清用户面前是什么**，并**直接操作查看器**——而不是去盲点
// 坐标（查看器整个界面就是一个 WebView2 HWND，对 Win32 来说里面没有控件树，
// 盲点既慢又脆）。
//
// 为什么这一层放在前端、靠 DOM 语义驱动，而不是「Rust 下发 → App 逐条处理」：
// 语义命令更稳，但要求改 App.tsx 的状态层；而界面元素本身已经有稳定的
// `aria-label` / 文本 / class，**按语义选择器驱动**同样稳，且**零耦合**——
// 不碰 App 的状态逻辑，也就不会和别的改动打架。
//
// 动作清单（Rust 侧 `viewer_action` 工具同名）：
//   open_file / open_url / set_path —— 走万能输入框（路径栏）
//   search                        —— 走搜索框
//   switch_tab / close_tab / close_all_tabs
//   menu                          —— 执行菜单栏命令（按菜单项文字）
//   open_panel                    —— 打开设置 / 系统资源 / 浏览器会话 / 关于
//   set_mode                      —— 纯净 / 重量视图
//   read_view                     —— 读当前可见界面（状态 + 预览正文）

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

/** 界面上此刻的状态（AI 看得见的东西）。 */
export interface ViewerState {
  ok: boolean;
  currentPath: string;
  pathInput: string;
  searchQuery: string;
  mode: string;
  tabs: Array<{ id: number; name: string; active: boolean }>;
  activeTab: { id: number; name: string } | null;
  previewName: string;
  panels: string[];
  notice: string;
}

/** 设置里可切的两档「网页内核」对应的菜单命令 id。 */
const MENU_BY_TEXT: Record<string, string> = {
  网页内核内嵌视图: "kernel-embedded",
  网页内核系统浏览器: "kernel-system",
};

function text(el: Element | null): string {
  return (el?.textContent ?? "").trim();
}

function q<T extends Element = HTMLElement>(selector: string): T | null {
  return document.querySelector<T>(selector);
}

function qa<T extends Element = HTMLElement>(selector: string): T[] {
  return Array.from(document.querySelectorAll<T>(selector));
}

/** 抓当前界面状态。 */
export function scrapeState(): ViewerState {
  const tabs = qa<HTMLElement>(".tab[data-tab-id]").map((el) => ({
    id: Number(el.getAttribute("data-tab-id") ?? "0"),
    name: text(el.querySelector(".tab-title")),
    active: el.classList.contains("active"),
  }));
  const active = tabs.find((t) => t.active) ?? null;
  const panels: string[] = [];
  if (q(".syspanel-overlay")) panels.push("system");
  if (q(".set-overlay")) panels.push("settings");
  if (q(".bpanel-overlay")) panels.push("browser");
  if (q(".about-overlay")) panels.push("about");
  return {
    ok: true,
    // 地址栏药丸上的 data-path（完整路径，带分隔符）。2026-10-24 起地址栏
    // 是「面包屑即地址栏」单行形态，旧 .breadcrumb-bar 容器没了；读属性
    // 也比拼 textContent 干净（旧读法各段会被连成一串）。
    currentPath: q<HTMLElement>(".addr-pill")?.dataset.path ?? "",
    pathInput: (q<HTMLInputElement>(".path-input")?.value ?? "").trim(),
    searchQuery: (q<HTMLInputElement>(".search-input")?.value ?? "").trim(),
    mode: q(".app")?.classList.contains("is-pure") ? "pure" : "heavy",
    tabs,
    activeTab: active ? { id: active.id, name: active.name } : null,
    previewName: text(q(".preview-toolbar-name")),
    panels,
    notice: text(q(".toast")),
  };
}

/** 读预览区当前可见正文（省 token：比截图便宜得多）。 */
function readPreviewText(limit = 4000): string {
  const inner = q(".preview-pane-inner");
  const raw = (inner?.textContent ?? "").replace(/\s+/g, " ").trim();
  return raw.length > limit ? `${raw.slice(0, limit)}…（已截断）` : raw;
}

/** 往受控输入框里写值：必须走原生 setter + input 事件，React 才知道。 */
function setInputValue(el: HTMLInputElement, value: string): void {
  const proto = Object.getPrototypeOf(el) as HTMLInputElement;
  const setter = Object.getOwnPropertyDescriptor(proto, "value")?.set;
  setter?.call(el, value);
  el.dispatchEvent(new Event("input", { bubbles: true }));
}

/** 提交路径栏（万能输入框：路径 / 网址 / 搜索词都走它）。 */
function submitPathBar(value: string): void {
  const input = q<HTMLInputElement>(".path-input");
  if (!input) throw new Error("找不到路径栏");
  setInputValue(input, value);
  const form = input.closest("form");
  if (form) {
    form.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true }));
  } else {
    input.closest("form")?.requestSubmit();
  }
}

/** 提交搜索框。 */
function submitSearch(value: string): void {
  const input = q<HTMLInputElement>(".search-input");
  if (!input) throw new Error("找不到搜索框");
  setInputValue(input, value);
  const form = input.closest("form");
  if (form) form.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true }));
}

/** 点菜单栏的某一项（按**显示文字**匹配，最贴近用户看到的）。 */
async function runMenuItem(label: string): Promise<void> {  const wanted = label.replace(/[：:]\s*$/, "").trim();
  // 先按 aria-label / title 找，再按可见文字找。
  let btn = qa<HTMLButtonElement>(".menubar-btn").find(
    (b) => (b.getAttribute("aria-label") ?? "").trim() === wanted || text(b) === wanted,
  );
  if (!btn) {
    for (const candidate of qa<HTMLButtonElement>(".menubar-btn")) {
      if (text(candidate).includes(wanted)) {
        btn = candidate;
        break;
      }
    }
  }
  if (!btn) throw new Error(`菜单栏里找不到「${wanted}」`);
  btn.click();
  // 菜单是 React 展开的，等一帧再找菜单项。
  return new Promise<void>((resolve, reject) => {    let tries = 0;
    const tick = () => {
      // .menu-item 同时存在于右键菜单，限定在菜单栏下拉里找。
      const item = qa<HTMLElement>(".menubar-pop .menu-item").find((el) => {
        const label = text(el.querySelector(".menu-label")) || text(el);
        return label === wanted || label.includes(wanted);
      });
      if (item) {
        item.click();
        resolve();
        return;
      }
      tries += 1;
      if (tries > 20) {
        reject(new Error(`菜单「${wanted}」里没有同名项`));
        return;
      }
      window.setTimeout(tick, 60);
    };
    window.setTimeout(tick, 60);
  });
}

/**
 * 打开右键菜单并点中某一项（按文字匹配）。
 *
 * 右键菜单**任何模式都在**（它挂在 document 上），所以它是「切视图模式」
 * 这类动作的稳定入口 —— 不像工具条，纯净模式下工具条只剩一个「重量模式」按钮。
 */
async function openContextMenuItem(pattern: RegExp): Promise<void> {
  const root = q(".app") ?? document.body;
  const rect = root.getBoundingClientRect();
  root.dispatchEvent(
    new MouseEvent("contextmenu", {
      bubbles: true,
      cancelable: true,
      clientX: Math.round(rect.left + 40),
      clientY: Math.round(rect.top + 40),
    }),
  );
  return new Promise<void>((resolve, reject) => {
    let tries = 0;
    const tick = () => {
      // .menu-label 同时存在于菜单栏下拉，限定在右键菜单里找。
      const label = qa<HTMLElement>(".ctxmenu .menu-label").find((el) =>
        pattern.test(text(el)),
      );
      if (label) {
        label.click();
        resolve();
        return;
      }
      tries += 1;
      if (tries > 30) {
        reject(new Error(`右键菜单里没找到「${pattern.source}」`));
        return;
      }
      window.setTimeout(tick, 60);
    };
    window.setTimeout(tick, 60);
  });
}

/** 当前视图模式。 */
function currentMode(): "pure" | "heavy" {
  return q(".preview")?.classList.contains("is-pure") ? "pure" : "heavy";
}

/**
 * 切视图模式。
 *
 * - **纯净模式**：右键菜单 →「纯净模式（只预览）」；
 * - **重量模式**：纯净模式工具条上的「重量模式」按钮（重量模式下走右键菜单）。
 */
async function setMode(mode: "pure" | "heavy"): Promise<void> {
  if (currentMode() === mode) return;
  if (mode === "heavy") {
    const btn = qa<HTMLElement>("button").find((b) => text(b) === "重量模式");
    if (btn) {
      btn.click();
      await waitFor(() => currentMode() === "heavy");
      return;
    }
  }
  await openContextMenuItem(mode === "pure" ? /纯净模式/ : /重量模式/);
  await waitFor(() => currentMode() === mode);
}

/** 等一个条件成立（最多 ~2s）。 */
async function waitFor(predicate: () => boolean, timeoutMs = 2000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (predicate()) return;
    await new Promise((r) => setTimeout(r, 50));
  }
}

/** 执行一个动作。 */
export async function runAction(action: string, args: Record<string, unknown>): Promise<unknown> {
  const s = (k: string): string => String(args?.[k] ?? "");
  switch (action) {
    case "open_file": {
      const value = s("path") || s("value");
      if (!value) throw new Error("open_file 需要 path 参数");
      // 走 Rust 的事件通道：**与视图模式无关**（路径栏只在重量模式存在，
      // 浏览器自动化任务默认是纯净模式 → 不能靠路径栏）。
      await invoke("viewer_open_path", { path: value });
      return { ok: true, action, value };
    }
    case "open_url": {
      const value = s("url") || s("value");
      if (!value) throw new Error("open_url 需要 url 参数");
      await invoke("viewer_open_url", { url: value });
      return { ok: true, action, value };
    }
    case "set_path": {
      const value = s("path") || s("value");
      if (!value) throw new Error("set_path 需要 path 参数");
      await setMode("heavy");
      submitPathBar(value);
      return { ok: true, action, value };
    }
    case "search": {
      const value = s("query") || s("value");
      if (!value) throw new Error("search 需要 query 参数");
      await setMode("heavy");
      submitSearch(value);
      return { ok: true, action, value };
    }
    case "switch_tab": {
      const id = Number(args?.id ?? 0);
      const tab = q<HTMLElement>(`.tab[data-tab-id="${id}"]`);
      if (!tab) throw new Error(`没有 id=${id} 的标签`);
      tab.click();
      return { ok: true, action, id };
    }
    case "close_tab": {
      const id = Number(args?.id ?? 0);
      const tab = q<HTMLElement>(`.tab[data-tab-id="${id}"]`);
      const btn = tab?.querySelector<HTMLElement>(".tab-close");
      if (!btn) throw new Error(`没有 id=${id} 的标签可关`);
      btn.click();
      return { ok: true, action, id };
    }
    case "close_all_tabs": {
      const closes = qa<HTMLElement>(".tab .tab-close");
      closes.forEach((b) => b.click());
      return { ok: true, action, closed: closes.length };
    }
    case "menu": {
      const command = s("command");
      const label = Object.entries(MENU_BY_TEXT).find(([, id]) => id === command)?.[0] ?? command;
      await runMenuItem(label);
      return { ok: true, action, command };
    }
    case "open_panel": {
      const panel = s("panel") || s("value");
      const label =
        panel === "settings"
          ? "设置…"
          : panel === "sys-panel" || panel === "system"
            ? "系统资源管理…"
            : panel === "browser"
              ? "浏览器会话…"
              : panel === "about"
                ? "关于 Hermix 文件查看器"
                : panel;
      await runMenuItem(label);
      return { ok: true, action, panel };
    }
    case "set_mode": {
      const mode = (s("mode") || s("value")) as "pure" | "heavy";
      await setMode(mode === "pure" ? "pure" : "heavy");
      return { ok: true, action, mode: currentMode() };
    }
    case "read_view": {
      return { ok: true, state: scrapeState(), previewText: readPreviewText() };
    }
    default:
      throw new Error(`未知的查看器动作：${action}`);
  }
}

/** 状态上报的节流间隔（毫秒）。 */
const REPORT_INTERVAL = 1500;

let reportTimer: number | undefined;

/** 开始 AI 自控面：听 Rust 下发的动作 + 定时上报界面状态。 */
export function startViewerControl(): () => void {
  let unlisten: UnlistenFn | undefined;
  void listen<{ id: string; action: string; args: Record<string, unknown> }>(
    "viewer-action",
    (event) => {
      const { id, action, args } = event.payload;
      void runAction(action, args ?? {})
        .then((data) => invoke("viewer_action_reply", { id, ok: true, data }))
        .catch((err: unknown) =>
          invoke("viewer_action_reply", {
            id,
            ok: false,
            data: err instanceof Error ? err.message : String(err),
          }),
        );
    },
  ).then((fn) => {
    unlisten = fn;
  });

  const report = () => {
    void invoke("viewer_state_report", { state: scrapeState() }).catch(() => undefined);
  };
  reportTimer = window.setInterval(report, REPORT_INTERVAL);
  report();

  return () => {
    unlisten?.();
    if (reportTimer !== undefined) window.clearInterval(reportTimer);
  };
}
