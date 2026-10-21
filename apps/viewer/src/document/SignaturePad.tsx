// 手写签名板：在画布上签名，导出成透明背景的 PNG。
//
// 为什么单独一个组件：签名是**一次性**的输入（画 → 确认 → 放到页面上），
// 与"在页面上直接画标注"（墨迹工具）是两件事 —— 签名要复用、要保持原始
// 笔迹比例，所以先在独立画布上画好，再作为一张图放到页面上。
//
// 导出前会**裁掉四周空白**（按像素扫 alpha 求包围盒）：否则一张"签在左
// 上角"的签名，放到页面上会带着一大圈透明边，占位全靠手调。

import { useCallback, useEffect, useRef, useState } from "react";
import { Eraser, PenLine, X } from "lucide-react";

const PAD_W = 720;
const PAD_H = 220;

export interface SignatureData {
  /** PNG 的 base64（不含 data: 前缀）。 */
  base64: string;
  /** 前端预览用的 data URL。 */
  url: string;
  /** 裁剪后签名图的宽高（像素）。 */
  w: number;
  h: number;
}

export function SignaturePad({
  onCancel,
  onConfirm,
}: {
  onCancel: () => void;
  onConfirm: (data: SignatureData) => void;
}) {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const drawingRef = useRef(false);
  const [empty, setEmpty] = useState(true);

  const ctx = useCallback(() => {
    const canvas = canvasRef.current;
    if (!canvas) return null;
    return canvas.getContext("2d");
  }, []);

  // 画布按 dpr 放大，避免高分屏上笔迹发虚。
  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const dpr = window.devicePixelRatio || 1;
    canvas.width = PAD_W * dpr;
    canvas.height = PAD_H * dpr;
    const c = canvas.getContext("2d");
    if (!c) return;
    c.scale(dpr, dpr);
    c.lineCap = "round";
    c.lineJoin = "round";
    c.strokeStyle = "#101828";
    c.lineWidth = 2.6;
  }, []);

  const pos = (e: React.PointerEvent<HTMLCanvasElement>) => {
    const rect = e.currentTarget.getBoundingClientRect();
    return { x: e.clientX - rect.left, y: e.clientY - rect.top };
  };

  const onDown = (e: React.PointerEvent<HTMLCanvasElement>) => {
    const c = ctx();
    if (!c) return;
    e.currentTarget.setPointerCapture(e.pointerId);
    drawingRef.current = true;
    setEmpty(false);
    const p = pos(e);
    c.beginPath();
    c.moveTo(p.x, p.y);
  };

  const onMove = (e: React.PointerEvent<HTMLCanvasElement>) => {
    if (!drawingRef.current) return;
    const c = ctx();
    if (!c) return;
    const p = pos(e);
    c.lineTo(p.x, p.y);
    c.stroke();
  };

  const onUp = () => {
    drawingRef.current = false;
  };

  const clear = () => {
    const c = ctx();
    const canvas = canvasRef.current;
    if (!c || !canvas) return;
    c.clearRect(0, 0, PAD_W, PAD_H);
    setEmpty(true);
  };

  /** 裁掉空白 → PNG。 */
  const confirm = () => {
    const canvas = canvasRef.current;
    const c = ctx();
    if (!canvas || !c) return;
    const dpr = window.devicePixelRatio || 1;
    const w = canvas.width;
    const h = canvas.height;
    const data = c.getImageData(0, 0, w, h).data;
    let minX = w;
    let minY = h;
    let maxX = -1;
    let maxY = -1;
    for (let y = 0; y < h; y++) {
      for (let x = 0; x < w; x++) {
        if (data[(y * w + x) * 4 + 3] > 8) {
          if (x < minX) minX = x;
          if (x > maxX) maxX = x;
          if (y < minY) minY = y;
          if (y > maxY) maxY = y;
        }
      }
    }
    if (maxX < 0) return; // 没画
    const pad = Math.round(4 * dpr);
    minX = Math.max(0, minX - pad);
    minY = Math.max(0, minY - pad);
    maxX = Math.min(w - 1, maxX + pad);
    maxY = Math.min(h - 1, maxY + pad);
    const cw = maxX - minX + 1;
    const ch = maxY - minY + 1;

    const out = document.createElement("canvas");
    out.width = cw;
    out.height = ch;
    const octx = out.getContext("2d");
    if (!octx) return;
    octx.drawImage(canvas, minX, minY, cw, ch, 0, 0, cw, ch);
    const url = out.toDataURL("image/png");
    onConfirm({
      base64: url.split(",")[1] ?? "",
      url,
      w: Math.round(cw / dpr),
      h: Math.round(ch / dpr),
    });
  };

  return (
    <div className="sig-overlay" role="dialog" aria-label="手写签名">
      <div className="sig-panel">
        <div className="sig-head">
          <PenLine className="icon-sm" />
          <span>手写签名</span>
          <div className="toolbar-spacer" />
          <button
            type="button"
            className="tb-btn"
            onClick={onCancel}
            title="取消"
            aria-label="取消签名"
          >
            <X className="icon-sm" />
          </button>
        </div>
        <canvas
          ref={canvasRef}
          className="sig-canvas"
          style={{ width: PAD_W, height: PAD_H }}
          onPointerDown={onDown}
          onPointerMove={onMove}
          onPointerUp={onUp}
          onPointerLeave={onUp}
        />
        {empty && <div className="sig-hint">在上方区域用鼠标或触控笔签名</div>}
        <div className="sig-actions">
          <button type="button" className="tb-btn" onClick={clear} disabled={empty}>
            <Eraser className="icon-sm" /> 清除
          </button>
          <div className="toolbar-spacer" />
          <button type="button" className="tb-btn" onClick={onCancel}>
            取消
          </button>
          <button type="button" className="tb-btn is-primary" onClick={confirm} disabled={empty}>
            确定
          </button>
        </div>
      </div>
    </div>
  );
}
