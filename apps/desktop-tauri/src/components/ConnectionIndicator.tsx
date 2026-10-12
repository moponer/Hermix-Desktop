import { Loader2 } from "lucide-react";
import { useBackendConnectionContext } from "../context/BackendConnectionContext";

/**
 * Connection status indicator — small colored dot shown in the sidebar.
 * Color semantics:
 *   - green   = online / connected
 *   - amber   = connecting (boot) or restarting
 *   - gray    = offline / error
 * Shows as a bare dot; no label, no hover tooltip clutter.
 *
 * 状态机与重启流程见 `useBackendConnection`。
 */
export function ConnectionIndicator() {
  const { state, mode, restarting, gaveUp, restart } = useBackendConnectionContext();

  // Visual semantics
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
        : "正在连接..."
      }
      aria-label="连接状态"
      className="connection-indicator"
      style={{ cursor: isOffline && !restarting ? "pointer" : "default" }}
    >
      {isConnecting && <Loader2 size={10} className="animate-spin opacity-70" />}
      <span className="connection-dot" style={{ background: dotColor }} />
    </button>
  );
}
