// context/StreamChatContext.tsx
//
// 把聊天 WebSocket 的拥有者（useStreamChat）提升到 App 顶层、视图切换
// 之上。ChatView（或任何视图）卸载时只暂停渲染 —— 连接和在途流继续跑，
// 切回聊天页接着收流。这是修复「切到设置页/其他页时正在跑的 agent 流被
// 掐断」的结构性保障：WS 不再挂在某个会卸载的视图组件里。
//
// 用法：
// - App 调用一次 useStreamChat(activeSessionId)，把返回值交给
//   <StreamChatContext.Provider value={api}> 包住整个视图区。
// - ChatView 用 useStreamChatContext() 读流状态（streamingStatus /
//   lastActivityAt / compactionStatus / contextUsage / usageSnapshot /
//   streamingSessionId / cancel），不再依赖父组件一层层透传。

import { createContext, useContext } from "react";
import type { StreamChatApi } from "../hooks/useStreamChat";

export const StreamChatContext = createContext<StreamChatApi | null>(null);

export function useStreamChatContext(): StreamChatApi {
  const ctx = useContext(StreamChatContext);
  if (!ctx) {
    throw new Error("useStreamChatContext must be used within a <StreamChatProvider>");
  }
  return ctx;
}
