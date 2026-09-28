// components/FloatingChat.tsx
//
// 工作台的悬浮对话：收起时是一个 48px 的球，展开时是一扇可拖动的浮窗。
//
// 为什么不是固定右栏：工作台的主产物是**画布**，固定 460px 对话栏把
// 画布永久切掉三分之一（见旧 .wb-chat 的 width: min(460px, 38vw)）。
// 改成悬浮体后画布拿回 100% 宽度，对话按需出现 —— 画布与对话不再
// 争夺同一块常驻空间。
//
// 位置记忆：球按**比例**存（x/容器宽），换分辨率或拖动窗口后球不会
// 跑到屏幕外；窗口按**视口像素**存（fixed 定位 + clientX 拖拽，坐标系
// 一致，拖哪存哪）。尺寸按像素存（用户对窗宽有偏好，不该随比例漂移）。
//
// 点任意非浮窗位置自动收起（画布 iframe 内的点击对宿主不可见，收不到
// 事件——这是 iframe 隔离的物理边界，不是缺陷；收球另有 Esc 和关闭钮）。
//
// 状态环读的是 stream 层的 streaming 信号。待审批态需要「当前会话有
// 挂起的审批卡」这一事实，而它目前只存在于 ChatView 的消息派生里，
// 不在 stream context 中 —— 越层去 DOM 查是坏味道。等它上提到全局
// 状态后，在 switch 里加一个 case 即可（见 ORB_STATE 注释）。

import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type PointerEvent as ReactPointerEvent,
  type ReactNode,
} from "react";
import { createPortal } from "react-dom";
import { MessageCircle, X } from "lucide-react";
import { useStreamChatContext } from "../context/StreamChatContext";

const ORB_SIZE = 48;
const WIN_MIN_W = 320;
const WIN_MIN_H = 320;
const WIN_DEFAULT_W = 440;
const WIN_DEFAULT_H = 560;
const EDGE_GAP = 16;

const kOrb = (id: string) => `hermix.workbench.${id}.orbPos`;
const kWin = (id: string) => `hermix.workbench.${id}.winBox`;

interface Box {
  /** 左上角坐标，视口像素（fixed 定位基准），相对视口左上角。 */
  x: number;
  y: number;
  w: number;
  h: number;
}

/** 球的位置用比例存：{"rx":0.94,"ry":0.86} */
interface OrbPos {
  rx: number;
  ry: number;
}

function readOrb(id: string): OrbPos {
  try {
    const raw = localStorage.getItem(kOrb(id));
    if (raw) {
      const p = JSON.parse(raw) as OrbPos;
      if (typeof p.rx === "number" && typeof p.ry === "number") return p;
    }
  } catch {
    /* 损坏的存档当没有 */
  }
  return { rx: 0.94, ry: 0.86 };
}

function readWin(id: string): Box {
  try {
    const raw = localStorage.getItem(kWin(id));
    if (raw) {
      const b = JSON.parse(raw) as Box;
      if ([b.x, b.y, b.w, b.h].every((n) => typeof n === "number")) return b;
    }
  } catch {
    /* 同上 */
  }
  return { x: 0, y: 0, w: WIN_DEFAULT_W, h: WIN_DEFAULT_H };
}

export function FloatingChat({
  manifestId,
  manifestName,
  chatNode,
}: {
  manifestId: string;
  manifestName: string;
  chatNode: ReactNode;
}) {
  const hostRef = useRef<HTMLDivElement>(null);
  const [open, setOpen] = useState(false);
  const [orb, setOrb] = useState<OrbPos>(() => readOrb(manifestId));
  const [box, setBox] = useState<Box>(() => readWin(manifestId));

  // ── 状态环：idle / streaming / done（一次性对勾划过）────────────
  const { streaming } = useStreamChatContext();
  const [phase, setPhase] = useState<"idle" | "busy" | "done">("idle");
  const wasStreaming = useRef(false);
  useEffect(() => {
    if (streaming) {
      wasStreaming.current = true;
      setPhase("busy");
      return;
    }
    if (!wasStreaming.current) return;
    wasStreaming.current = false;
    setPhase("done");
    const t = setTimeout(() => setPhase("idle"), 900);
    return () => clearTimeout(t);
  }, [streaming]);

  // ── 位置换算：比例 ↔ 像素 ────────────────────────────────────
  const hostSize = useCallback(() => {
    const el = hostRef.current;
    return { w: el?.clientWidth ?? 0, h: el?.clientHeight ?? 0 };
  }, []);

  const orbPx = useCallback(
    (p: OrbPos) => {
      const { w, h } = hostSize();
      return {
        left: Math.max(EDGE_GAP, Math.min(w - ORB_SIZE - EDGE_GAP, p.rx * w - ORB_SIZE / 2)),
        top: Math.max(EDGE_GAP, Math.min(h - ORB_SIZE - EDGE_GAP, p.ry * h - ORB_SIZE / 2)),
      };
    },
    [hostSize],
  );

  /** 窗口位置按**视口坐标**钳制（fixed 定位 + clientX/clientY 拖拽，
      坐标系必须一致，混用容器像素会让拖拽整体偏移）。 */
  const winBox = useCallback(
    (b: Box): Box => {
      const vw = window.innerWidth;
      const vh = window.innerHeight;
      const width = Math.max(WIN_MIN_W, Math.min(b.w || WIN_DEFAULT_W, vw - 32));
      const height = Math.max(WIN_MIN_H, Math.min(b.h || WIN_DEFAULT_H, vh - 32));
      return {
        w: width,
        h: height,
        x: Math.max(8, Math.min(vw - width - 8, b.x || vw - width - EDGE_GAP)),
        y: Math.max(8, Math.min(vh - height - 8, b.y || vh - height - EDGE_GAP)),
      };
    },
    [],
  );

  // ── 拖拽：球与窗共用一套 pointer 逻辑 ─────────────────────────
  const dragRef = useRef<{
    kind: "orb" | "win" | "resize";
    startX: number;
    startY: number;
    origin: { rx: number; ry: number } | Box;
  } | null>(null);
  // 拖过就算拖拽：pointerup 后浏览器仍会补发 click，不拦的话
  // "把球拖到别处"会顺手把浮窗打开。
  const draggedRef = useRef(false);

  const onPointerDown = (
    kind: "orb" | "win" | "resize",
    e: ReactPointerEvent<HTMLElement>,
  ) => {
    if (e.button !== 0) return;
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    if (kind === "orb") {
      const { w, h } = hostSize();
      const px = orbPx(orb);
      dragRef.current = {
        kind,
        startX: e.clientX,
        startY: e.clientY,
        // 记比例，落点换算与容器尺寸无关，拖到哪都是相对位置。
        origin: { rx: px.left / Math.max(1, w), ry: px.top / Math.max(1, h) },
      };
    } else {
      dragRef.current = {
        kind,
        startX: e.clientX,
        startY: e.clientY,
        origin: kind === "win" ? box : { ...winBox(box) },
      };
    }
  };

  const onPointerMove = (e: ReactPointerEvent<HTMLElement>) => {
    const d = dragRef.current;
    if (!d) return;
    const dx = e.clientX - d.startX;
    const dy = e.clientY - d.startY;
    if (Math.abs(dx) + Math.abs(dy) > 3) draggedRef.current = true;
    if (d.kind === "orb") {
      const o = d.origin as { rx: number; ry: number };
      setOrb({ rx: o.rx + dx / Math.max(1, hostSize().w), ry: o.ry + dy / Math.max(1, hostSize().h) });
      return;
    }
    if (d.kind === "win") {
      const o = d.origin as Box;
      setBox((b) => ({ ...b, x: o.x + dx, y: o.y + dy }));
      return;
    }
    // resize：右下角
    const o = d.origin as Box;
    setBox((b) => ({
      ...b,
      w: Math.max(WIN_MIN_W, o.w + dx),
      h: Math.max(WIN_MIN_H, o.h + dy),
    }));
  };

  const onPointerUp = (e: ReactPointerEvent<HTMLElement>) => {
    if (!dragRef.current) return;
    (e.currentTarget as HTMLElement).releasePointerCapture?.(e.pointerId);
    dragRef.current = null;
    // 让紧随其后的 click 事件读到一个稳定的"刚拖过"标记（click 在
    // pointerup 之后同步派发，下一帧前必须清掉）。
    setTimeout(() => { draggedRef.current = false; }, 0);
    // 落位即存：球存比例，窗存像素。onPointerUp 与最后一次 move 同属
    // 一帧序列，React 已提交最新值，这里直接写。
    try {
      localStorage.setItem(kOrb(manifestId), JSON.stringify(orb));
      localStorage.setItem(kWin(manifestId), JSON.stringify(box));
    } catch {
      /* 隐私模式下写不进去，不影响本次会话 */
    }
  };

  // 展开时 Esc 收球（不劫持窗内的其他 Esc 语义：浮窗里没有别的层）。
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        setOpen(false);
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [open]);

  // 点任意非浮窗位置自动收起：画布是跨文档 iframe，里面的点击对宿主
  // 完全不可见（iframe 事件隔离的物理边界）——宿主收到的 mousedown 只
  // 发生在 DOM 树内的元素上，所以"DOM 内任意位置"= 真正的外面，点就收。
  // 浮窗/球内部放行。用 mousedown 而非 click：拖拽松手补发的 click 不应
  // 误触发收球；close 钮的 stopPropagation 使"点关闭钮"走 onClick 主动收，
  // 与自动收殊途同归，无副作用。
  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      const target = e.target as HTMLElement;
      if (target.closest(".wb-float, .wb-orb")) return;
      setOpen(false);
    };
    document.addEventListener("mousedown", onDown, true);
    return () => document.removeEventListener("mousedown", onDown, true);
  }, [open]);

  // 首次布局后校正一次窗位（存档可能是旧分辨率或旧容器坐标系下的，
  // 0 坐标归一到右下角默认位，避免闪到视口左上角）。
  useLayoutEffect(() => {
    setBox((b) => {
      const hasPos = b.x !== 0 || b.y !== 0;
      const fallback: Box = { x: 0, y: 0, w: b.w || WIN_DEFAULT_W, h: b.h || WIN_DEFAULT_H };
      return winBox(hasPos ? b : fallback);
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [manifestId]);

  const px = orbPx(orb);
  const wb = winBox(box);

  return (
    <div ref={hostRef} className="wb-float-host">
      {/* 球 —— 收起态。跨页不在本设计内：离开工作台即整块卸载。 */}
      {!open && (
        <button
          type="button"
          className={`wb-orb wb-orb--${phase}`}
          style={{ left: px.left, top: px.top }}
          onPointerDown={(e) => onPointerDown("orb", e)}
          onPointerMove={onPointerMove}
          onPointerUp={onPointerUp}
          onClick={() => {
            if (draggedRef.current) return;
            setOpen(true);
          }}
          title={`${manifestName} · 打开对话`}
          aria-label={`${manifestName} 对话，点开`}
        >
          <span className="wb-orb__ring" aria-hidden="true" />
          <MessageCircle className="wb-orb__icon icon-lg" aria-hidden="true" />
          {phase === "done" && (
            <svg className="wb-orb__check" viewBox="0 0 24 24" aria-hidden="true">
              <path d="M5 12.5l4.5 4.5L19 7.5" fill="none" stroke="currentColor" strokeWidth="2.5"
                strokeLinecap="round" strokeLinejoin="round" />
            </svg>
          )}
        </button>
      )}

      {/* 浮窗 —— 展开态。portal 到 body：宿主层无"外面"概念，iframe 内点击
          会被宿主拦截，挂到 body 后 mousedown 监听才能正确判定"点在浮窗外"。 */}
      {open &&
        createPortal(
          <div
            className="wb-float"
            style={{ left: wb.x, top: wb.y, width: wb.w, height: wb.h }}
            role="dialog"
            aria-label={`${manifestName} 对话`}
          >
            <div
              className="wb-float__head"
              onPointerDown={(e) => onPointerDown("win", e)}
              onPointerMove={onPointerMove}
              onPointerUp={onPointerUp}
              title="拖动移动"
            >
              <span className={`wb-float__dot wb-float__dot--${phase}`} aria-hidden="true" />
              <span className="wb-float__title">{manifestName}</span>
              <button
                type="button"
                className="wb-float__close"
                // pointerdown 冒泡到标题栏会触发 setPointerCapture，浏览器把
                // 后续 click 重定向给捕获元素，按钮永远收不到 —— 必须拦住。
                onPointerDown={(e) => e.stopPropagation()}
                onClick={() => setOpen(false)}
                title="收成球"
                aria-label="收成球"
              >
                <X className="icon-sm" />
              </button>
            </div>
            <div className="wb-float__body">{chatNode}</div>
            <div
              className="wb-float__resize"
              onPointerDown={(e) => onPointerDown("resize", e)}
              onPointerMove={onPointerMove}
              onPointerUp={onPointerUp}
              aria-hidden="true"
            />
          </div>,
          document.body,
        )}
    </div>
  );
}
