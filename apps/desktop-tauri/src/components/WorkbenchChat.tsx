// 工作台对话栏 chrome。
//
// 右侧对话栏与主对话共用同一个 ChatView 实例（chatNode），本组件不改
// 消息流逻辑，只在外层加工作台专属 chrome（WORKBENCH_CHAT_REDESIGN_PLAN
// 方案 A）。原顶部 chip 已删：悬浮浮窗的标题栏已显示工作台名，chip
// 只会叠出双层标题。保留底部状态条：最近代码改动时间（workbench://changed）。
//
// 不做「应用到工作台」按钮（plan §九问 4 已定：热更新自动生效，按钮冗余）。
import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import type { WorkbenchManifest } from "../workbench/api";
import type { ReactNode } from "react";

interface WorkbenchChatProps {
  manifest: WorkbenchManifest;
  chatNode: ReactNode;
}

export function WorkbenchChat({ manifest, chatNode }: WorkbenchChatProps) {
  // null = 本次会话还没有改动记录；收到 workbench://changed 后记时间。
  const [lastChange, setLastChange] = useState<number | null>(null);

  useEffect(() => {
    let alive = true;
    const un = listen<string>("workbench://changed", (e) => {
      if (e.payload === manifest.id && alive) setLastChange(Date.now());
    });
    return () => {
      alive = false;
      void un.then((fn) => fn());
    };
  }, [manifest.id]);

  return (
    <div className="wb-chat-frame">
      <div className="wb-chat-body">{chatNode}</div>
      <div className="wb-chat-status">
        {lastChange === null ? (
          <span className="wb-chat-status-muted">代码改动会自动应用到画布</span>
        ) : (
          <span className="wb-chat-status-live">
            <span className="wb-chat-status-dot" aria-hidden="true" />
            已应用 · {new Date(lastChange).toLocaleTimeString()}
          </span>
        )}
      </div>
    </div>
  );
}
