// 截图覆盖窗（独立 window：index.html#/screenshot-overlay）。
// 2026-10-23 重写（决策：docs/notes/implemented/feature/2026-10-23-screenshot-rewrite.md）：
// - 幕布帧改**事件推送**（hermix:screenshot-frame），页面解码完再回报
//   screenshot_overlay_ready，Rust 才亮窗 → 没有黑屏/半就绪画面。
// - **窗口自动吸附**：鼠标移动即查询光标下的窗口，画**绿色**边框 + 标题；
//   单击该窗口即选中（不必先按模式键）。拖拽仍然照常框选自由区域。
// - **选多大显示多大**：选区确认后 Rust 把覆盖窗缩到选区大小，编辑器
//   只显示选中的那部分，底下不再垫一层全屏黑底。
// - 缩放换算统一走 `devicePixelRatio`（网页自己报的缩放，Rust 不猜）。
import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { ScreenshotEditor } from "./ScreenshotEditor";
import { CANCEL, CLOSE, CONFIRM } from "./strings";

interface FramePayload {
  data_url: string;
  /** 虚拟桌面原点（物理像素） */
  origin_x: number;
  origin_y: number;
  /** 物理像素尺寸 */
  width: number;
  height: number;
  /** 会话序号：丢弃上一轮迟到的帧 */
  seq: number;
}

/** Rust FrameState（serde untagged）：null=编码中；Ready 平铺；Failed 带 error。 */
type FrameState = FramePayload | { error: string };

interface WindowRect {
  x: number;
  y: number;
  width: number;
  height: number;
  title: string;
}

/** 窗口内的矩形（CSS 像素，相对覆盖窗左上角）。 */
interface Box {
  x: number;
  y: number;
  w: number;
  h: number;
}

/** 窗口吸附查询的节流间隔（ms）：一次 EnumWindows <1ms，但别跟着鼠标跑满。 */
const SNAP_THROTTLE_MS = 40;

export function ScreenshotOverlay() {
  const [frame, setFrame] = useState<FramePayload | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  /** 窗口吸附高亮（CSS 像素，窗口内坐标）。 */
  const [snap, setSnap] = useState<Box | null>(null);
  /** 吸附到的窗口标题（显示在框上方）。 */
  const [snapTitle, setSnapTitle] = useState("");
  /** 手动拖拽选区（CSS 像素，窗口内坐标）。 */
  const [drag, setDrag] = useState<Box | null>(null);
  /** 编辑态：非 null = 已选区，src = Rust 裁好的无损 PNG data URL。 */
  const [sel, setSel] = useState<{ src: string; scale: number } | null>(null);
  /** 选区裁剪中：Rust 正在把窗口缩到选区，此时幕布要藏起来，
   *  否则会闪一下"全屏照片缩在小窗口里"。 */
  const [selecting, setSelecting] = useState(false);

  const frameRef = useRef<FramePayload | null>(null);
  frameRef.current = frame;
  const dragStartRef = useRef<{ x: number; y: number } | null>(null);
  const lastSnapKeyRef = useRef<string>("");
  const snapAtRef = useRef(0);

  const cancel = useCallback(() => {
    void invoke("screenshot_cancel").catch(() => {});
  }, []);

  // 幕布帧：先挂事件监听，再补拉一次当前帧。
  // 顺序反了会漏掉"帧已就绪、页面刚挂载"这一帧（页面再也收不到事件）。
  useEffect(() => {
    let stopped = false;
    const unlisten = listen<FramePayload>("hermix:screenshot-frame", (e) => {
      if (!stopped) applyFrame(e.payload);
    });
    void (async () => {
      try {
        const st = await invoke<FrameState | null>("screenshot_frame_data");
        if (stopped || !st) return;
        if ("error" in st) setLoadError(st.error);
        else applyFrame(st);
      } catch (e) {
        if (!stopped) setLoadError(String(e));
      }
    })();
    return () => {
      stopped = true;
      void unlisten.then((f) => f());
    };
  }, []);

  const applyFrame = useCallback((p: FramePayload) => {
    setFrame(p);
    setLoadError(null);
    setSel(null);
    setSnap(null);
    setSnapTitle("");
    setDrag(null);
    dragStartRef.current = null;
    lastSnapKeyRef.current = "";
  }, []);

  /** CSS 像素 → 虚拟屏幕物理坐标。 */
  const toPhysical = useCallback((cssX: number, cssY: number) => {
    const f = frameRef.current;
    const s = window.devicePixelRatio || 1;
    if (!f) return { x: 0, y: 0 };
    return {
      x: f.origin_x + Math.round(cssX * s),
      y: f.origin_y + Math.round(cssY * s),
    };
  }, []);

  /** 虚拟屏幕物理矩形 → 窗口内 CSS 矩形。 */
  const toLocal = useCallback((r: WindowRect): Box => {
    const f = frameRef.current;
    const s = window.devicePixelRatio || 1;
    if (!f) return { x: 0, y: 0, w: 0, h: 0 };
    return {
      x: (r.x - f.origin_x) / s,
      y: (r.y - f.origin_y) / s,
      w: r.width / s,
      h: r.height / s,
    };
  }, []);

  /** 选区确认：交给 Rust 裁剪（无损 PNG）+ 把窗口缩到选区，随后进编辑器。 */
  const beginEdit = useCallback(async (b: Box) => {
    const f = frameRef.current;
    if (!f || b.w < 4 || b.h < 4) return;
    const s = window.devicePixelRatio || 1;
    setSelecting(true);
    try {
      const src = await invoke<string>("screenshot_select", {
        x: f.origin_x + Math.round(b.x * s),
        y: f.origin_y + Math.round(b.y * s),
        width: Math.round(b.w * s),
        height: Math.round(b.h * s),
      });
      setSel({ src, scale: s });
    } catch (e) {
      setLoadError(e instanceof Error ? e.message : String(e));
    } finally {
      setSelecting(false);
    }
  }, []);

  // 键盘：ESC 取消全程；Enter 确认当前选区（拖拽框优先，否则吸附的窗口）。
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") cancel();
      else if (sel) return;
      else if (e.key === "Enter") {
        if (drag && drag.w > 4 && drag.h > 4) void beginEdit(drag);
        else if (snap) void beginEdit(snap);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [cancel, beginEdit, drag, snap, sel]);

  const onMouseDown = (e: React.MouseEvent) => {
    if (e.button === 2) return; // 右键在 mouseup 统一处理
    dragStartRef.current = { x: e.clientX, y: e.clientY };
    setSnap(null);
    setDrag({ x: e.clientX, y: e.clientY, w: 0, h: 0 });
  };

  const onMouseMove = (e: React.MouseEvent) => {
    const start = dragStartRef.current;
    if (start) {
      setDrag({
        x: Math.min(start.x, e.clientX),
        y: Math.min(start.y, e.clientY),
        w: Math.abs(e.clientX - start.x),
        h: Math.abs(e.clientY - start.y),
      });
      return;
    }
    // 吸附查询：节流 + 矩形未变不重渲染（避免 mousemove 洪泛）。
    const now = Date.now();
    if (now - snapAtRef.current < SNAP_THROTTLE_MS) return;
    snapAtRef.current = now;
    const p = toPhysical(e.clientX, e.clientY);
    void invoke<WindowRect | null>("screenshot_window_at", { x: p.x, y: p.y }).then((r) => {
      const key = r ? `${r.x},${r.y},${r.width},${r.height}` : "none";
      if (key === lastSnapKeyRef.current) return;
      lastSnapKeyRef.current = key;
      setSnap(r ? toLocal(r) : null);
      setSnapTitle(r?.title ?? "");
    });
  };

  const onMouseUp = (e: React.MouseEvent) => {
    const start = dragStartRef.current;
    dragStartRef.current = null;
    if (e.button === 2) {
      cancel();
      return;
    }
    if (!start) return;
    const moved = Math.hypot(e.clientX - start.x, e.clientY - start.y);
    if (moved > 4) {
      // 拖拽框选
      const b = {
        x: Math.min(start.x, e.clientX),
        y: Math.min(start.y, e.clientY),
        w: Math.abs(e.clientX - start.x),
        h: Math.abs(e.clientY - start.y),
      };
      setDrag(null);
      void beginEdit(b);
      return;
    }
    // 单击：选中光标下的窗口（吸附）；没有窗口则保持现状
    if (snap) void beginEdit(snap);
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

  // 编辑态：选区已确认，交给编辑器（覆盖全窗，选区 UI 不再渲染）。
  if (sel) {
    return <ScreenshotEditor source={sel} onCancel={cancel} />;
  }

  if (!frame) {
    return (
      <div className="so-loading" aria-busy="true">
        正在截取屏幕…
      </div>
    );
  }

  const box = drag && drag.w > 1 ? drag : null;

  return (
    <div
      className="so-root"
      onMouseDown={onMouseDown}
      onMouseMove={onMouseMove}
      onMouseUp={onMouseUp}
      onContextMenu={(e) => {
        e.preventDefault();
        cancel();
      }}
    >
      <img
        className="so-backdrop"
        src={frame.data_url}
        alt=""
        draggable={false}
        // 幕布解码完才亮窗：Rust 侧 show() 在此之后，窗口不会黑屏一闪。
        onLoad={() => void invoke("screenshot_overlay_ready").catch(() => {})}
        style={selecting ? { opacity: 0 } : undefined}
      />

      {/* 窗口吸附：绿色边框 + 标题（默认常驻，鼠标移到哪个窗口就吸哪个） */}
      {snap && !box && (
        <>
          <div
            className="so-snap"
            style={{ left: snap.x, top: snap.y, width: snap.w, height: snap.h }}
          />
          {snapTitle && (
            <div
              className="so-snap-label"
              style={{ left: snap.x, top: Math.max(0, snap.y - 24) }}
            >
              {snapTitle}
            </div>
          )}
        </>
      )}

      {/* 手动框选：选区外压暗 + 选区亮边框 */}
      {box && (
        <>
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
          <div
            className="so-selection"
            style={{ left: box.x, top: box.y, width: box.w, height: box.h }}
          />
          <div className="so-size-tag" style={{ left: box.x, top: Math.max(0, box.y - 26) }}>
            {Math.round(box.w * (window.devicePixelRatio || 1))} ×{" "}
            {Math.round(box.h * (window.devicePixelRatio || 1))}
          </div>
        </>
      )}

      <div className="so-toolbar">
        <span className="so-hint">
          移动鼠标自动吸附窗口 · 拖拽可框选 · Enter {CONFIRM} · ESC {CANCEL}
        </span>
        <button className="so-cancel" title={`${CANCEL}（ESC）`} onClick={cancel}>
          ✕
        </button>
      </div>
    </div>
  );
}
