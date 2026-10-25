import { getCurrentWindow } from "@tauri-apps/api/window";
import { invoke } from "@tauri-apps/api/core";
import { Minus, Square, X, MessageSquare, Camera, FolderTree } from "lucide-react";
import { useCallback } from "react";
import { toastWarning } from "../hooks/useToast";
import { CLOSE } from "./strings";

/** 标题栏按**三栏**排布，与下方 .app-body 的三列共用同一组列宽变量
 *  （`--ab-left-w` / `--ab-right-w`），所以每一栏的边界都对齐到正下方那一列的
 *  边界，而不是「品牌靠左、按钮靠右、中间空一大片」。
 *
 *  三栏各归其位：
 *  - 左栏 = 品牌（侧栏那一列的领地）
 *  - 中栏 = 当前对话标题（主列的领地）
 *  - 右栏 = 窗口按钮（产物栏那一列的领地）
 *
 *  窗口按钮不再额外包 `no-drag` 的弹性空白区 —— 此前整条标题栏中段都是死区
 *  （点不动、拖不动），现在除按钮本身外全条都可拖动窗口。
 *
 *  侧栏折叠 / 搜索是「内容」层级动作，已在侧栏头部与 ChatView 头部，不在此。 */
export function TitleBar({ title }: { title?: string }) {
  const handleMinimize = useCallback(() => {
    try {
      void getCurrentWindow().minimize();
    } catch (err) {
      console.error("[TitleBar] minimize failed:", err);
    }
  }, []);

  const handleMaximize = useCallback(() => {
    try {
      void getCurrentWindow().toggleMaximize();
    } catch (err) {
      console.error("[TitleBar] maximize failed:", err);
    }
  }, []);

  const handleClose = useCallback(() => {
    try {
      void getCurrentWindow().close();
    } catch (err) {
      console.error("[TitleBar] close failed:", err);
    }
  }, []);

  // 截图入口之二（热键之外）：主进程内触发，无需全局热键注册。
  // 失败要可见（功能被关/抓屏失败），不能静默——用户不知道为什么没反应。
  const handleScreenshot = useCallback(() => {
    void invoke("start_screenshot").catch((err) => {
      console.error("[TitleBar] screenshot failed:", err);
      toastWarning(String(err));
    });
  }, []);

  // 文件管理器入口：hermix-viewer 是独立进程（多格式预览 + 全盘搜索 +
  // 文件操作右键菜单），主程序里只有右栏一个窄窄的「浏览目录」，此前
  // 唯一能开到查看器的路径是右键菜单里的「在查看器中打开」——没有一级
  // 入口，用户根本不知道它存在。这里给标题栏一个常驻按钮。
  const handleFileManager = useCallback(() => {
    void invoke("open_in_viewer", { path: null }).catch((err) => {
      console.error("[TitleBar] file manager failed:", err);
      toastWarning(`文件管理器打开失败：${String(err)}`);
    });
  }, []);

  return (
    <div className="titlebar drag">
      {/* ① 左栏 —— 与侧栏同宽。墨与纸 v4：品牌区改纯文字（白粗名，
          无图标底板；版本号由侧栏头部承担，不重复）。 */}
      <div className="titlebar-zone titlebar-zone--left titlebar-brand">
        <span className="titlebar-title">Hermix</span>
      </div>

      {/* ② 中栏 —— 与主列同宽。会话标题是这一列的主语，标题栏是它的唯一出口：
          ChatView 的头部不再重复渲染标题（那处只剩左右动作按钮）。 */}
      <div className="titlebar-zone titlebar-zone--center">
        {title && (
          <>
            <MessageSquare className="icon-sm titlebar-doc-icon" aria-hidden="true" />
            <span className="titlebar-doc" title={title}>
              {title}
            </span>
          </>
        )}
      </div>

      {/* ③ 右栏 —— 与产物栏同宽。
          Each button carries both `no-drag` and `data-tauri-no-drag`:
          the class covers the CSS path, while `data-tauri-no-drag` is
          Tauri's own marker and wins regardless of CSS specificity or
          import order — belt and braces, since a swallowed click here
          would leave the window undraggable-but-unclosable. */}
      <div className="titlebar-zone titlebar-zone--right">
        <button
          type="button"
          onClick={handleFileManager}
          className="titlebar-btn no-drag"
          data-tauri-no-drag
          title="文件管理器（独立查看器）"
          aria-label="文件管理器"
        >
          <FolderTree className="icon-md" />
        </button>
        <button
          type="button"
          onClick={handleScreenshot}
          className="titlebar-btn no-drag"
          data-tauri-no-drag
          title="截图（Alt+Q）"
          aria-label="截图"
        >
          <Camera className="icon-md" />
        </button>
        <button
          type="button"
          onClick={handleMinimize}
          className="titlebar-btn no-drag"
          data-tauri-no-drag
          title="最小化"
          aria-label="最小化"
        >
          {/* 五个窗口按钮统一 icon-md（R1-2）：同一组按钮此前 md/lg 混档 */}
          <Minus className="icon-md" />
        </button>
        <button
          type="button"
          onClick={handleMaximize}
          className="titlebar-btn no-drag"
          data-tauri-no-drag
          title="最大化"
          aria-label="最大化"
        >
          <Square className="icon-md" />
        </button>
        <button
          type="button"
          onClick={handleClose}
          className="titlebar-btn titlebar-btn-danger no-drag"
          data-tauri-no-drag
          title={CLOSE}
          aria-label={CLOSE}
        >
          <X className="icon-md" />
        </button>
      </div>
    </div>
  );
}
