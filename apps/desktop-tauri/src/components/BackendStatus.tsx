import { Cpu, Loader2, RefreshCw, Wifi, WifiOff } from "lucide-react";
import { useBackendConnectionContext } from "../context/BackendConnectionContext";
import { toastError } from "../hooks/useToast";

export function BackendStatus() {
  // 三态语义与重启流程统一在 `useBackendConnection` 里
  // （"connecting" = sidecar 已 spawn 等首次健康探测；"online" =
  //  探测通过；"offline" = 探测失败或收到 gave-up 事件）。
  // 旧实现连接中返回 null，在 hermix.exe 的 2-6s 启动窗口里看起来
  // 像界面卡死，故此处改为显式"连接中"呈现。
  const { state, mode, restarting, gaveUp, restart } = useBackendConnectionContext();

  const handleRestart = async () => {
    // 重启失败要给用户可见反馈，不能静默 —— 失败后 UI 只是回到离线，
    // 用户会以为按钮没生效。
    if (!(await restart())) {
      toastError("后端服务重启失败，请尝试手动重启");
    }
  };

  // Shared restart button (shown once the user is in a definable state —
  // i.e. not "connecting"). During boot the connecting pill already
  // signals activity; adding a restart button there would invite the
  // user to fight an in-progress startup.
  const restartBtn = state !== "connecting" ? (
    <button
      onClick={() => void handleRestart()}
      disabled={restarting}
      className="ml-1 shrink-0 px-2 h-6 rounded text-tiny font-medium text-text-secondary border border-border-secondary hover:bg-bg-tertiary transition-colors disabled:opacity-50"
      title="重新启动本地后端服务"
      aria-label="重新连接后端服务">
      重新连接
    </button>
  ) : null;

  if (mode === "local") {
    if (gaveUp) {
      return (
        <div className="flex items-center gap-1.5 px-2 py-1 rounded text-tiny text-amber-400" title="后端多次重启失败，需要手动重启">
          <WifiOff className="icon-sm text-amber-400" />
          <span>重启失败</span>
          {restartBtn}
        </div>
      );
    }
    if (restarting) {
      return (
        <div className="flex items-center gap-1.5 px-2 py-1 rounded text-tiny text-zinc-400" title="后端正在重启...">
          <RefreshCw className="icon-sm animate-spin text-zinc-400" />
          <span>重启中...</span>
        </div>
      );
    }
    if (state === "connecting") {
      // Boot window — backend spawned, health probe not yet returned
      // OK. Don't lie with "离线" while the user can see the spinner
      // and trust that we're doing work.
      return (
        <div className="flex items-center gap-1.5 px-2 py-1 rounded text-tiny text-text-muted" title="本地后端正在启动...">
          <Loader2 className="icon-sm animate-spin text-text-muted" />
          <span>正在连接</span>
        </div>
      );
    }
    if (state === "offline") {
      return (
        <div className="flex items-center gap-1.5 px-2 py-1 rounded text-tiny text-text-muted" title="本地模式 — 后端未连接">
          <WifiOff className="icon-sm text-text-muted" />
          <span>离线</span>
          {restartBtn}
        </div>
      );
    }
    return (
      <div className="flex items-center gap-1.5 px-2 py-1 rounded text-tiny text-text-muted" title="本地模式 — 内嵌 agent 运行时">
        <Cpu className="icon-sm text-success" />
        <span>本地已连接</span>
      </div>
    );
  }

  return (
    <div
      className="flex items-center gap-1.5 px-2 py-1 rounded text-tiny text-text-muted"
      title={
        state === "online"
          ? "已连接到后端"
          : state === "offline"
            ? "后端未连接"
            : "正在连接后端..."
      }
    >
      {state === "connecting" ? (
        <>
          <Loader2 className="icon-sm animate-spin text-text-muted" />
          <span>正在连接</span>
        </>
      ) : state === "online" ? (
        <>
          <Wifi className="icon-sm text-success" />
          <span>已连接</span>
        </>
      ) : (
        <>
          <WifiOff className="icon-sm text-text-muted" />
          <span>离线</span>
          {restartBtn}
        </>
      )}
    </div>
  );
}
