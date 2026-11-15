// 全屏大图查看器。由 imageViewer.ts 的 openImageViewer() 唤起。
// 功能：
//  · 顶部悬浮工具栏（灰阶，符合项目无蓝紫配色）：放大 / 缩小 / 适应屏幕 /
//    向左旋转 / 向右旋转 / 下载原图 / 关闭
//  · 多图时左右切换 + 底部「n / N」计数
//  · 滚轮缩放（native 非 passive 监听，避免 passive 警告）
//  · 放大后可拖拽平移，双击在 适应/1:1 间切换
//  · 键盘：Esc 关闭，+/- 缩放，[ ] 旋转，r 复位，← → 切换
// 配色严格使用灰阶（rgba(28,28,32,...) = token #1c1c20），不引入蓝/紫。

import { useCallback, useEffect, useRef, useState } from "react";
import {
  ZoomIn,
  ZoomOut,
  RotateCw,
  RotateCcw,
  X,
  Download,
  ChevronLeft,
  ChevronRight,
  Maximize2,
} from "lucide-react";
import { onImageViewerOpen, type ImageViewItem } from "./imageViewer";

const MIN_SCALE = 0.2;
const MAX_SCALE = 8;
const clamp = (v: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, v));

export function ImageLightbox() {
  const [items, setItems] = useState<ImageViewItem[]>([]);
  const [index, setIndex] = useState(0);
  const [visible, setVisible] = useState(false);
  const [scale, setScale] = useState(1);
  const [rotation, setRotation] = useState(0);
  const [pan, setPan] = useState({ x: 0, y: 0 });
  const [dragging, setDragging] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);
  const dragRef = useRef<{ x: number; y: number; px: number; py: number } | null>(null);

  useEffect(() => {
    return onImageViewerOpen((d) => {
      setItems(d.images);
      setIndex(d.index);
      setScale(1);
      setRotation(0);
      setPan({ x: 0, y: 0 });
      setVisible(true);
    });
  }, []);

  const close = useCallback(() => setVisible(false), []);
  const zoomBy = useCallback((factor: number) => {
    setScale((s) => clamp(Number((s * factor).toFixed(3)), MIN_SCALE, MAX_SCALE));
  }, []);
  const rotate = useCallback((deg: number) => setRotation((r) => (r + deg) % 360), []);
  const reset = useCallback(() => {
    setScale(1);
    setRotation(0);
    setPan({ x: 0, y: 0 });
  }, []);
  const go = useCallback((dir: number) => {
    setItems((cur) => {
      if (cur.length === 0) return cur;
      setIndex((i) => (i + dir + cur.length) % cur.length);
      setScale(1);
      setRotation(0);
      setPan({ x: 0, y: 0 });
      return cur;
    });
  }, []);

  // 原生非 passive wheel 监听（React onWheel 为 passive，无法 preventDefault）
  useEffect(() => {
    const el = rootRef.current;
    if (!el || !visible) return;
    const onWheel = (e: WheelEvent) => {
      e.preventDefault();
      zoomBy(e.deltaY < 0 ? 1.1 : 1 / 1.1);
    };
    el.addEventListener("wheel", onWheel, { passive: false });
    return () => el.removeEventListener("wheel", onWheel);
  }, [visible, zoomBy]);

  // 键盘快捷键
  useEffect(() => {
    if (!visible) return;
    const onKey = (e: KeyboardEvent) => {
      switch (e.key) {
        case "Escape":
          close();
          break;
        case "+":
        case "=":
          zoomBy(1.2);
          break;
        case "-":
        case "_":
          zoomBy(1 / 1.2);
          break;
        case "[":
          rotate(-90);
          break;
        case "]":
          rotate(90);
          break;
        case "r":
        case "R":
          reset();
          break;
        case "ArrowLeft":
          if (items.length > 1) go(-1);
          break;
        case "ArrowRight":
          if (items.length > 1) go(1);
          break;
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [visible, items.length, zoomBy, rotate, reset, go, close]);

  if (!visible || items.length === 0) return null;
  const current = items[index];

  const onStageMouseDown = (e: React.MouseEvent) => {
    if (scale <= 1) return;
    dragRef.current = { x: e.clientX, y: e.clientY, px: pan.x, py: pan.y };
    setDragging(true);
    const move = (ev: MouseEvent) => {
      if (!dragRef.current) return;
      setPan({
        x: dragRef.current.px + (ev.clientX - dragRef.current.x),
        y: dragRef.current.py + (ev.clientY - dragRef.current.y),
      });
    };
    const up = () => {
      dragRef.current = null;
      setDragging(false);
      window.removeEventListener("mousemove", move);
      window.removeEventListener("mouseup", up);
    };
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", up);
  };

  const onDownload = () => {
    const a = document.createElement("a");
    a.href = current.src;
    a.download = current.name || "image";
    document.body.appendChild(a);
    a.click();
    a.remove();
  };

  return (
    <div
      ref={rootRef}
      onClick={close}
      style={{
        position: "fixed",
        inset: 0,
        zIndex: 2147483647,
        background: "rgba(0,0,0,0.92)",
      }}
    >
      {/* 顶部悬浮工具栏 */}
      <div
        onClick={(e) => e.stopPropagation()}
        style={{
          position: "absolute",
          top: 12,
          left: "50%",
          transform: "translateX(-50%)",
          display: "flex",
          alignItems: "center",
          gap: 4,
          padding: "6px 8px",
          background: "rgba(28,28,32,0.85)",
          border: "1px solid rgba(255,255,255,0.12)",
          borderRadius: 12,
          backdropFilter: "blur(8px)",
          zIndex: 2,
        }}
      >
        <ToolbarBtn title="放大" onClick={() => zoomBy(1.2)}>
          <ZoomIn className="icon-lg" />
        </ToolbarBtn>
        <ToolbarBtn title="缩小" onClick={() => zoomBy(1 / 1.2)}>
          <ZoomOut className="icon-lg" />
        </ToolbarBtn>
        <ToolbarBtn title="适应屏幕" onClick={reset}>
          <Maximize2 className="icon-lg" />
        </ToolbarBtn>
        <ToolbarBtn title="向左旋转" onClick={() => rotate(-90)}>
          <RotateCcw className="icon-lg" />
        </ToolbarBtn>
        <ToolbarBtn title="向右旋转" onClick={() => rotate(90)}>
          <RotateCw className="icon-lg" />
        </ToolbarBtn>
        <span
          style={{
            width: 1,
            height: 20,
            background: "rgba(255,255,255,0.12)",
            margin: "0 2px",
          }}
        />
        <ToolbarBtn title="下载原图" onClick={onDownload}>
          <Download className="icon-lg" />
        </ToolbarBtn>
        <ToolbarBtn title="关闭 (Esc)" onClick={close}>
          <X className="icon-lg" />
        </ToolbarBtn>
      </div>

      {items.length > 1 && (
        <>
          <NavBtn side="left" onClick={(e) => { e.stopPropagation(); go(-1); }}>
            <ChevronLeft size={22} />
          </NavBtn>
          <NavBtn side="right" onClick={(e) => { e.stopPropagation(); go(1); }}>
            <ChevronRight size={22} />
          </NavBtn>
          <div
            style={{
              position: "absolute",
              bottom: 14,
              left: "50%",
              transform: "translateX(-50%)",
              color: "rgba(255,255,255,0.7)",
              fontSize: 12,
            }}
          >
            {index + 1} / {items.length}
          </div>
        </>
      )}

      <div
        onClick={(e) => e.stopPropagation()}
        onMouseDown={onStageMouseDown}
        onDoubleClick={(e) => {
          e.stopPropagation();
          setScale((s) => (s > 1 ? 1 : 2));
        }}
        style={{
          position: "absolute",
          inset: 0,
          display: "flex",
          alignItems: "center",
          justifyContent: "center",
          overflow: "hidden",
          cursor: dragging ? "grabbing" : scale > 1 ? "grab" : "default",
        }}
      >
        <img
          src={current.src}
          alt={current.name || "image"}
          draggable={false}
          style={{
            maxWidth: "92%",
            maxHeight: "88%",
            transform: `translate(${pan.x}px, ${pan.y}px) scale(${scale}) rotate(${rotation}deg)`,
            transition: dragging ? "none" : "transform 0.12s ease-out",
            objectFit: "contain",
            userSelect: "none",
            boxShadow: "0 8px 40px rgba(0,0,0,0.5)",
          }}
        />
      </div>
    </div>
  );
}

function ToolbarBtn({
  title,
  onClick,
  children,
}: {
  title: string;
  onClick: () => void;
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      title={title}
      aria-label={title}
      onClick={(e) => {
        e.stopPropagation();
        onClick();
      }}
      // 恒暗图片浮层（登记的显式例外）上的 hover 高光，非主题样式。
      // design-guard: allow
      onMouseEnter={(e) => (e.currentTarget.style.background = "rgba(255,255,255,0.14)")}
      onMouseLeave={(e) => (e.currentTarget.style.background = "transparent")}
      style={{
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        width: 34,
        height: 34,
        borderRadius: 8,
        color: "rgba(255,255,255,0.85)",
        background: "transparent",
        border: "none",
        cursor: "pointer",
      }}
    >
      {children}
    </button>
  );
}

function NavBtn({
  side,
  onClick,
  children,
}: {
  side: "left" | "right";
  onClick: (e: React.MouseEvent) => void;
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      style={{
        position: "absolute",
        top: "50%",
        left: side === "left" ? 16 : undefined,
        right: side === "right" ? 16 : undefined,
        transform: "translateY(-50%)",
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        width: 40,
        height: 40,
        borderRadius: "50%",
        background: "rgba(28,28,32,0.7)",
        border: "1px solid rgba(255,255,255,0.12)",
        color: "rgba(255,255,255,0.85)",
        cursor: "pointer",
        zIndex: 2,
      }}
    >
      {children}
    </button>
  );
}
