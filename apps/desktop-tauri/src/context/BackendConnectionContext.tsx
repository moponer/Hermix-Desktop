import { createContext, useContext, type ReactNode } from "react";
import { useBackendConnection } from "../hooks/useBackendConnection";

/** 与 `useBackendConnection()` 的返回值同形，用 ReturnType 派生以免漂移。 */
type BackendConnectionValue = ReturnType<typeof useBackendConnection>;

const BackendConnectionContext = createContext<BackendConnectionValue | null>(null);

/**
 * 连接状态机的单一实例 provider：探测 + 事件订阅 + 重启只跑一份，供三处
 * 连接 UI（ConnectionIndicator / TitleBarConnectionIndicator / BackendStatus）
 * 以及任何需要 online 信号的组件共享。
 *
 * 收敛前：三个组件各自 `useBackendConnection()`，同一套 30s 轮询 + 三组
 * Tauri 事件监听跑三遍，且彼此状态可能瞬时不一致（一个已 online、另一个还
 * 在上一轮探测）。挂在此处的实例对全树可见，状态天然一致。
 */
export function BackendConnectionProvider({ children }: { children: ReactNode }) {
  const value = useBackendConnection();
  return (
    <BackendConnectionContext.Provider value={value}>
      {children}
    </BackendConnectionContext.Provider>
  );
}

/** 读取共享的连接状态机。必须在 `<BackendConnectionProvider>` 内使用。 */
export function useBackendConnectionContext(): BackendConnectionValue {
  const ctx = useContext(BackendConnectionContext);
  if (!ctx) {
    throw new Error(
      "useBackendConnectionContext must be used within <BackendConnectionProvider>",
    );
  }
  return ctx;
}
