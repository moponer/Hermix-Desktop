// 宿主外壳：左画布渲染工作台代码，右对话栏由宿主渲染。
//
// 折叠按钮与对话栏都属宿主 chrome —— 工作台代码无权改动或隐藏，
// 否则工作台能把自己锁死，也能屏蔽掉需要用户知情的操作。
//
// 宿主同时是**唯一的能力网关**：iframe 内的代码没有 IPC 入口，只能
// postMessage 过来，由这里转发给 Rust 网关再把结果发回去。这样「数据和
// 本机文件怎么被访问」始终是宿主说了算，代码层改不出花来。
import { useEffect, useRef, useState, type ReactNode } from "react";
import { listen } from "@tauri-apps/api/event";
import { workbenchEntryUrl, workbenchGateway, type WorkbenchManifest } from "../workbench/api";
import { WorkbenchChat } from "./WorkbenchChat";
import { FloatingChat } from "./FloatingChat";

type GatewayMethod = "db.query" | "db.exec" | "fs.read" | "fs.write" | "fs.list";

interface WorkbenchShellProps {
  manifest: WorkbenchManifest;
  chatNode: ReactNode;
}

export function WorkbenchShell({ manifest, chatNode }: WorkbenchShellProps) {
  // chatNode 包上工作台专属 chrome（chip + 改动状态条），让右栏一眼区别于主对话。
  const chat = <WorkbenchChat manifest={manifest} chatNode={chatNode} />;
  const [entryUrl, setEntryUrl] = useState<string | null>(null);
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

  // 让宿主主题色透出 iframe：onLoad 时把 html/body 背景强制透明，
  // 父容器 .wb-canvas 的 var(--color-bg-primary) 即可透出。
  // 工作台代码若主动设 !important 背景色，宿主不阻拦 —— 这里只兜底
  // "未开发工作台"的默认白底，让默认态跟随主程序主题。
  // memory: 主题色系不同启动的也不同 —— 默认画布不能割裂。
  useEffect(() => {
    const frame = frameRef.current;
    if (!frame) return;
    const apply = () => {
      try {
        const doc = frame.contentDocument;
        if (!doc) return;
        doc.documentElement.style.background = "transparent";
        doc.body.style.setProperty("background", "transparent", "important");
      } catch {
        // cross-origin — 跳过兜底。
      }
    };
    apply();
    frame.addEventListener("load", apply);
    return () => frame.removeEventListener("load", apply);
  }, [reloadToken, entryUrl]);

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
            allowTransparency
          />
        ) : (
          <div className="wb-canvas-empty">工作台入口未就绪</div>
        )}
      </div>
      {/* 对话改成悬浮体：收起是一个可拖的球，展开是一扇可拖可缩的浮窗。
          画布因此拿到 100% 宽度 —— 固定右栏曾永久切掉它三分之一。 */}
      <FloatingChat manifestId={manifest.id} manifestName={manifest.name} chatNode={chat} />
    </div>
  );
}
