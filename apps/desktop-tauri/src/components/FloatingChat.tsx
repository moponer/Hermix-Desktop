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
import { History, MessageCircle, Plus, X } from "lucide-react";
import { useStreamChatContext } from "../context/StreamChatContext";
import { getSessions } from "../api/rest";
import type { Session } from "../types";
import { formatSidebarTime } from "../utils/time";
import { SkeletonCard } from "./ui";

const ORB_SIZE = 48;
const WIN_MIN_W = 320;
const WIN_MIN_H = 320;
const WIN_DEFAULT_W = 440;
const WIN_DEFAULT_H = 560;
const EDGE_GAP = 16;

const kOrb = (id: string) => `hermix.workbench.${id}.orbPos`;
// v2：窗口坐标从容器系换视口系后弃用旧存档，旧 key 留着不管。
const kWin = (id: string) => `hermix.workbench.${id}.winBox.v2`;
// 停靠态（贴视口右缘、全高）与浮动态是两种使用姿势，各自记忆。
const kDocked = (id: string) => `hermix.workbench.${id}.docked`;

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
  onNewSession,
  onSelectSession,
  activeSessionId,
}: {
  manifestId: string;
  manifestName: string;
  chatNode: ReactNode;
  /** 开一条属于本工作台的新会话（创建+绑定+盖戳+切换），由 WorkbenchView 实现。 */
  onNewSession: () => void;
  /** 切到本工作台的某条历史会话（画布本就跨会话共享，切换只是换对话线）。 */
  onSelectSession: (id: string) => void;
  /** 当前激活的会话 id —— 历史列表里高亮用。 */
  activeSessionId: string | null;
}) {
  const hostRef = useRef<HTMLDivElement>(null);
  // 浮窗本体：拖拽 / 缩放期间直接改 style（零 React 渲染），松手才提交。
  const floatRef = useRef<HTMLDivElement | null>(null);
  const [open, setOpen] = useState(false);
  const [docked, setDocked] = useState<boolean>(() => {
    try { return localStorage.getItem(kDocked(manifestId)) === "true"; } catch { return false; }
  });
  const [orb, setOrb] = useState<OrbPos>(() => readOrb(manifestId));
  const [box, setBox] = useState<Box>(() => readWin(manifestId));

  // ── 历史会话下拉 ─────────────────────────────────────────────
  // 画布（工作台文件）跨会话共享；对话线按会话分叉。历史下拉让人在
  // 同一个工作台里来回切对话线，而不必去主侧栏翻（那里根本不过滤出来）。
  const [menuOpen, setMenuOpen] = useState(false);
  const [history, setHistory] = useState<Session[] | null>(null);
  const [historyLoading, setHistoryLoading] = useState(false);
  const toggleHistory = useCallback(() => {
    setOpen(true);
    setMenuOpen((v) => {
      if (!v) {
        setHistoryLoading(true);
        getSessions(manifestId)
          .then((list) => setHistory(list))
          .catch(() => setHistory(null))
          .finally(() => setHistoryLoading(false));
      }
      return !v;
    });
  }, [manifestId]);

  /** 双击头部切换：停靠（贴视口右缘、全高、不再失焦收起）⇄ 浮动。 */
  const toggleDock = useCallback(() => {
    setDocked((d) => {
      const next = !d;
      try { localStorage.setItem(kDocked(manifestId), String(next)); } catch { /* 存不进就算了 */ }
      return next;
    });
    // 停靠样式不含 transform；浮窗拖拽残留的 translate3d 若不清掉，
    // 停靠窗会被平移到旧位置。
    if (floatRef.current) floatRef.current.style.transform = "";
    setOpen(true);
    setMenuOpen(false);
  }, [manifestId]);

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
      坐标系必须一致，混用容器像素会让拖拽整体偏移）。
      默认首现位：水平贴右、垂直居中；x/y 为 0 视为未定位走默认。 */
  const winBox = useCallback(
    (b: Box): Box => {
      const vw = window.innerWidth;
      const vh = window.innerHeight;
      const width = Math.max(WIN_MIN_W, Math.min(b.w || WIN_DEFAULT_W, vw - 32));
      const height = Math.max(WIN_MIN_H, Math.min(b.h || WIN_DEFAULT_H, vh - 32));
      const defX = vw - width - EDGE_GAP;
      const defY = Math.round((vh - height) / 2);
      return {
        w: width,
        h: height,
        x: Math.max(8, Math.min(vw - width - 8, b.x ? b.x : defX)),
        y: Math.max(8, Math.min(vh - height - 8, b.y ? b.y : defY)),
      };
    },
    [],
  );

  // ── 拖拽：球与窗共用一套 pointer 逻辑 ─────────────────────────
  const dragRef = useRef<{
    kind: "orb" | "win" | "resize" | "dockresize";
    startX: number;
    startY: number;
    origin: { rx: number; ry: number } | Box;
  } | null>(null);
  // 拖过就算拖拽：pointerup 后浏览器仍会补发 click，不拦的话
  // "把球拖到别处"会顺手把浮窗打开。
  const draggedRef = useRef(false);
  // 拖拽/缩放期间的最终几何值；onPointerUp 提交进 state + localStorage。
  const dragCommit = useRef<Box | null>(null);

  const onPointerDown = (
    kind: "orb" | "win" | "resize" | "dockresize",
    e: ReactPointerEvent<HTMLElement>,
  ) => {
    if (e.button !== 0) return;
    // 停靠态窗口固定在右缘，拖移动没有意义；左缘把手负责改宽度。
    if (kind === "win" && docked) return;
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
    // ── 浮窗拖拽 / 缩放：直接改 style，不走 setState ──────────────
    // 此前每个 pointermove 都 setBox → FloatingChat 整体重渲染（连带
    // portal 里整棵 ChatView 子树做协调），这就是拖动卡顿的根源。
    // 现在拖动期间 React 完全静止，只写合成器友好的 transform / 尺寸，
    // 松手（onPointerUp）才把最终值提交进 state 和 localStorage。
    const el = floatRef.current;
    if (!el) return;
    if (d.kind === "win") {
      const o = d.origin as Box;
      const vw = window.innerWidth;
      const vh = window.innerHeight;
      const next = {
        x: Math.max(8, Math.min(vw - o.w - 8, o.x + dx)),
        y: Math.max(8, Math.min(vh - o.h - 8, o.y + dy)),
      };
      el.style.transform = `translate3d(${next.x}px, ${next.y}px, 0)`;
      dragCommit.current = { x: next.x, y: next.y, w: o.w, h: o.h };
      return;
    }
    if (d.kind === "resize") {
      const o = d.origin as Box;
      const w = Math.max(WIN_MIN_W, o.w + dx);
      const h = Math.max(WIN_MIN_H, o.h + dy);
      el.style.width = `${w}px`;
      el.style.height = `${h}px`;
      dragCommit.current = { x: o.x, y: o.y, w, h };
      return;
    }
    // 停靠态左缘把手：右缘固定，宽度 = 视口右边界 - 指针 x。
    if (d.kind === "dockresize") {
      const o = d.origin as Box;
      const w = Math.max(WIN_MIN_W, Math.min(window.innerWidth - 320, window.innerWidth - e.clientX));
      el.style.width = `${w}px`;
      dragCommit.current = { x: o.x, y: o.y, w, h: o.h };
    }
  };

  const onPointerUp = (e: ReactPointerEvent<HTMLElement>) => {
    if (!dragRef.current) return;
    (e.currentTarget as HTMLElement).releasePointerCapture?.(e.pointerId);
    dragRef.current = null;
    // 让紧随其后的 click 事件读到一个稳定的"刚拖过"标记（click 在
    // pointerup 之后同步派发，下一帧前必须清掉）。
    setTimeout(() => { draggedRef.current = false; }, 0);
    // 提交：球走 state（渲染用），窗把拖拽期间直写的 style 固化进
    // state + localStorage。orb 的落位在 orb 分支的 setState 里已是最新。
    try {
      localStorage.setItem(kOrb(manifestId), JSON.stringify(orb));
      if (dragCommit.current) {
        const c = dragCommit.current;
        dragCommit.current = null;
        const merged = winBox({ x: c.x ?? box.x, y: c.y ?? box.y, w: c.w ?? box.w, h: c.h ?? box.h });
        setBox(merged);
        localStorage.setItem(kWin(manifestId), JSON.stringify(merged));
      } else {
        localStorage.setItem(kWin(manifestId), JSON.stringify(winBox(box)));
      }
    } catch {
      /* 隐私模式下写不进去，不影响本次会话 */
    }
  };

  // 展开时 Esc 收球（不劫持窗内的其他 Esc 语义：浮窗里没有别的层）。
  // 停靠态下 Esc = 取消停靠并收球，与关闭钮一致。
  useEffect(() => {
    if (!open && !docked) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        if (docked) {
          setDocked(false);
          try { localStorage.setItem(kDocked(manifestId), "false"); } catch { /* 忽略 */ }
        }
        setOpen(false);
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [open, docked, manifestId]);

  // 点任意非浮窗位置自动收起 —— 两条通道，因为宿主只能看见半个世界：
  // 1. 宿主 DOM：document capture mousedown，目标不在浮窗/球内 → 收。
  // 2. 画布 iframe：跨文档事件宿主完全收不到（iframe 隔离），只能借焦点
  //    信号 —— 点击 iframe 后宿主 window blur、activeElement 变成 iframe。
  //    副作用：Alt-Tab 切走应用同样触发收起 —— 浮窗是 popover 语义，
  //    失焦即收符合直觉，可接受。
  // 用 mousedown 而非 click：拖拽松手补发的 click 不应误触发收球；
  // close 钮的 stopPropagation 使"点关闭钮"走 onClick 主动收，殊途同归。
  useEffect(() => {
    // 停靠态是常驻面板：点外面 / 点画布都不收起 —— 收起只走关闭钮或 Esc。
    if (!open || docked) return;
    const onDown = (e: MouseEvent) => {
      const target = e.target as HTMLElement;
      if (target.closest(".wb-float, .wb-orb")) return;
      setOpen(false);
    };
    const onBlur = () => {
      if (document.activeElement instanceof HTMLIFrameElement) setOpen(false);
    };
    document.addEventListener("mousedown", onDown, true);
    window.addEventListener("blur", onBlur);
    return () => {
      document.removeEventListener("mousedown", onDown, true);
      window.removeEventListener("blur", onBlur);
    };
  }, [open, docked]);

  // 首次布局后校正一次窗位（存档可能是旧分辨率下的；0/越界坐标由
  // winBox 归一到默认位，不会闪到视口角落）。
  useLayoutEffect(() => {
    setBox((b) => winBox(b));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [manifestId]);

  const px = orbPx(orb);
  const wb = winBox(box);
  // 拖拽期间 style 直写在 DOM 上，但流式 chunk 到达也会触发本组件重渲染 ——
  // React 会用 state 里的旧坐标覆盖掉拖拽中的位置。渲染时优先读 dragCommit，
  // 让重渲染与直写一致；松手提交后二者天然相同。
  const live = dragCommit.current;
  const floatStyle = {
    left: live?.x ?? wb.x,
    top: live?.y ?? wb.y,
    width: live?.w ?? wb.w,
    height: live?.h ?? wb.h,
  };
  // 停靠态：贴视口右缘、标题栏（40px）以下全高，宽度沿用浮窗宽（左缘把手可调）。
  const dockStyle = {
    top: 40,
    right: 0,
    bottom: 0,
    width: Math.max(WIN_MIN_W, Math.min(wb.w, window.innerWidth - 320)),
  };

  return (
    <div ref={hostRef} className="wb-float-host">
      {/* 球 —— 收起态。停靠时窗口常驻，球不出现。跨页不在本设计内：离开工作台即整块卸载。 */}
      {!open && !docked && (
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
      {(open || docked) &&
        createPortal(
          <div
            ref={floatRef}
            className={`wb-float ${docked ? "wb-float--docked" : ""}`}
            style={docked ? dockStyle : floatStyle}
            role="dialog"
            aria-label={`${manifestName} 对话`}
          >
            <div
              className="wb-float__head"
              onPointerDown={(e) => onPointerDown("win", e)}
              onPointerMove={onPointerMove}
              onPointerUp={onPointerUp}
              onDoubleClick={toggleDock}
              title={docked ? "双击恢复浮窗" : "拖动移动 · 双击停靠到屏幕右侧"}
            >
              <span className={`wb-float__dot wb-float__dot--${phase}`} aria-hidden="true" />
              <span className="wb-float__title">{manifestName}</span>
              <div className="wb-float__actions">
                <button
                  type="button"
                  className="wb-float__btn"
                  // pointerdown 冒泡到标题栏会触发 setPointerCapture，浏览器把
                  // 后续 click 重定向给捕获元素，按钮永远收不到 —— 必须拦住。
                  onPointerDown={(e) => e.stopPropagation()}
                  onClick={() => { setMenuOpen(false); onNewSession(); }}
                  title="新会话（画布保留，开启一条新对话线）"
                  aria-label="新会话"
                >
                  <Plus className="icon-sm" />
                </button>
                <button
                  type="button"
                  className={`wb-float__btn ${menuOpen ? "is-open" : ""}`}
                  onPointerDown={(e) => e.stopPropagation()}
                  onClick={toggleHistory}
                  title="本工作台的历史会话"
                  aria-label="历史会话"
                >
                  <History className="icon-sm" />
                </button>
                <button
                  type="button"
                  className="wb-float__btn"
                  onPointerDown={(e) => e.stopPropagation()}
                  onClick={() => {
                    if (docked) {
                      setDocked(false);
                      try { localStorage.setItem(kDocked(manifestId), "false"); } catch { /* 存不进就算了 */ }
                    }
                    setOpen(false);
                  }}
                  title="收成球"
                  aria-label="收成球"
                >
                  <X className="icon-sm" />
                </button>
              </div>
            </div>
            {menuOpen && (
              <div className="wb-float__menu" role="menu" aria-label="本工作台的历史会话">
                {historyLoading && (
                  <div className="wb-float__menu-empty" aria-busy="true">
                    <SkeletonCard lines={2} />
                  </div>
                )}
                {!historyLoading && (history?.length ?? 0) === 0 && (
                  <div className="wb-float__menu-empty">还没有历史会话</div>
                )}
                {(history ?? []).map((sess) => (
                  <button
                    key={sess.id}
                    type="button"
                    role="menuitem"
                    className={`wb-float__menu-item ${sess.id === activeSessionId ? "is-current" : ""}`}
                    onPointerDown={(e) => e.stopPropagation()}
                    onClick={() => { setMenuOpen(false); onSelectSession(sess.id); }}
                  >
                    <span className="wb-float__menu-title">{sess.title || "新建对话"}</span>
                    <span className="wb-float__menu-time">{formatSidebarTime(sess.updated_at)}</span>
                  </button>
                ))}
              </div>
            )}
            <div className="wb-float__body">{chatNode}</div>
            {docked ? (
              <div
                className="wb-float__dockresize"
                onPointerDown={(e) => onPointerDown("dockresize", e)}
                onPointerMove={onPointerMove}
                onPointerUp={onPointerUp}
                aria-hidden="true"
              />
            ) : (
              <div
                className="wb-float__resize"
                onPointerDown={(e) => onPointerDown("resize", e)}
                onPointerMove={onPointerMove}
                onPointerUp={onPointerUp}
                aria-hidden="true"
              />
            )}
          </div>,
          document.body,
        )}
    </div>
  );
}
