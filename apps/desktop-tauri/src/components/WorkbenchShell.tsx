// 宿主外壳：左画布渲染工作台代码，右对话栏由宿主渲染。
//
// 折叠按钮与对话栏都属宿主 chrome —— 工作台代码无权改动或隐藏，
// 否则工作台能把自己锁死，也能屏蔽掉需要用户知情的操作。
//
// 宿主同时是**唯一的能力网关**：iframe 内的代码没有 IPC 入口，只能
// postMessage 过来，由这里转发给 Rust 网关再把结果发回去。这样「数据和
// 本机文件怎么被访问」始终是宿主说了算，代码层改不出花来。
import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { listen } from "@tauri-apps/api/event";
import { workbenchEntryUrl, workbenchGateway, type WorkbenchManifest } from "../workbench/api";

const collapseKey = (id: string) => `hermix.workbench.${id}.chatCollapsed`;

type GatewayMethod = "db.query" | "db.exec" | "fs.read" | "fs.write" | "fs.list";

interface WorkbenchShellProps {
  manifest: WorkbenchManifest;
  chatNode: ReactNode;
}

export function WorkbenchShell({ manifest, chatNode }: WorkbenchShellProps) {
  const [entryUrl, setEntryUrl] = useState<string | null>(null);
  const [collapsed, setCollapsed] = useState(
    () => localStorage.getItem(collapseKey(manifest.id)) === "1",
  );
  // 画布没有构建步骤，重载就是热更新：key 变化强制重建 iframe 元素。
  const [reloadToken, setReloadToken] = useState(0);
  const frameRef = useRef<HTMLIFrameElement>(null);

  useEffect(() => {
    let alive = true;
    workbenchEntryUrl(manifest.id)
      .then((url) => {
        if (alive) setEntryUrl(url);
      })
      .catch(() => {
        if (alive) setEntryUrl(null);
      });
    return () => {
      alive = false;
    };
  }, [manifest.id]);

  // agent 改完代码后宿主发事件，这里收到就重建画布。
  useEffect(() => {
    const un = listen<string>("workbench://changed", (e) => {
      if (e.payload === manifest.id) setReloadToken((t) => t + 1);
    });
    return () => {
      void un.then((fn) => fn());
    };
  }, [manifest.id]);

  useEffect(() => {
    const onMessage = async (e: MessageEvent) => {
      const d = e.data as
        | { source?: string; type?: string; id?: number; method?: string; params?: Record<string, unknown> }
        | undefined;
      if (!d || d.source !== "hermix-workbench" || d.type !== "request") return;
      // 只认自己画布里来的请求，别的窗口发同名消息一概不理。
      if (e.source !== frameRef.current?.contentWindow) return;
      const target = frameRef.current?.contentWindow;
      if (!target || d.id === undefined || !d.method) return;
      try {
        const result = await workbenchGateway(
          manifest.id,
          d.method as GatewayMethod,
          d.params ?? {},
        );
        target.postMessage(
          { source: "hermix-host", type: "response", id: d.id, ok: true, result },
          "*",
        );
      } catch (err) {
        target.postMessage(
          {
            source: "hermix-host",
            type: "response",
            id: d.id,
            ok: false,
            error: err instanceof Error ? err.message : String(err),
          },
          "*",
        );
      }
    };
    window.addEventListener("message", onMessage);
    return () => window.removeEventListener("message", onMessage);
  }, [manifest.id]);

  const toggleChat = useCallback(() => {
    setCollapsed((prev) => {
      localStorage.setItem(collapseKey(manifest.id), prev ? "0" : "1");
      return !prev;
    });
  }, [manifest.id]);

  return (
    <div className="wb-shell">
      <div className="wb-canvas">
        {entryUrl ? (
          <iframe
            key={reloadToken}
            ref={frameRef}
            className="wb-frame"
            src={entryUrl}
            title={manifest.name}
          />
        ) : (
          <div className="wb-canvas-empty">工作台入口未就绪</div>
        )}
      </div>
      <button
        type="button"
        className="wb-toggle"
        onClick={toggleChat}
        title={collapsed ? "展开对话栏" : "收起对话栏"}
        aria-label={collapsed ? "展开对话栏" : "收起对话栏"}
      >
        {collapsed ? "‹" : "›"}
      </button>
      {!collapsed && <div className="wb-chat">{chatNode}</div>}
    </div>
  );
}
