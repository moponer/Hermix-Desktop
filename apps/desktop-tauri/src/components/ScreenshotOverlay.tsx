// 截图覆盖窗（独立 window：index.html#/screenshot-overlay）。
// 全屏幕布上画矩形 / 窗口模式高亮；确认后进入编辑器（ScreenshotEditor），
// 编辑完成 → screenshot_commit_edited → 剪贴板。
// 幕布帧经轮询 invoke("screenshot_frame_data") 获取：Rust 侧窗口创建后
// 立即后台编码（与页面加载并行），None=编码中、Ready=渲染并 show、
// Failed=错误 UI。页面首帧前由 index.html #overlay-boot 静态占位。
// 交互契约见 docs/notes/implemented/feature/2026-10-07-screenshot-tool.md。
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { ScreenshotEditor } from "./ScreenshotEditor";
import { CANCEL, CLOSE, CONFIRM } from "./strings";

interface FramePayload {
  data_url: string;
  origin_x: number;
  origin_y: number;
  width: number;
  height: number;
  scale: number;
}

/** Rust FrameState（serde untagged）：null=编码中；Ready 平铺；Failed 带 error。 */
type FrameState = FramePayload | { error: string };

interface WindowRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

type Mode = "rect" | "window";

export function ScreenshotOverlay() {
  const [frame, setFrame] = useState<FramePayload | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [mode, setMode] = useState<Mode>("rect");
  // 选区：物理像素（虚拟屏幕坐标）；state 存逻辑像素（CSS 尺寸）。
  const [dragStart, setDragStart] = useState<{ x: number; y: number } | null>(null);
  const [rect, setRect] = useState<{ x: number; y: number; w: number; h: number } | null>(null);
  const [hoverWin, setHoverWin] = useState<WindowRect | null>(null);
  // 编辑态：非 null = 选区已确认，进入编辑器（值 = 选区在帧图内的物理像素矩形）。
  const [sel, setSel] = useState<{ sx: number; sy: number; sw: number; sh: number } | null>(null);
  const frameRef = useRef<FramePayload | null>(null);
  frameRef.current = frame;
  // 窗口模式 hover 去抖：rect 没变不 setState，避免 mousemove 洪泛重渲染。
  const lastHoverKeyRef = useRef<string>("");

  // 轮询幕布帧：Rust 侧窗口建好后后台线程编码 JPEG（与页面加载并行），
  // 150ms 一跳；Ready → 渲染 + show 窗口，Failed → 错误 UI，30s 上限与
  // Rust 侧 30s 关窗兜底同量级（前端先到先显示超时态，Rust 侧保证不留
  // 隐形死窗）。轮询期间 .so-loading「正在截取屏幕…」持续显示。
  useEffect(() => {
    let stopped = false;
    let pollTimer: number | undefined;
    const deadline = Date.now() + 30_000;
    const tick = async () => {
      try {
        while (!stopped && Date.now() < deadline) {
          const st = await invoke<FrameState | null>("screenshot_frame_data");
          if (stopped) return;
          if (!st) {
            pollTimer = window.setTimeout(() => void tick(), 150);
            return;
          }
          if ("error" in st) {
            setLoadError(st.error);
            // 失败也要亮窗：错误面板直接可见可退（ESC / 关闭按钮），
            // 不再靠主窗 toast 单通道。
            void getCurrentWindow().show().catch(() => {});
          } else {
            setFrame(st);
            void getCurrentWindow().show().catch(() => {});
          }
          return;
        }
        if (!stopped) setLoadError("截图幕布 30s 未就绪");
      } catch (e) {
        if (!stopped) setLoadError(String(e));
      }
    };
    void tick();
    return () => {
      stopped = true;
      if (pollTimer !== undefined) window.clearTimeout(pollTimer);
    };
  }, []);

  const cancel = useCallback(() => {
    void invoke("screenshot_cancel").catch(() => {});
  }, []);

  /** 选区确认（矩形松开 / 窗口模式点击）：转帧图物理像素，进入编辑器。
   *  帧图 (0,0) 对应屏幕 (origin_x, origin_y)，故本地逻辑坐标 × scale
   *  即帧图像素，无需 origin。 */
  const beginEdit = useCallback((r: { x: number; y: number; w: number; h: number }) => {
    const f = frameRef.current;
    if (!f || r.w < 4 || r.h < 4) return;
    const s = f.scale;
    const sx = Math.max(0, Math.round(r.x * s));
    const sy = Math.max(0, Math.round(r.y * s));
    setSel({
      sx,
      sy,
      sw: Math.min(f.width - sx, Math.round(r.w * s)),
      sh: Math.min(f.height - sy, Math.round(r.h * s)),
    });
  }, []);

  // 编辑器基底按需走 Rust 无损裁剪（screenshot_crop_data，只传选区不传整帧）。
  // memo 保证 source 对象恒等——父组件重渲染不触发编辑器重取。
  const editorSource = useMemo(
    () => (sel && frame ? { ...sel, scale: frame.scale } : null),
    [sel, frame],
  );

  // 键盘：ESC 取消全程；Enter 确认 / Tab 切模式仅选区阶段有效（编辑态
  // Enter 归文本输入用）。
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") cancel();
      else if (sel) return;
      else if (e.key === "Enter" && rect) beginEdit(rect);
      else if (e.key === "Tab") {
        e.preventDefault();
        setMode((m) => (m === "rect" ? "window" : "rect"));
        setRect(null);
        lastHoverKeyRef.current = "";
        setHoverWin(null);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [cancel, beginEdit, rect, sel]);

  // 屏幕逻辑坐标 → 本地：监听期缓存窗口位置。
  const winPosRef = useRef<{ x: number; y: number }>({ x: 0, y: 0 });
  useEffect(() => {
    const f = frameRef.current;
    if (!f) return;
    void getCurrentWindow()
      .outerPosition()
      .then((p) => {
        winPosRef.current = { x: p.x / f.scale - f.origin_x, y: p.y / f.scale - f.origin_y };
      });
  }, [frame]);

  const onMouseDown = (e: React.MouseEvent) => {
    if (e.button === 2) {
      cancel();
      return;
    }
    if (mode === "window") return; // window 模式点击即截（在 click 里处理）
    const p = { x: e.screenX - winPosRef.current.x, y: e.screenY - winPosRef.current.y };
    setDragStart(p);
    setRect({ x: p.x, y: p.y, w: 0, h: 0 });
  };

  const onMouseMove = (e: React.MouseEvent) => {
    const p = { x: e.screenX - winPosRef.current.x, y: e.screenY - winPosRef.current.y };
    if (mode === "window" && !dragStart) {
      const f = frameRef.current;
      if (!f) return;
      const phys = {
        x: f.origin_x + Math.round(p.x * f.scale),
        y: f.origin_y + Math.round(p.y * f.scale),
      };
      void invoke<WindowRect | null>("screenshot_window_at", { x: phys.x, y: phys.y }).then(
        (r) => {
          const win = r ?? null;
          const key = win ? `${win.x},${win.y},${win.width},${win.height}` : "none";
          if (key !== lastHoverKeyRef.current) {
            lastHoverKeyRef.current = key;
            setHoverWin(win);
          }
        },
      );
      return;
    }
    if (dragStart) {
      setRect({
        x: Math.min(dragStart.x, p.x),
        y: Math.min(dragStart.y, p.y),
        w: Math.abs(p.x - dragStart.x),
        h: Math.abs(p.y - dragStart.y),
      });
    }
  };

  const onMouseUp = () => {
    if (mode === "rect" && dragStart && rect && rect.w > 2 && rect.h > 2) {
      beginEdit(rect);
    }
    setDragStart(null);
  };

  const onClick = () => {
    if (mode === "window" && hoverWin) {
      const f = frameRef.current;
      if (!f) return;
      beginEdit({
        x: hoverWin.x / f.scale - f.origin_x,
        y: hoverWin.y / f.scale - f.origin_y,
        w: hoverWin.width / f.scale,
        h: hoverWin.height / f.scale,
      });
    }
  };

  if (loadError) {
    return (
      <div className="so-loading">
        <div className="so-error">
          <div>截图初始化失败：{loadError}，请重新截图。</div>
          <button type="button" onClick={cancel}>
            {CLOSE}（ESC）
          </button>
        </div>
      </div>
    );
  }

  if (!frame) {
    return <div className="so-loading" aria-busy="true">正在截取屏幕…</div>;
  }

  // 编辑态：选区已确认，交给编辑器（覆盖全窗，选区 UI 不再渲染）。
  if (sel && editorSource) {
    return <ScreenshotEditor source={editorSource} onCancel={cancel} />;
  }

  // hover 窗口高亮框（本地逻辑坐标）
  const hoverLocal =
    hoverWin && frame
      ? {
          x: hoverWin.x / frame.scale - frame.origin_x - winPosRef.current.x,
          y: hoverWin.y / frame.scale - frame.origin_y - winPosRef.current.y,
          w: hoverWin.width / frame.scale,
          h: hoverWin.height / frame.scale,
        }
      : null;

  const box = mode === "window" && hoverLocal ? hoverLocal : rect;

  return (
    <div
      className="so-root"
      onMouseDown={onMouseDown}
      onMouseMove={onMouseMove}
      onMouseUp={onMouseUp}
      onClick={onClick}
      onContextMenu={(e) => {
        e.preventDefault();
        cancel();
      }}
    >
      <img className="so-backdrop" src={frame.data_url} alt="" draggable={false} />
      {box && box.w > 0 && (
        <>
          {/* 遮罩四片：选区外变暗，选区内原图亮度 */}
          <div className="so-shade" style={{ left: 0, top: 0, width: "100%", height: box.y }} />
          <div
            className="so-shade"
            style={{ left: 0, top: box.y + box.h, width: "100%", bottom: 0 }}
          />
          <div className="so-shade" style={{ left: 0, top: box.y, width: box.x, height: box.h }} />
          <div
            className="so-shade"
            style={{ left: box.x + box.w, top: box.y, right: 0, height: box.h }}
          />
          <div className="so-selection" style={{ left: box.x, top: box.y, width: box.w, height: box.h }} />
          <div className="so-size-tag" style={{ left: box.x, top: Math.max(0, box.y - 26) }}>
            {Math.round(box.w * frame.scale)} × {Math.round(box.h * frame.scale)}
          </div>
        </>
      )}
      <div className="so-toolbar">
        <button
          className={mode === "rect" ? "active" : ""}
          title="矩形选区（默认）"
          onClick={() => setMode("rect")}
        >
          ▭ 矩形
        </button>
        <button
          className={mode === "window" ? "active" : ""}
          title="自动识别窗口"
          onClick={() => setMode("window")}
        >
          ⛶ 窗口
        </button>
        {rect && (
          <button className="so-confirm" title={`${CONFIRM}（Enter），进入编辑`} onClick={() => beginEdit(rect)}>
            ✓
          </button>
        )}
        <button className="so-cancel" title={`${CANCEL}（ESC）`} onClick={cancel}>
          ✕
        </button>
      </div>
    </div>
  );
}
