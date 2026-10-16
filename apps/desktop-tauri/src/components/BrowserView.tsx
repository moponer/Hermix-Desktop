// 浏览器小窗（独立 Tauri 子窗：index.html#/browser-view）。
// 渲染无头 Edge 的 screencast JPEG 帧流（Rust 侧 stdio_bridge 经
// emit("stdio-frame", channel="browser-frame") 推送）——本机不存在任何浏览器
// 窗口，不抢焦点、不进 Alt+Tab；所有浏览器输入由 agent 经 CDP 合成事件完成。
//
// 多会话（2026-10-17）：每个对话一个浏览器实例，帧 payload 带 `session` 键。
// 小窗顶部渲染会话标签；默认跟随最新活动的会话，点标签钉住某一个。
//
// 契约（与 Rust 侧对应）：
// - mount 后自调 getCurrentWindow().show() + setFocus()：Rust 先建后亮，
//   白屏不会被看到；show 不提前台，必须显式聚焦（否则被主窗盖住）。
// - 小窗不可见（最小化 / 完全遮挡 / document.hidden）时上报暂停（带当前
//   会话键，停那一路 Chrome 编码）。刻意不含 blur——小窗核心场景恰是
//   「焦点在主窗看聊天、小窗在旁边直播」。
// - 帧流全程不落盘；「保存当前帧」是唯一写盘路径（前端 canvas toBlob）。
import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";

/** `startResizeDragging` 的方向参数类型（v2.12 未导出 `ResizeDirection`，从方法签名推导）。 */
type ResizeDir = Parameters<ReturnType<typeof getCurrentWindow>["startResizeDragging"]>[0];
import "../styles/browser-view.css";

interface BrowserFramePayload {
  /** data:image/jpeg;base64,… 直接喂 Image。 */
  data: string;
  /** 原始字节数。 */
  bytes: number;
  /** 帧所属的浏览器会话键（_session_id，缺省 "default"）。 */
  session?: string;
}

const DEFAULT_SESSION = "default";

/** 会话显示名：UUID 太长，截前 8 位。 */
function sessionLabel(key: string): string {
  return key === DEFAULT_SESSION ? "默认" : key.length > 8 ? `${key.slice(0, 8)}…` : key;
}

export function BrowserView() {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const [paused, setPaused] = useState(false);
  const [fps, setFps] = useState(0);
  const [frameBytes, setFrameBytes] = useState(0);
  const [everFramed, setEverFramed] = useState(false);
  const pausedRef = useRef(false);
  pausedRef.current = paused;

  // 多会话状态：session → 最新帧。order 按「最近有帧」降序；selected 为
  // null 表示跟随最新（默认），点标签后钉住该会话。
  const framesRef = useRef<Map<string, { data: string; bytes: number; at: number }>>(new Map());
  const [sessionOrder, setSessionOrder] = useState<string[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const selectedRef = useRef<string | null>(null);
  selectedRef.current = selected;

  // 帧渲染循环的节流阀：rAF 合并——JPEG 解码是主线程活，20fps 的流足够，
  // 无需每帧都画（也画不过来）。pending 标记保证不丢最新帧。
  const pendingFrameRef = useRef<{ data: string; session: string } | null>(null);
  const rafRef = useRef<number | null>(null);

  const drawPending = useCallback(() => {
    rafRef.current = null;
    const pending = pendingFrameRef.current;
    if (!pending) return;
    pendingFrameRef.current = null;
    const canvas = canvasRef.current;
    if (!canvas) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    const img = new Image();
    img.onload = () => {
      // 容器尺寸变化时画布跟随；drawImage 拉伸铺满（监看场景无所谓形变）。
      if (canvas.width !== img.width || canvas.height !== img.height) {
        canvas.width = img.width;
        canvas.height = img.height;
      }
      ctx.drawImage(img, 0, 0);
    };
    img.src = pending.data;
  }, []);

  // 帧监听 + fps 统计（按会话；只统计正在显示的那一路）。
  useEffect(() => {
    let frames = 0;
    const fpsTimer = window.setInterval(() => {
      setFps(frames);
      frames = 0;
    }, 1000);

    const unlisten = listen<{ channel: string; payload: string }>("stdio-frame", (event) => {
      if (event.payload.channel !== "browser-frame") return;
      if (pausedRef.current) return; // 暂停闸门：隐藏时不再渲染新帧
      let parsed: BrowserFramePayload;
      try {
        parsed = JSON.parse(event.payload.payload) as BrowserFramePayload;
      } catch {
        return;
      }
      const session = parsed.session || DEFAULT_SESSION;
      const info = { data: parsed.data, bytes: parsed.bytes, at: Date.now() };
      framesRef.current.set(session, info);
      setSessionOrder((prev) => {
        const rest = prev.filter((k) => k !== session);
        return [session, ...rest];
      });
      setEverFramed(true);

      // 只画当前显示的会话；跟随模式下最新帧即当前会话。
      const showing = selectedRef.current ?? session;
      if (session !== showing) return;
      setFrameBytes(info.bytes);
      frames += 1;
      pendingFrameRef.current = { data: parsed.data, session };
      if (rafRef.current === null) {
        rafRef.current = requestAnimationFrame(drawPending);
      }
    });

    return () => {
      window.clearInterval(fpsTimer);
      void unlisten.then((f) => f());
      if (rafRef.current !== null) cancelAnimationFrame(rafRef.current);
    };
  }, [drawPending]);

  // mount 完即亮窗（Rust visible(false) 先建后亮）。show 不提前台——主窗
  // 盖在小窗上方时用户会以为没打开（2026-10-17 实测），show 后必须 setFocus。
  useEffect(() => {
    const win = getCurrentWindow();
    void win
      .show()
      .then(() => win.setFocus())
      .catch(() => {});
  }, []);

  // 不可见上报：只在 document.hidden（小窗被最小化/完全遮挡）时暂停，
  // 停的是当前显示会话那一路 Chrome 编码。恢复可见时取消暂停。手动暂停
  // （togglePaused）仍随时可用。**不含 blur**——小窗的核心场景恰恰是
  // 「用户焦点在主窗看聊天、小窗在旁边直播」。
  const reportPaused = useCallback((next: boolean, session: string) => {
    void invoke("browser_view_set_paused", { paused: next, session }).catch(() => {});
  }, []);

  useEffect(() => {
    const onVisibility = () => {
      const session = selectedRef.current ?? framesRef.current.keys().next().value;
      if (session) reportPaused(document.hidden, session);
      setPaused(document.hidden);
    };
    document.addEventListener("visibilitychange", onVisibility);
    return () => {
      document.removeEventListener("visibilitychange", onVisibility);
    };
  }, [reportPaused]);

  const togglePaused = useCallback(() => {
    const next = !pausedRef.current;
    setPaused(next);
    const session = selectedRef.current ?? framesRef.current.keys().next().value;
    if (session) reportPaused(next, session);
  }, [reportPaused]);

  // 切换会话：钉住目标；旧会话那路恢复推帧（不再被看），新会话按当前
  // 暂停态上报。选中帧立即绘制（从缓存，无需等下一帧）。
  const switchSession = useCallback(
    (key: string) => {
      const prev = selectedRef.current ?? framesRef.current.keys().next().value;
      if (prev && prev !== key) reportPaused(false, prev);
      setSelected(key);
      const cached = framesRef.current.get(key);
      if (cached) {
        setFrameBytes(cached.bytes);
        pendingFrameRef.current = { data: cached.data, session: key };
        if (rafRef.current === null) {
          rafRef.current = requestAnimationFrame(drawPending);
        }
      }
      reportPaused(pausedRef.current, key);
    },
    [drawPending, reportPaused],
  );

  // 最大化/还原（decorations(false) 后系统按钮没了，得自建）。
  const handleMaximize = useCallback(() => {
    void getCurrentWindow().toggleMaximize().catch(() => {});
  }, []);

  // 边缘缩放：decorations(false) 后系统边框消失，靠这些热区发起 OS 缩放。
  const startResize = useCallback(
    (direction: ResizeDir) => (e: React.MouseEvent) => {
      e.preventDefault();
      void getCurrentWindow().startResizeDragging(direction).catch(() => {});
    },
    [],
  );

  // 保存当前帧：canvas → PNG 下载。帧流本身不落盘，这是唯一写盘路径。
  const saveFrame = useCallback(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    canvas.toBlob((blob) => {
      if (!blob) return;
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      const stamp = new Date().toISOString().replace(/[:.]/g, "-");
      a.href = url;
      a.download = `browser-frame-${stamp}.png`;
      a.click();
      URL.revokeObjectURL(url);
    }, "image/png");
  }, []);

  const sessionCount = sessionOrder.length;
  const activeSession = selected ?? sessionOrder[0] ?? DEFAULT_SESSION;

  return (
    <div className="bv-root">
      <div className="bv-bar" data-tauri-drag-region>
        <span className="bv-title">浏览器小窗</span>
        <span className="bv-status">
          {everFramed ? (
            <>
              <span className={paused ? "bv-dot bv-dot--paused" : "bv-dot bv-dot--live"} />
              {paused ? "已暂停（画面仍在后台运行）" : `${fps} fps · ${(frameBytes / 1024).toFixed(0)} KB/帧`}
            </>
          ) : (
            <>
              <span className="bv-dot bv-dot--idle" />
              等待浏览器会话…
            </>
          )}
        </span>
        <div className="bv-actions">
          <button type="button" className="bv-btn" onClick={handleMaximize}>
            最大化
          </button>
          <button type="button" className="bv-btn" onClick={togglePaused}>
            {paused ? "恢复" : "暂停"}
          </button>
          <button type="button" className="bv-btn" onClick={saveFrame} disabled={!everFramed}>
            保存当前帧
          </button>
          <button
            type="button"
            className="bv-btn bv-btn--close"
            onClick={() => void getCurrentWindow().close().catch(() => {})}
          >
            关闭
          </button>
        </div>
      </div>
      {sessionCount > 1 && (
        <div className="bv-tabs" role="tablist" aria-label="浏览器会话">
          {sessionOrder.map((key) => {
            const active = key === activeSession;
            return (
              <button
                key={key}
                type="button"
                role="tab"
                aria-selected={active}
                className={`bv-tab${active ? " bv-tab--active" : ""}`}
                onClick={() => switchSession(key)}
                title={key}
              >
                {sessionLabel(key)}
              </button>
            );
          })}
        </div>
      )}
      <div className="bv-stage">
        <canvas ref={canvasRef} className="bv-canvas" />
        {!everFramed && (
          <div className="bv-placeholder">
            浏览器会话尚未建立。
            <br />
            模型调用浏览器工具（如打开网页、点击元素）后，这里会实时显示页面画面。
          </div>
        )}
      </div>
      <div className="bv-resize-layer">
        <div className="bv-resize bv-resize--n" onMouseDown={startResize("North")} />
        <div className="bv-resize bv-resize--s" onMouseDown={startResize("South")} />
        <div className="bv-resize bv-resize--w" onMouseDown={startResize("West")} />
        <div className="bv-resize bv-resize--e" onMouseDown={startResize("East")} />
        <div className="bv-resize bv-resize--nw" onMouseDown={startResize("NorthWest")} />
        <div className="bv-resize bv-resize--ne" onMouseDown={startResize("NorthEast")} />
        <div className="bv-resize bv-resize--sw" onMouseDown={startResize("SouthWest")} />
        <div className="bv-resize bv-resize--se" onMouseDown={startResize("SouthEast")} />
      </div>
    </div>
  );
}
