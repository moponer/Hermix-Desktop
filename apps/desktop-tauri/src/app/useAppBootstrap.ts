// 启动初始化副作用，从 App.tsx 抽出。
// 仅包含不依赖 App 组件内部 state 的自包含副作用：窗口高度适配、
// 旧版 localStorage key 迁移、dev 构建下的 localStorage 诊断 dump。
import { useEffect } from "react";
import { getCurrentWindow, LogicalSize } from "@tauri-apps/api/window";
import { storage, STORAGE_KEYS, KNOWN_KEYS } from "../utils/storageCompat";

/**
 * 执行一次性启动引导：
 * 1. 窗口高度自适应屏幕（避开任务栏，不阻塞启动流程）。
 * 2. 把 `hermes.*` 旧 key 迁移到 `hermix.*`，并清除欢迎界面 draft 槽位。
 * 3. dev 构建下把所有 `hermix.*` localStorage 打到 console 便于排查。
 *
 * `defaultWindowWidth` 来自 App 的 `DEFAULT_WINDOW_WIDTH`（三列临界视口
 * 下打开右栏时把窗口扩到的宽度），此处用于窗口高度适配时的 setSize 宽度。
 */
export function useAppBootstrap(defaultWindowWidth: number): void {
  // 窗口高度自适应屏幕 — Tauri 默认 860px 高，在 768p 屏幕（1366×768 笔记本）
  // 上窗口底部 92px 落到屏幕外。启动时若窗口高度超出屏幕可用高度则缩到刚好适配。
  // 注意：outerSize 返回物理像素，需除以 scaleFactor 换算逻辑像素再跟 screen.availHeight 比较。
  // 同时避开 Windows 任务栏/系统栏占用（availHeight < screen.height）。
  // 不阻塞启动流程 — 失败也不影响 app 正常运行。
  useEffect(() => {
    const fit = async () => {
      try {
        const appWindow = getCurrentWindow();
        const outer = await appWindow.outerSize();
        const factor = await appWindow.scaleFactor();
        const logicalH = Math.max(1, Math.round(outer.height / factor));
        // 用 screen.availHeight 而不是 screen.height，避开任务栏/系统栏高度。
        // 额外留 40px 底边距让用户能托动窗口边缘。
        const maxH = window.screen.availHeight - 40;
        if (logicalH > maxH) {
          await appWindow.setSize(new LogicalSize(defaultWindowWidth, maxH));
        }
      } catch {
        // best-effort: 启动流程不因窗口适配失败而阻塞
      }
    };
    fit();
  }, [defaultWindowWidth]);

  // One-shot: copy any `hermes.*` localStorage entries forward to
  // `hermix.*` so users on a pre-rename install keep their settings.
  // Idempotent: gated by `hermix.compat.migrated` flag inside the helper.
  useEffect(() => {
    // Iterate the canonical registry so newly added STORAGE_KEYS are
    // auto-migrated. Draft keys are intentionally cleared below,
    // not migrated, to avoid cross-restart residue in the welcome UI.
    storage.migrateLegacyKeys(KNOWN_KEYS);

    // 欢迎界面（无 session）用的 draft 槽位（hermix.desktop.draft.*）不持久化，
    // 否则上次的选择会跨重启残留。每次启动清除，让用户在新会话从头选择。
    try { storage.removeItem(STORAGE_KEYS.draftExpertId); } catch {}
    try { storage.removeItem(STORAGE_KEYS.draftWorkspace); } catch {}

    // 启动诊断：把所有 hermix.* key 打到 console，便于在 devtools 直接看 localStorage
    // 实际状态 — 区分"数据没写"还是"数据写了没读"。**仅在 dev build 跑**
    // （import.meta.env.DEV）：生产构建时 Vite 直接 dead-code-eliminate，不影响产物。
    // 可读 localStorage 失败时静默，避免控制台噪音。
    if (import.meta.env.DEV) {
      try {
        const dump: Record<string, string> = {};
        for (let i = 0; i < window.localStorage.length; i++) {
          const k = window.localStorage.key(i);
          if (k && k.startsWith("hermix.")) {
            const v = window.localStorage.getItem(k) ?? "";
            dump[k] = v.length > 200 ? v.slice(0, 200) + `…(${v.length} chars)` : v;
          }
        }
        console.info("[hermix-boot] localStorage dump:", dump);
        // Also expose a console helper so the user can re-dump at any time
        // after interacting with Settings (without a hard reload).
        (window as unknown as { __hermixDumpLs?: () => void }).__hermixDumpLs = () => {
          const out: Record<string, string> = {};
          for (let i = 0; i < window.localStorage.length; i++) {
            const k = window.localStorage.key(i);
            if (k && k.startsWith("hermix.")) {
              const v = window.localStorage.getItem(k) ?? "";
              out[k] = v.length > 200 ? v.slice(0, 200) + `…(${v.length} chars)` : v;
            }
          }
          console.info("[hermix-ls] dump:", out);
          return out;
        };
      } catch {}
    }
  }, []);
}
