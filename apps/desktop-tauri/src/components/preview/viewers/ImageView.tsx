// 图片预览：blob URL + panzoom（缩放 / 拖拽 / 滚轮缩放 / 旋转）。
//
// 之前把字节拼成 base64 data URL——8 MiB 的图要在主线程跑字符串拼接，
// 期间界面直接卡住；而且图片是死的，既不能放大看细节也不能拖动。
// blob URL 是浏览器原生句柄，零拷贝、零拼接。

import { useEffect, useMemo, useRef, useState } from "react";
import Panzoom from "@panzoom/panzoom";
import { ZoomIn, ZoomOut, RotateCw, Maximize } from "lucide-react";

export function ImageView({
  bytes,
  mime,
  onOpenViewer,
}: {
  bytes: Uint8Array;
  mime: string;
  onOpenViewer?: () => void;
}) {
  const stageRef = useRef<HTMLDivElement | null>(null);
  const imgRef = useRef<HTMLImageElement | null>(null);
  const pzRef = useRef<ReturnType<typeof Panzoom> | null>(null);

  const [rotation, setRotation] = useState(0);
  const [failed, setFailed] = useState(false);

  const src = useMemo(() => {
    // 复制一份：bytes 可能是共享缓冲区的视图，交给 Blob 前先固化
    const copy = new Uint8Array(bytes.length);
    copy.set(bytes);
    return URL.createObjectURL(new Blob([copy], { type: mime }));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [bytes, mime]);

  useEffect(() => () => URL.revokeObjectURL(src), [src]);

  useEffect(() => {
    const img = imgRef.current;
    const stage = stageRef.current;
    if (!img || !stage) return;
    const pz = Panzoom(img, { maxScale: 12, minScale: 0.1, step: 0.2 });
    pzRef.current = pz;
    // panzoom 不接管滚轮，需要宿主转发
    const onWheel = (e: WheelEvent) => {
      e.preventDefault();
      pz.zoomWithWheel(e);
    };
    stage.addEventListener("wheel", onWheel, { passive: false });
    return () => {
      stage.removeEventListener("wheel", onWheel);
      pz.destroy();
      pzRef.current = null;
    };
  }, [src]);

  return (
    <div className="preview-image">
      <div className="preview-pdf-bar">
        <button
          type="button"
          className="head-action"
          onClick={() => pzRef.current?.zoomOut()}
          aria-label="缩小"
        >
          <ZoomOut className="icon-md" />
        </button>
        <button
          type="button"
          className="head-action"
          onClick={() => pzRef.current?.zoomIn()}
          aria-label="放大"
        >
          <ZoomIn className="icon-md" />
        </button>
        <button
          type="button"
          className="head-action"
          onClick={() => {
            const next = (rotation + 90) % 360;
            setRotation(next);
            pzRef.current?.reset();
          }}
          aria-label="旋转 90°"
        >
          <RotateCw className="icon-md" />
        </button>
        <button
          type="button"
          className="head-action"
          onClick={() => pzRef.current?.reset()}
          aria-label="适应窗口"
        >
          <Maximize className="icon-md" />
        </button>
      </div>
      <div ref={stageRef} className="preview-image-stage">
        {failed ? (
          <div className="preview-state-error">图片无法解码，文件可能已损坏。</div>
        ) : (
          // 旋转放在外层：panzoom 靠 img 自身的 transform 做缩放/平移，
          // 直接写 img 的 transform 会和它互相覆盖。
          <div className="preview-image-rotor" style={{ transform: `rotate(${rotation}deg)` }}>
            <img
              ref={imgRef}
              className="preview-image-img"
              src={src}
              alt=""
              draggable={false}
              onError={() => setFailed(true)}
              onClick={onOpenViewer}
              style={{ cursor: onOpenViewer ? "pointer" : "default" }}
            />
          </div>
        )}
      </div>
    </div>
  );
}
