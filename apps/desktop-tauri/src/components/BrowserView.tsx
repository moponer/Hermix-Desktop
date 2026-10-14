// 浏览器小窗（独立 Tauri 子窗：index.html#/browser-view）。
// 渲染无头 Edge 的 screencast JPEG 帧流（Rust 侧 browser_view_controller
// 经 emit_to("browser-frame") 推送）——本机不存在任何浏览器窗口，不抢焦点、
// 不进 Alt+Tab；所有浏览器输入由 agent 经 CDP 合成事件完成。
//
// 契约（与 Rust 侧对应）：
// - mount 后自调 getCurrentWindow().show()：Rust 先建后亮，白屏不会被看到。
// - 小窗不可见（最小化 / 被遮挡 / document.hidden）时上报暂停：Rust 只停
//   Rust→前端推流，Chrome 照常出帧（broadcast 丢旧帧兜底）。刻意不停
//   Chrome 侧 screencast——ego-browser 实测：采集后端停流后窗口恢复时
//   反复重建会画面抖动。
// - 帧流全程不落盘；「保存当前帧」是唯一写盘路径（前端 canvas toBlob）。
import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import "../styles/browser-view.css";

interface BrowserFramePayload {
  /** data:image/jpeg;base64,… 直接喂 Image。 */
  data: string;
  /** 原始字节数。 */
  bytes: number;
}

export function BrowserView() {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const [paused, setPaused] = useState(false);
  const [fps, setFps] = useState(0);
  const [frameBytes, setFrameBytes] = useState(0);
  const [everFramed, setEverFramed] = useState(false);
  const pausedRef = useRef(false);
  pausedRef.current = paused;

  // 帧渲染循环的节流阀：rAF 合并——JPEG 解码是主线程活，20fps 的流足够，
  // 无需每帧都画（也画不过来）。pending 标记保证不丢最新帧。
  const pendingFrameRef = useRef<string | null>(null);
  const rafRef = useRef<number | null>(null);

  const drawPending = useCallback(() => {
    rafRef.current = null;
    const data = pendingFrameRef.current;
    if (!data) return;
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
    img.src = data;
  }, []);

  // 帧监听 +  fps 统计。
  useEffect(() => {
    let frames = 0;
    const fpsTimer = window.setInterval(() => {
      setFps(frames);
      frames = 0;
    }, 1000);

    const unlisten = listen<BrowserFramePayload>("browser-frame", (event) => {
      frames += 1;
      setFrameBytes(event.payload.bytes);
      setEverFramed(true);
      pendingFrameRef.current = event.payload.data;
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

  // mount 完即亮窗（Rust visible(false) 先建后亮）。
  useEffect(() => {
    void getCurrentWindow().show().catch(() => {});
  }, []);

  // 不可见上报：document.hidden（最小化/遮挡）与窗口 blur 都算“没人看”。
  // 恢复时取消暂停。Rust 侧只停推流不停 Chrome 出帧，见模块头注释。
  useEffect(() => {
    const setPausedBoth = (next: boolean) => {
      setPaused(next);
      void invoke("browser_view_set_paused", { paused: next }).catch(() => {});
    };
    const onVisibility = () => setPausedBoth(document.hidden);
    const onBlur = () => setPausedBoth(true);
    const onFocus = () => setPausedBoth(false);
    document.addEventListener("visibilitychange", onVisibility);
    window.addEventListener("blur", onBlur);
    window.addEventListener("focus", onFocus);
    return () => {
      document.removeEventListener("visibilitychange", onVisibility);
      window.removeEventListener("blur", onBlur);
      window.removeEventListener("focus", onFocus);
    };
  }, []);

  const togglePaused = useCallback(() => {
    const next = !pausedRef.current;
    setPaused(next);
    void invoke("browser_view_set_paused", { paused: next }).catch(() => {});
  }, []);

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
      <div className="bv-stage">
        <canvas ref={canvasRef} className="bv-canvas" />
        {!everFramed && (
          <div className="bv-placeholder">
            浏览器会话尚未建立。
            <br />
            模型调用浏览器工具后，这里会实时显示页面画面。
          </div>
        )}
      </div>
    </div>
  );
}
