import { Loader2 } from "lucide-react";
import { useBackendConnectionContext } from "../context/BackendConnectionContext";

/**
 * Title bar connection indicator — breathing pulse animation.
 * Rendered inline in the titlebar, to the left of window controls.
 *
 * 状态机与重启流程见 `useBackendConnection`（三处连接 UI 共用一份实现）。
 */
export function TitleBarConnectionIndicator() {
  const { state, mode, restarting, gaveUp, restart } = useBackendConnectionContext();

  const isOnline  = state === "online" && mode === "local";
  const isOffline = state === "offline" || gaveUp;
  const isConnecting = state === "connecting" || restarting || mode !== "local";

  const dotColor = isOnline
    ? "var(--color-success)"
    : isOffline
      ? "var(--color-text-faint)"
      : "var(--color-warning)";

  return (
    <button
      type="button"
      onClick={isOffline && !restarting ? () => void restart() : undefined}
      title={
        isOnline   ? "后端已连接"
        : isOffline ? (gaveUp ? "重启失败，点击重试" : "后端离线，点击重启")
        : "正在连接…"
      }
      aria-label="连接状态"
      className="titlebar-conn-indicator"
      style={{ cursor: isOffline && !restarting ? "pointer" : "default" }}
      data-tauri-no-drag
    >
      {/* 脉冲环（仅在连接/启动时显示） */}
      {isConnecting && !isOffline && (
        <span className="conn-pulse-ring" />
      )}
      {/* 中心点 */}
      <span
        className="conn-dot"
        style={{ background: dotColor }}
      />
      {/* 旋转加载图标 */}
      {isConnecting && <Loader2 className="icon-xs conn-spin" />}
    </button>
  );
}
