// 全局 Provider 组合，从 App.tsx 抽出。
// 当前应用只通过 StreamChatContext 向下分发流式聊天 API；
// 后续若有 Theme / Store 等 Provider，统一在此处组合，App 只负责组合本模块。
import type { ReactNode, ContextType } from "react";
import { StreamChatContext } from "../context/StreamChatContext";

export function AppProviders({
  value,
  children,
}: {
  value: ContextType<typeof StreamChatContext>;
  children: ReactNode;
}) {
  return <StreamChatContext.Provider value={value}>{children}</StreamChatContext.Provider>;
}
