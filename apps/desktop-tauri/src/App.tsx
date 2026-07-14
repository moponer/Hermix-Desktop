import { useState, useEffect, useCallback, useRef, useMemo } from "react";
import { getCurrentWindow, LogicalSize } from "@tauri-apps/api/window";
import { Sidebar } from "./components/Sidebar";
import { ChatView } from "./components/ChatView";
import { AutomationView } from "./components/AutomationView";
import { SettingsView } from "./components/SettingsView";
import { SearchView } from "./components/SearchView";
import { PluginsView } from "./components/PluginsView";
import { AgentsView } from "./components/AgentsView";
import { GroupChatView } from "./components/GroupChatView";
import { AnalyticsView } from "./components/AnalyticsView";
import { RightSidebar, PreviewPanel, type RightTab } from "./components/RightSidebar";
import { TitleBar } from "./components/TitleBar";
import { useStreamChat } from "./hooks/useStreamChat";
import type { StreamPhase, TodoSnapshotItem } from "./hooks/useStreamChat";
import { useResizable } from "./hooks/useResizable";
import { useWorkspaceRoot } from "./hooks/useWorkspaceRoot";
import { useArtifacts } from "./hooks/useArtifacts";
import { diagLog, nextDiagId } from "./lib/diag";
import { ErrorBoundary } from "./components/ErrorBoundary";
import { useAcuiState, type AcuiDecisionBus } from "./acui";
import type { Artifact } from "./utils/artifacts";
import * as api from "./api";
import { registerBuiltinsToBackend } from "./data/builtinExperts";
import type { NavPage, Session, ChatMessage, ApprovalDecision, TimelineStep, ExecutionTimelineEvent } from "./types";
// 2026-07-10: expandTimelineToMessages and detectToolError imports removed —
// tools are now tracked in the assistant's execution_timeline, not expanded
// into separate role:tool messages.
import { storage, STORAGE_KEYS, KNOWN_KEYS, sessionKey } from "./utils/storageCompat";

// 2026-07-12: Welcome-state draft workspace. Read at App-level so
// both `handleNewChat` (新建对话 button) and `handleSendMessage`
// (first send in an empty session) can pass it to
// `api.createSession(title, project)` — that way the server records
// it on `sessions.workspace` from the start, and a desktop restart
// restores the same Sidebar bucket instead of dumping the row into
// "未分组" (bug 2026-07-12).
function readDraftWorkspace(): string | null {
  try {
    return storage.getItem(STORAGE_KEYS.draftWorkspace);
  } catch {
    return null;
  }
}

const LEFT_KEY = STORAGE_KEYS.sidebarWidth;/**
 * Map known English backend error strings to Chinese for a better
 * desktop user experience. Falls through to English for anything
 * unrecognised.
 */
function localizeStreamError(err: string): string {
  // Normalize: strip redundant wrappers ("LLM API error:" repeated, etc.)
  const normalized = err
    .replace(/^LLM\s+API\s+error:\s*/gi, "")
    .replace(/^LLM\s+API\s+error:\s*/gi, "")
    .replace(/^API\s+error:\s*/gi, "")
    .trim();

  // Try to extract a human-readable message from JSON error bodies
  // e.g. {"error":{"message":"unknown model..."}} or {"type":"error","error":{...}}
  let extracted = normalized;
  try {
    const parsed = JSON.parse(normalized);
    const msg =
      parsed?.error?.message ??
      parsed?.error?.error?.message ??
      parsed?.message ??
      null;
    if (msg && typeof msg === "string") {
      extracted = msg;
    }
  } catch {
    // not JSON — keep normalized as-is
  }

  // No API key configured for provider
  if (
    /no\s+api\s+key/i.test(extracted) ||
    /api[\s_]*key[\s:]*(not\s+found|missing|empty|not\s+configured)/i.test(extracted)
  ) {
    return "LLM API 密钥未配置。请前往「设置 → API Keys」添加 API 密钥后重试。";
  }
  // LLM provider URL not configured
  if (
    /no\s+llm_provider_url/i.test(extracted) ||
    /provider.*url.*not\s+configured/i.test(extracted)
  ) {
    return "LLM 服务地址未配置。请检查 Settings → API Keys 中的服务地址配置。";
  }
  // Connection refused / unreachable
  if (/(refused|unreachable|connection\s+refused|ECONNREFUSED)/i.test(extracted)) {
    return "无法连接到后端服务，请确认 hermix 进程是否正常运行。";
  }
  // Timeout
  if (/(timeout|timed\s*out)/i.test(extracted)) {
    return "请求超时，请检查网络连接或稍后重试。";
  }
  // auth / token / 401 / 403
  if (/(401|403|unauthorized|forbidden|invalid\s+(api\s*)?key|auth\s+failed)/i.test(extracted)) {
    return "API 密钥认证失败，请检查 Settings → API Keys 中的密钥是否正确。";
  }
  // Rate limit
  if (/(rate\s*limit|429|too\s+many\s+requests)/i.test(extracted)) {
    return "请求过于频繁，请稍后重试。";
  }
  // 400 / bad request — model not found (common MiniMax error)
  if (/(400|bad\s+request|unknown\s+model|invalid\s+model|model.*not\s+found)/i.test(extracted)) {
    // Extract the model name if present
    const modelMatch = extracted.match(/model\s+['"]?([^\s'"]+)['"]?/i);
    const modelName = modelMatch ? modelMatch[1] : null;
    return modelName
      ? `模型「${modelName}」不可用，请在输入框左侧切换模型后重试。`
      : "模型不可用或参数错误，请检查模型配置后重试。";
  }
  // WebSocket / connection lost
  if (/WebSocket\s*error/i.test(extracted)) {
    return "连接异常（WebSocket 错误），请检查网络后重新发送消息。";
  }
  if (/Connection\s*closed/i.test(extracted)) {
    return "连接已断开，请重新发送消息。";
  }
  // Fallback: return the extracted message (already normalized)
  return extracted || err;
}
const RIGHT_KEY = STORAGE_KEYS.rightWidth;
const RIGHT_OPEN_KEY = STORAGE_KEYS.rightOpen;
const ACTIVE_SESSION_KEY = STORAGE_KEYS.activeSession;

const LEFT_MIN = 200;
const LEFT_MAX = 360;
// v7: 左列默认从 232 减到 200，把 32px 让给主列，让默认三列共存时主列更宽。
// vw=1440 时主列 = 1440 - 200 - 640 = 800 (v11: 右列默认 640 替代 480).
// PLAN_UI_ENHANCEMENT §2.1 (2026-06-22): 菜单栏加宽 200→280，让工作区/群聊名称更舒展。
// 已存在会话的 localStorage width 不会自动升级（用户拖动过的尊重保留），仅默认提高。
const LEFT_DEFAULT = 280;
// v11: 右列默认 640, 拖动范围 360-840.
// 主人反馈 480 装不下"节假日危险品重大危险源..."这样的中文长标题分类,
// 标题被截断成"节假日危险品重大危险源是级管控专题培训会...".
// 640 = 480 + 160, 足以显示完整中文文件名 + 扩展 chip, 不挤压主列.
const RIGHT_MIN = 360;
const RIGHT_MAX = 840;
const RIGHT_DEFAULT = 640;

// 对话主列的"舒适最小宽度"。当主列宽度 ≥ MAIN_MIN_WIDTH 时，chat 气泡的
// 内容（消息头像 + 文本 + 内边距）有充裕的呼吸感，气泡不显拥挤。
//
// v11: 主人反馈右列 480 装不下中文长文件名分类, 改 RIGHT_DEFAULT 480→640.
// 为避免 1440 视口三列总宽溢出 (200+720+640=1560>1440), 同步把
// MAIN_MIN_WIDTH 720→580. 主列 580 仍能装 50+ 中文字符, 平衡两端.
//
// 三列共存临界视口 (v11): LEFT_MIN(200) + MAIN_MIN_WIDTH(580) + RIGHT_MIN(360) = 1140.
// 默认视口 1440 时: 左 200 + 主 580 + 右 640 = 1420 (剩 20 给 splitter, OK).
// 默认视口 1680 时: 左 200 + 主 840 + 右 640 = 1680.
//
// v23: "想看右栏就自动给空间" — 视口 < 1140 + 用户点 TitleBar 产物按钮想开右栏
// 时, 把 Tauri 窗口 outer 宽扩到 DEFAULT_WINDOW_WIDTH (1280 = tauri.conf.json
// 默认), 高度保留当前 outer. 装下三列后 setRightOpen(true). 跟 v22 的
// "装不下就少一列" 不冲突: resize 拖小窗口仍然自动关, 用户**主动**想看才扩.
// appWindow setSize 失败时降级 (仍 setRightOpen(true), 让 layout 走 v22 退化路径).
// 默认视口 1920 时: 左 200 + 主 1080 + 右 640 = 1920.
/* v27: 自适应 MAIN_MIN_WIDTH — 不再硬编码 580px。
   在小视口下允许缩小到 360px（还能装约 30 个中文字符 + 气泡容差），
   大视口保持 580px 的舒适阅读宽度。
   阈值规则：
   - 视口 ≥ 1400px: MAIN_MIN_WIDTH = 580（舒适宽）
   - 视口 1000~1399px: MAIN_MIN_WIDTH = 480（中等）
   - 视口 800~999px: MAIN_MIN_WIDTH = 400（紧凑）
   - 视口 < 800px: MAIN_MIN_WIDTH = 360（最小可读）
   同时 LEFT_MIN 也从 200 适当降低, 避免在窄窗口下左列 + 主列
   合计最小宽度 > 视口。
   对应的三列共存阈值从 1140 降到 920. */
function computeMainMinWidth(vw: number): number {
  if (vw >= 1400) return 580;
  if (vw >= 1000) return 480;
  if (vw >= 800) return 400;
  return 360;
}
function computeLeftMin(vw: number): number {
  if (vw >= 1200) return 200;
  if (vw >= 900) return 160;
  return 120;
}

// v27: 分辨率自适应 — 不再用 const，改为 state + resize listener
// 在 App 组件内初始化，这样 setup 时窗口已经确定，准确拿到实际视口宽度。
// eslint-disable-next-line prefer-const
// (上面保留 computeMainMinWidth / computeLeftMin 供 typing 引用)

// ── Unified timeline step helpers ─────────────────────────────────
// These functions maintain the `unified_steps` array on the streaming
// assistant message. They track phase transitions (reasoning → tools →
// text) so that multi-iteration agent loops display each step in
// chronological order instead of a fixed 3-section layout.

/** Append reasoning text to the last reasoning step, or push a new one. */
function appendReasoningStep(steps: TimelineStep[] | undefined, content: string): TimelineStep[] {
  const arr = steps ? [...steps] : [];
  const last = arr[arr.length - 1];
  if (last && last.kind === "reasoning") {
    arr[arr.length - 1] = { kind: "reasoning", content: last.content + content };
  } else {
    arr.push({ kind: "reasoning", content });
  }
  return arr;
}

/** Append text to the last text step, or push a new one. */
function appendTextStep(steps: TimelineStep[] | undefined, content: string): TimelineStep[] {
  const arr = steps ? [...steps] : [];
  const last = arr[arr.length - 1];
  if (last && last.kind === "text") {
    arr[arr.length - 1] = { kind: "text", content: last.content + content };
  } else {
    arr.push({ kind: "text", content });
  }
  return arr;
}

/** Append a tool event to the last tools step, or push a new one. */
function appendToolStep(steps: TimelineStep[] | undefined, event: ExecutionTimelineEvent): TimelineStep[] {
  const arr = steps ? [...steps] : [];
  const last = arr[arr.length - 1];
  if (last && last.kind === "tools") {
    arr[arr.length - 1] = { kind: "tools", events: [...last.events, event] };
  } else {
    arr.push({ kind: "tools", events: [event] });
  }
  return arr;
}

/**
 * Update (or create) a `tool_stdout` event in the last tools step,
 * mirroring the same logic that `onToolChunk` applies to
 * `execution_timeline`. This keeps the unified_steps in sync so the
 * rendered tool cards show live stdout.
 */
function updateToolStepStdout(
  steps: TimelineStep[] | undefined,
  toolCallId: string,
  content: string,
): TimelineStep[] {
  const arr = steps ? [...steps] : [];
  const lastIdx = arr.length - 1;
  if (lastIdx < 0 || arr[lastIdx].kind !== "tools") return arr;
  const events = [...arr[lastIdx].events];
  let found = false;
  for (let i = events.length - 1; i >= 0; i--) {
    if (events[i].type === "tool_stdout" && events[i].tool_call_id === toolCallId) {
      events[i] = { ...events[i], content: (events[i].content ?? "") + content };
      found = true;
      break;
    }
  }
  if (!found) {
    events.push({
      type: "tool_stdout",
      tool_call_id: toolCallId,
      content,
      created_at: new Date().toISOString(),
    });
  }
  arr[lastIdx] = { kind: "tools", events };
  return arr;
}

export default function App() {
  const [page, setPageRaw] = useState<NavPage>("chat");
  // v27: 视口自适应面板最小宽度 — 窗口缩小时缩小主列/左列最小宽
  const [mainMinWidth, setMainMinWidth] = useState(() => computeMainMinWidth(window.innerWidth));
  const [leftMin, setLeftMin] = useState(() => computeLeftMin(window.innerWidth));
  // 包装 setPage: 切到非 chat 页时主动关闭产物栏 (右栏是 chat 专属 UI)
  // 2026-06-18 主人要求: 在右栏展开时切到 agents/experts/settings/plugins/
  // automation/search/groupchat 任何非对话页 → 自动 setRightOpen(false)
  const setPage = useCallback((p: NavPage) => {
    setPageRaw(p);
    if (p !== "chat") setRightOpen(false);
  }, []);
const [sessions, setSessions] = useState<Session[]>([]);
const [messageQueue, setMessageQueue] = useState<string[]>([]);
// P0-9: External file attachment from right-sidebar “加入对话” context menu.
const [pendingFileAttachment, setPendingFileAttachment] = useState<string | null>(null);
const handleAddFileToChat = useCallback((path: string) => {
setPendingFileAttachment(path);
// Ensure we're on the chat page so the user sees the attachment chip.
setPage("chat");
}, []);
  // Queue mutation handlers — kept memoized so ChatComposer doesn't
  // re-render on every keystroke in the main textarea.
  // 2026-06-28: added per-item remove / edit / send-now callbacks
  // alongside the pre-existing bulk `onClearQueue`, so the queue
  // panel can show its contents and let the user act on each row.
  //
  // NOTE: `sendFromQueue` closes over `streaming` and
  // `handleSendMessage`, both defined further down, so it's
  // declared after them (see below).
  const removeFromQueue = useCallback((idx: number) => {
    setMessageQueue((prev) => prev.filter((_, i) => i !== idx));
  }, []);
  const editQueueItem = useCallback((idx: number, newText: string) => {
    setMessageQueue((prev) => prev.map((m, i) => (i === idx ? newText : m)));
  }, []);
const [selectedRoomId, setSelectedRoomId] = useState<string | null>(null);
const isCreatingSessionRef = useRef(false);
const [isCreatingSession, setIsCreatingSession] = useState(false);

  // v27: 视口自适应 — 窗口尺寸变化时重算面板最小宽度
  // 对应 computeMainMinWidth / computeLeftMin 的分档逻辑。
  // 放在这里（useEffect 最前面），确保每次 resize 都更新。
  useEffect(() => {
    const onResize = () => {
      setMainMinWidth(computeMainMinWidth(window.innerWidth));
      setLeftMin(computeLeftMin(window.innerWidth));
    };
    onResize(); // 初始化执行一次
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, []);

  // v28: 窗口高度自适应屏幕 — Tauri 默认 860px 高，在 768p 屏幕（1366×768 笔记本）
  // 上窗口底部 92px 落到屏幕外。启动时检测窗口高度是否超出屏幕可用高度，
  // 超出则缩到刚好适配。注意：outerSize 返回的是物理像素，需要除以 scaleFactor
  // 换算成逻辑像素再跟 screen.availHeight 比较。
  //
  // 同时处理 Windows 任务栏/系统栏占用的情况（availHeight < screen.height）。
  // 不阻塞启动流程 — 失败了也不影响 app 正常运行。
  useEffect(() => {
    const fit = async () => {
      try {
        const outer = await appWindow.outerSize();
        const factor = await appWindow.scaleFactor();
        const logicalH = Math.max(1, Math.round(outer.height / factor));
        // 用 screen.availHeight 而不是 screen.height，避开任务栏/系统栏高度。
        // 额外留 40px 底边距让用户能托动窗口边缘。
        const maxH = window.screen.availHeight - 40;
        if (logicalH > maxH) {
          await appWindow.setSize(new LogicalSize(DEFAULT_WINDOW_WIDTH, maxH));
        }
      } catch {
        // best-effort: 启动流程不因窗口适配失败而阻塞
      }
    };
    fit();
  }, []);
  // One-shot: copy any `hermes.*` localStorage entries forward to
  // `hermix.*` so users on a pre-rename install keep their settings.
  // Idempotent: gated by `hermix.compat.migrated` flag inside the helper.
  useEffect(() => {
    // Iterate the canonical registry so newly added STORAGE_KEYS are
    // auto-migrated (P2-2). Draft keys are intentionally cleared below,
    // not migrated, to avoid cross-restart residue in the welcome UI.
    storage.migrateLegacyKeys(KNOWN_KEYS);

    // 主人反馈：重启后欢迎界面的「目录/专家」选择状态不应保留。
    // draft 槽位（hermix.desktop.draft.*）用于无 session 时的欢迎界面，
    // 持久化会导致上次的选择跨重启残留。每次启动时清除，让用户
    // 在新会话中从头选择。
    try { storage.removeItem(STORAGE_KEYS.draftExpertId); } catch {}
    try { storage.removeItem(STORAGE_KEYS.draftWorkspace); } catch {}

    // 2026-06-25 主人反馈"进历史对话，目录/模型/默认模型都消失"诊断。
    // 临时挂一个启动诊断：把所有 hermix.* key 打到 console，方便主人在
    // devtools 直接看 localStorage 实际状态 — 区分是"数据没写"还是
    // "数据写了没读"。**仅在 dev build 跑**（import.meta.env.DEV）：
    // 生产构建时 Vite 直接 dead-code-eliminate，不影响产物。
    // 可读 localStorage 失败时静默，避免控制台噪音。
    if (import.meta.env.DEV) {
      try {
        const dump: Record<string, string> = {};
        for (let i = 0; i < window.localStorage.length; i++) {
          const k = window.localStorage.key(i);
          if (k && k.startsWith("hermix.")) {
            const v = window.localStorage.getItem(k) ?? "";
            dump[k] = v.length > 200 ? v.slice(0, 200) + `…(${v.length} chars)` : v;
          }
        }
        console.info("[hermix-boot] localStorage dump:", dump);
        // Also expose a console helper so the user can re-dump at any time
        // after interacting with Settings (without a hard reload).
        (window as unknown as { __hermixDumpLs?: () => void }).__hermixDumpLs = () => {
          const out: Record<string, string> = {};
          for (let i = 0; i < window.localStorage.length; i++) {
            const k = window.localStorage.key(i);
            if (k && k.startsWith("hermix.")) {
              const v = window.localStorage.getItem(k) ?? "";
              out[k] = v.length > 200 ? v.slice(0, 200) + `…(${v.length} chars)` : v;
            }
          }
          console.info("[hermix-ls] dump:", out);
          return out;
        };
      } catch {}
    }
  }, []);

  const [activeSessionId, setActiveSessionIdRaw] = useState<string | null>(() => {
    try { return storage.getItem(ACTIVE_SESSION_KEY); } catch { return null; }
  });
  const setActiveSessionId = useCallback((id: string | null) => {
    setActiveSessionIdRaw(id);
    try { id ? storage.setItem(ACTIVE_SESSION_KEY, id) : storage.removeItem(ACTIVE_SESSION_KEY); } catch {}
  }, []);

  // v31 (2026-06-29): sessions 的最新值镜像到 ref，给异步回调（onDone 后的
  // 500ms re-fetch）一个同步读 sessions 的入口。setState 之后 useEffect
  // 链上的 closure 会拿到陈旧值，但 ref 永远跟最新值同步。
  const sessionsRef = useRef<typeof sessions>([]);
  useEffect(() => { sessionsRef.current = sessions; }, [sessions]);

  // Mirror activeSessionId to a ref so callbacks like handleNewChat can read
  // the current value without depending on it in their dep array. Without
  // this, handleNewChat would be recreated on every activeSessionId change,
  // which — combined with the sessions dep — caused the "flicker between
  // sessions on new chat" bug.
  const activeSessionIdRef = useRef(activeSessionId);
  useEffect(() => { activeSessionIdRef.current = activeSessionId; }, [activeSessionId]);

  const [sidebarOpen, setSidebarOpen] = useState(true);

  const [rightOpen, setRightOpen] = useState<boolean>(() => {
    try { return storage.getItem(RIGHT_OPEN_KEY) !== "false"; } catch { return false; }
  });
  useEffect(() => {
    try { storage.setItem(RIGHT_OPEN_KEY, String(rightOpen)); } catch {}
  }, [rightOpen]);

  // 2026-07-08 (user): 右栏当前标签，可被外部控制（点目录 chip 直接切到「电脑」）。
  const [rightTab, setRightTab] = useState<RightTab>("products");

  // v23: 三列临界视口 + 默认窗口宽. 见上方注释块.
  const THREE_COL_MIN_VIEWPORT = 1140;
  const DEFAULT_WINDOW_WIDTH = 1280;
  const appWindow = getCurrentWindow();

  // v24: 切换右栏时, 视口 < 1140 + 用户在"开"动作上 → 先 await 扩窗到能装三列,
  // 扩窗完成后再 setRightOpen(true). resize 路径仍然走 v22 (装不下关右栏).
  //
  // v23 死循环修复: 之前是 await setSize + 同步 setRightOpen((v)=>!v), 但 setSize
  // 异步完成期间 window.innerWidth 仍是 < 1140, line 415 的 viewport-aware effect
  // 看到 rightOpen=true + vw<1140 立刻 setRightOpen(false), 用户视觉上"点一下抖
  // 一下". 现在 await setSize 完成后再开, 那时 vw 已经是 ≥1140, effect 不会再掐.
  //
  // setSize 失败处理: 不再降级到 v22 退化路径 (v22 退化 = setRightOpen(true) 后
  // 立刻被 effect 掐 = 还是抖). setSize 失败就保持 rightOpen 不变 + console.warn.
  // 用户能再点一次, 或先手动拉宽窗口.
  const handleToggleRight = useCallback(async () => {
    if (rightOpen) {
      // 关闭动作 — 任何视口下都直接关, 跟 v22 一致.
      setRightOpen(false);
      return;
    }
    // 打开动作 — 视口不足时先扩窗.
    if (window.innerWidth < THREE_COL_MIN_VIEWPORT) {
      try {
        const outer = await appWindow.outerSize();
        const factor = await appWindow.scaleFactor();
        // outerSize / scaleFactor → logical px (setSize 用 logical).
        const logicalH = Math.max(1, Math.round(outer.height / factor));
        await appWindow.setSize(new LogicalSize(DEFAULT_WINDOW_WIDTH, logicalH));
      } catch (e) {
        // setSize 失败 — 保持 rightOpen 不动, 不开右栏. 避免 v23 那种"开了又
        // 立刻被掐"的抖动. 用户可以手动拉宽窗口再点.
        console.warn("[right] auto-expand window failed, right sidebar not opened:", e);
        return;
      }
    }
    setRightOpen(true);
  }, [rightOpen]);

  // 2026-06-25 主人反馈："欢迎页默认是关闭产物栏的"。
  // 当 ChatView 从有消息 / 有 session 切到 empty state（欢迎页）时，
  // 由 ChatView 通过 onEnterWelcome 回调通知此 handler — 此处只做
  // 幂等的关闭动作。ChatView 内部用 lastIsWelcomeRef 保证只在
  // false→true 切换时触发，所以这里即使 setRightOpen(false) 也是
  // 一次性的，不会把用户在欢迎页手动开的右栏反复掐掉。
  const handleEnterWelcome = useCallback(() => {
    setRightOpen(false);
  }, []);

  // 左列 / 右列：viewport-aware 夹紧在下面那个 useEffect 里集中做（每次
  // window.resize、rightOpen 切换、右列宽度变化时重算），不再给
  // useResizable 传 dynamicMax — 那样做会让两个 useResizable 互为依赖，
  // 一旦 deps 抖动就会触发 React hook-order 报错（详见上一次提交后控制台
  // 里的 "change in the order of Hooks" 报告）。useResizable 现在只负责
  // 自己那列的 min/max 静态边界，动态夹紧走这里的单一 effect，单向数据
  // 流，没有循环依赖。
  const left = useResizable({
    storageKey: LEFT_KEY,
    defaultWidth: LEFT_DEFAULT,
    min: LEFT_MIN,
    max: LEFT_MAX,
    side: "left",
  });
  const right = useResizable({
    storageKey: RIGHT_KEY,
    defaultWidth: RIGHT_DEFAULT,
    min: RIGHT_MIN,
    max: RIGHT_MAX,
    side: "right",
  });
  const { streaming, streamingSessionId, streamingStatus, compactionStatus, contextUsage, usageSnapshot, send: sendStream, cancel: cancelStream } = useStreamChat();

  // Per-send phase tracking. Set by the `onPhaseChange` callback in
  // `handleSendMessage` and scoped to the current `sessionId` so the
  // ChatView only shows the phase chip for the active session.
  const [streamPhase, setStreamPhase] = useState<StreamPhase | null>(null);
  const prevPhase = useRef<StreamPhase | null>(null);

  // M5 ACUI: the `acui` hook + `acuiBus` useMemo are declared further
  // down (after handleApproveApproval / handleDenyApproval /
  // handleAnswerClarify) to keep the temporal dead zone happy. We
  // reference them inside `handleSendMessage` via closure.

  // Per-session streaming flag. The hook's `streaming` is just
  // `streamingSessionId !== null` for backward-compat callers, but the
  // ChatView needs the *scoped* answer: is *this* session the one
  // currently streaming? Without this, switching to another session
  // mid-stream would leak the "正在思考…" indicator into the new
  // session's view (the new session isn't actually streaming).
  const isStreamingActiveSession =
    streamingSessionId !== null && streamingSessionId === activeSessionId;

  /**
   * Most recent stream error, scoped to the active session.  Set when
   * the WebSocket closes before any chunk landed on the optimistic
   * assistant placeholder (typical when the backend was unreachable
   * or killed mid-handshake).  Cleared by ChatView after rendering
   * for ~6s, or whenever a new send starts.  The ChatView uses this
   * to surface a synthetic "⚠️ …" row at the tail of the message
   * list so the user sees something happen — without this the empty
   * optimistic row gets filtered out by the placeholder filter and
   * the chat just sits there silent (which is what the previous
   * version did).
   */
  const [streamError, setStreamError] = useState<string | null>(null);
  const clearStreamError = useCallback(() => setStreamError(null), []);

  /**
   * Per-session message-load errors. Cleared on success, set on any
   * fetch rejection. Lives only in memory — these are transient network
   * states, not user data. The `ChatView` reads the active session's
   * entry to decide between the empty-state and an inline error card
   * with a "重试" button.
   */
  const [messageLoadErrors, setMessageLoadErrors] = useState<Map<string, string>>(
    () => new Map(),
  );
  const setLoadError = useCallback((sessionId: string, message: string) => {
    setMessageLoadErrors((prev) => {
      const next = new Map(prev);
      next.set(sessionId, message);
      return next;
    });
  }, []);
  const clearLoadError = useCallback((sessionId: string) => {
    setMessageLoadErrors((prev) => {
      if (!prev.has(sessionId)) return prev;
      const next = new Map(prev);
      next.delete(sessionId);
      return next;
    });
  }, []);
  /**
   * Mutable ref to the *currently streaming* assistant message id.
   * Re-assigned whenever the agent pauses for an approval request —
   * the previous id is "frozen" (no more chunks land on it) and a
   * new empty assistant message takes over. This lets the inline
   * approval card sit at the exact slot the agent emitted it, with
   * the post-approval response continuing as a fresh message
   * immediately below.
   */
  const currentAssistantIdRef = useRef<string | null>(null);
  /**
   * The latest prefetch intent info from the WS stream. Set by
   * `onPrefetch` so ChatView can render a `PrefetchIntentChip`
   * below the streaming message.
   */
  const [prefetchIntent, setPrefetchIntent] = useState<{
    intent: string;
    confidence: number;
    results: import("./hooks/useStreamChat").PrefetchResultEntry[];
  } | null>(null);
  /**
   * Pending decision resolvers, keyed by approval request id. Populated
   * the moment the server emits `RequestApproval`; settled when the
   * user clicks allow/deny on the inline card. The settlement value is
   * the WebSocket payload that the stream layer echoes back via
   * `approval_response`.
   */
  const approvalResolversRef = useRef<
    Map<string, (decision: ApprovalDecision) => void>
  >(new Map());
  /**
   * Tracks running tool messages by their stable WS `tool_call_id`
   * so that `onToolChunk` / `onToolComplete` can find the right inline
   * card (by message `id`) even when multiple tools run concurrently.
   */
  const runningToolRef = useRef<Map<string, string>>(new Map());
  /**
   * Tracks the current streaming "phase" (reasoning / tools / text) so
   * we can detect phase transitions and push new unified timeline steps
   * at the right time. Reset to null at the start of each stream.
   */
  const streamPhaseRef = useRef<"reasoning" | "tools" | "text" | null>(null);
  // v2 (2026-07-07): one TodoListCard per plan / group. State
  // (not ref) so ChatView re-renders when a new plan arrives.
  // Ordered: groups are stored in the order their group_started
  // event arrived. Items are keyed by id so upsert is a single
  // object property write.
  interface TodoGroup {
    groupId: string;
    groupTitle: string | null;
    anchorMessageId: string | null;
    itemsById: Record<string, TodoSnapshotItem>;
  }
  const [todoGroups, setTodoGroups] = useState<TodoGroup[]>([]);
  const todoGroupsByIdRef = useRef<Map<string, TodoGroup>>(new Map());
  /**
   * Shared mutable set between App.tsx and every TodoListCard. Card
   * writes the item id before an optimistic toggle-patch and removes
   * it when the patch settles (including on error — see
   * TodoListCard.tsx catch block).  The onTodoChange handler below
   * skips WS echo upserts for ids that are currently in this set,
   * preventing rapid-toggle races (Bug #1 fix, 2026-07-08).
   */
  const justPatchedTodoRef = useRef<Set<string>>(new Set());
// 2026-07-10: needsNewAssistantRef removed. All text chunks in an
// agent loop now go to the SAME assistant message. Tool calls are
// tracked in the assistant's `execution_timeline` (rendered as a
// collapsible block inside the bubble) instead of separate
// `role: "tool"` messages. This makes streaming and reload
// paths produce identical visuals:
//   [avatar] [text] [collapsible tools] [more text]
// instead of the old split where tools appeared above the avatar
// because empty assistant placeholders were filtered out.
  /**
   * Pending `clarify` callbacks — keyed by `request_id`. When the
   * server pushes a `ClarifyRequest` event we register a resolver
   * here, the user picks an option (or types a free-form answer) in
   * the inline card, and `handleAnswerClarify` resolves the promise.
   * The stream layer then sends the answer back as a
   * `clarify_response` WS frame.
   */
  const clarifyResolversRef = useRef<
    Map<string, (answer: string) => void>
  >(new Map());
  const { workspaceRoot, setWorkspaceRoot } = useWorkspaceRoot();

  // 2026-07-07 主人反馈："选了工作区 Computer 面板不显示文件夹 / 对话没归到对应工作区分组"。
  //   根因：Composer chip 走 useSessionWorkspace（per-session localStorage + 后端 PATCH），
  //   但 ComputerTab 读的是 useWorkspaceRoot（global），Sidebar 的 useMemo 只依赖
  //   sessions/workspaces state —— 两边都不感知 chip 的修改。本回调把变更广播到两个
  //   出口：(1) setWorkspaceRoot 推 global，(2) setSessions 改 session.workspace
  //   让 Sidebar useMemo 重算。
  //   当 path=null（用户点"不绑定目录"）时也清掉 global workspaceRoot，避免下次开
  //   新 session 时还残留上次的根目录。
  const onWorkspaceChange = useCallback((sessionId: string | null | undefined, path: string | null) => {
    if (path) {
      setWorkspaceRoot(path);
    } else {
      setWorkspaceRoot(null);
    }
    if (sessionId) {
      setSessions((prev) => prev.map((s) => s.id === sessionId ? { ...s, workspace: path ?? undefined } : s));
    }
  }, [setWorkspaceRoot]);

  // 2026-07-08 (user): 点目录 chip（已选工作区时）→ 打开右栏并切到「电脑」标签，
  //   直接把文件浏览器切到所选工作区目录，不再弹 chooser 列表。
  const openComputer = useCallback(() => {
    setRightTab("computer");
    setRightOpen(true);
  }, []);

  // Auto-collapse sidebar on small screens
  useEffect(() => {
    const mq = window.matchMedia("(max-width: 768px)");
    const handler = (e: MediaQueryListEvent | MediaQueryList) => setSidebarOpen(!e.matches);
    handler(mq);
    mq.addEventListener("change", handler);
    return () => mq.removeEventListener("change", handler);
  }, []);

  // P1-T4 (Frontend Audit 2026-07-13): clear `todoGroups` when the
  // user switches sessions. Without this, todos from the previous
  // session leak into the new session's chat (the WS listener
  // already filters per-session, but stale state from a same-tab
  // session switch would persist). The new session is re-hydrated
  // by the next `todo_snapshot` WS event (P0-T2).
  //
  // Note: this only triggers when `activeSessionId` actually
  // changes, not on every render. The `todoGroupsByIdRef` mirror is
  // cleared in the same pass so the dedup set never sees ghosts.
  const prevActiveSessionIdRef = useRef<string | null>(null);
  useEffect(() => {
    if (prevActiveSessionIdRef.current === activeSessionId) return;
    prevActiveSessionIdRef.current = activeSessionId;
    todoGroupsByIdRef.current.clear();
    setTodoGroups([]);
  }, [activeSessionId]);

  // Viewport-aware layout: 三列共存阈值从 state 读取, 不再硬编码 1140.
  //
  // 主人反馈 v20: "拖动边框缩小时右栏换行/弹遮罩". 改用最简方案 —
  // 视口装不下三列就关掉右栏, 不挤不变形, 也不弹 modal 遮罩 (跟 workbuddy
  // "三列就三列, 装不下就少一列" 的行为一致). 拉宽后不会自动重开 —
  // 用户需要手动点 TitleBar 上的右栏按钮.
  //
  // v27: 阈值从 state (leftMin + mainMinWidth + RIGHT_MIN) 实时计算,
  // 不再硬编码 1140. 窗口缩小时主列和左列最小宽度都会缩, 临界阈值随之降低,
  // 右栏可以在更小的窗口下保持打开.
  //
  // recompute 触发条件: window.resize / rightOpen 切换 / 用户拖动 resizer.
  // 三列共存时再 clamp 各自列到静态边界 (LEFT_MAX / RIGHT_MIN..RIGHT_MAX).
  const leftWidthRef = useRef(left.width);
  const rightWidthRef = useRef(right.width);
  useEffect(() => { leftWidthRef.current = left.width; }, [left.width]);
  useEffect(() => { rightWidthRef.current = right.width; }, [right.width]);

  useEffect(() => {
    const recompute = () => {
      const vw = window.innerWidth;
      // 三列共存下限 = state 中的 leftMin + mainMinWidth + RIGHT_MIN(360).
      // v27 改为动态计算: 视口缩小时 leftMin 和 mainMinWidth 都缩,
      // 临界阈值随之降低, 右栏可以在更小的窗口下保持打开.
      const minThreeCol = leftMin + mainMinWidth + RIGHT_MIN;
      if (vw < minThreeCol && rightOpen) {
        setRightOpen(false);
        return;
      }
      // 三列共存: clamp 各自列到静态边界 (LEFT_MAX / RIGHT_MIN..RIGHT_MAX).
      // grid minmax 在 CSS 端兜底物理下限.
      if (rightOpen && rightWidthRef.current < RIGHT_MIN) {
        right.setWidth(RIGHT_MIN);
      }
      if (leftWidthRef.current > LEFT_MAX) {
        left.setWidth(LEFT_MAX);
      }
    };
    recompute();
    window.addEventListener("resize", recompute);
    return () => window.removeEventListener("resize", recompute);
  }, [rightOpen, left.setWidth, right.setWidth, leftMin, mainMinWidth]);

  // Load sessions
  useEffect(() => { api.getSessions().then(setSessions).catch((e) => console.error("getSessions failed", e)); }, []);

  // Register built-in experts to backend ExpertRegistry on app start.
  // Best-effort: the UI fetches the catalog on first mount and falls back
  // to BUILTIN_EXPERTS if the backend is down, so a failure here is non-fatal.
  useEffect(() => { registerBuiltinsToBackend().catch(() => {}); }, []);

  // Load messages for active session.
  //
  // Why no `loadedSessionMessagesRef` here: a previous version added the
  // sessionId to a Set *before* dispatching the fetch, then `.catch(() => {})`
  // swallowed the error. If the first fetch failed (typical during a backend
  // restart where the desktop is up before the HTTP server is listening),
  // the Set entry would persist across every subsequent re-render and the
  // effect would silently never retry — user would see a permanently empty
  // session. The fix: only check `session.messages.length > 0` and let
  // transient failures re-attempt on the next `sessions` reference change
  // (which happens whenever the user does *anything* that triggers a
  // `setSessions`, including switching to another session and back). The
  // `cancelled` flag in the cleanup function prevents a stale response from
  // a previous mount of this effect from clobbering fresher data.
  useEffect(() => {
    if (!activeSessionId) return;
    // 2026-07-10: use sessionsRef to avoid re-running this effect on
    // every sessions change (e.g. during streaming). The previous deps
    // included `sessions`, which meant every streaming chunk re-triggered
    // this effect. Although the `messages.length > 0` guard prevented
    // re-fetching, the effect cleanup (`cancelled = true`) would fire
    // on every chunk, potentially cancelling an in-flight fetch from
    // a different session that had just been switched to.
    //
    // 2026-07-11: Added `sessions.length` to deps to fix a startup race
    // condition. On app launch, `activeSessionId` is restored from
    // localStorage BEFORE `getSessions()` completes. The effect fires
    // immediately, but `sessionsRef.current` is empty, so it returns
    // without loading messages. When `getSessions()` completes later,
    // `sessions.length` changes from 0 to N, which re-triggers this
    // effect and loads the messages for the active session.
    //
    // During streaming, `sessions.length` doesn't change (only the
    // messages within existing sessions change), so this effect doesn't
    // re-fire on every chunk — preserving the 2026-07-10 fix.
    if (sessions.length === 0) return;
    const session = sessionsRef.current.find((s) => s.id === activeSessionId);
    if (!session) { if (sessionsRef.current.length > 0) setActiveSessionId(null); return; }
    if (session.messages.length > 0) return;

    let cancelled = false;
    api.getSessionMessages(activeSessionId)
      .then((rawMessages) => {
        if (cancelled) return;
        clearLoadError(activeSessionId);
        const messages = rawMessages;
        setSessions((prev) => prev.map((s) =>
          s.id === activeSessionId ? { ...s, messages } : s
        ));
      })
      .catch((e) => {
        if (cancelled) return;
        const msg = e instanceof Error ? e.message : String(e);
        console.error(`[chat] getSessionMessages(${activeSessionId}) failed:`, msg);
        setLoadError(activeSessionId, msg);
      });
    return () => { cancelled = true; };
  }, [activeSessionId, sessions.length, setActiveSessionId, setLoadError, clearLoadError]);

  const activeSession = sessions.find((s) => s.id === activeSessionId) ?? null;

  const { rawArtifacts, artifacts, selectedArtifactId, setSelectedArtifactId, handlePathClick } = useArtifacts({
    activeSession,
    workspaceRoot,
    onOpenRightSidebar: () => setRightOpen(true),
  });

  /**
   * Number of tool messages whose `status === "running"` in the active
   * session. We surface this to `ChatView` so the post-stream
   * activity indicator can show "执行工具中…" when the LLM stream
   * settles but tool calls are still in flight (the user otherwise
   * sees a blank gap between "已完成" and the next turn's first
   * chunk, which reads as "Hermix is stuck"). Memoized off the
   * session reference so we don't re-walk messages on every render.
   */
  const pendingToolsCount = useMemo(() => {
    if (!activeSession) return 0;
    let n = 0;
    for (const m of activeSession.messages) {
      if (m.role === "tool" && m.tool?.status === "running") n++;
    }
    return n;
  }, [activeSession]);

  /**
   * Auto-generate a session title from the first user message, unless the
   * existing title is already a meaningful one. Mirrors the previous
   * in-place logic; the only change is that we now expose it as a helper
   * so the persistence call below can reuse the same value.
   */
  const computeAutoTitle = (s: Session, firstUserContent: string): string => {
    const isPlaceholder =
      !s.title || s.title === "新建对话" || s.title === "新对话" || s.title === "New chat";
    return isPlaceholder || s.messages.length === 0
      ? firstUserContent.replace(/\s+/g, " ").trim().slice(0, 28) +
        (firstUserContent.length > 28 ? "…" : "")
      : s.title;
  };

const handleNewChat = useCallback(async () => {
if (isCreatingSessionRef.current) return;
// 2026-07-14: Do NOT cancel the active stream when creating a new chat.
// The stream on the previous session should continue in the background.
try { clearStreamError(); } catch (_) { /* swallow */ }

    // Read sessions from ref instead of closure — this callback must NOT
    // depend on `sessions` in its dep array. If it does, every streaming
    // chunk (which calls setSessions) recreates this callback, and the
    // "find an existing empty session" logic below can pick a DIFFERENT
    // empty session on each recreation, causing the UI to flicker between
    // sessions instead of landing on the new one.
    const currentSessions = sessionsRef.current;
    const currentActiveId = activeSessionIdRef.current;

    // Find an existing session with no messages (empty session),
    // but skip the currently active session — the user clicked
    // "新建对话" to get a *new* blank slate, not to re-select
    // the one they're already looking at.
    const emptySession = currentSessions.find(
      (s) => s.id !== currentActiveId && (s.message_count ?? s.messages.length) === 0,
    );

    // (2026-07-12) `readDraftWorkspace` is defined at module scope so
    // `handleSendMessage` can also send the welcome draft to the
    // server on creation — see the hook below.
    // Helper: migrate the welcome-screen draft workspace into the session so
    // the sidebar groups it under the correct workspace instead of "未分组".
    const applyDraftWorkspace = (sessionId: string): string | null => {
      try {
        const draftWorkspace = storage.getItem(STORAGE_KEYS.draftWorkspace);
        if (draftWorkspace) {
          storage.setItem(sessionKey(sessionId, "workspace"), draftWorkspace);
        }
        return draftWorkspace;
      } catch {
        return null;
      }
    };

    if (emptySession) {
      const ws = applyDraftWorkspace(emptySession.id);
      if (ws) {
        setSessions((prev) =>
          prev.map((s) =>
            s.id === emptySession.id ? { ...s, workspace: ws } : s
          )
        );
      }
      setActiveSessionId(emptySession.id);
      setPage("chat");
      return;
    }

    isCreatingSessionRef.current = true;
    setIsCreatingSession(true);
    try {
      // 2026-07-12: send the welcome-state draft workspace to the
      // server on creation so it lands in `sessions.workspace`
      // (not just localStorage). Without this, the desktop sidebar
      // bucketed every restarted session under "未分组" — the
      // draft only ever lived in `hermix.session.<id>.workspace`
      // localStorage, which the server doesn't see.
      const draftWs = readDraftWorkspace();
      const session = await api.createSession("新建对话", draftWs ?? undefined);
      const ws = applyDraftWorkspace(session.id);
      setSessions((prev) => [ws ? { ...session, workspace: ws } : session, ...prev]);
      setActiveSessionId(session.id);
      setPage("chat");
    } finally {
      isCreatingSessionRef.current = false;
      setIsCreatingSession(false);
    }
  }, [clearStreamError, setActiveSessionId]);

  const handleSelectSession = useCallback(async (id: string) => {
    // 2026-07-14: Do NOT cancel the active stream when switching sessions.
    // The previous implementation called `cancelStream()` which closed the
    // WebSocket, killing any in-flight agent task on the old session.
    // Now the stream continues in the background — useStreamChat already
    // tracks streamingSessionId so the UI correctly scopes the thinking
    // indicator and phase chip to whichever session is actually streaming.
    // The Sidebar pulse dot also uses streamingSessionId so the user can
    // see that the old session is still working even after switching away.
    try { clearStreamError(); } catch (_) { /* swallow */ }
    // Clear the phase chip — it belongs to the old session. If the
    // active stream is on the new session, onPhaseChange will
    // re-set it immediately; otherwise we dont want a stale chip.
    setStreamPhase(null);
    setActiveSessionId(id);
    setPage("chat");
    if (window.innerWidth < 768) setSidebarOpen(false);

    // The activeSessionId effect will fetch messages from the server
    // if the session doesn't have them in memory yet. We no longer
    // duplicate the fetch here — the effect is the single source of
    // truth for loading session messages, which avoids race conditions
    // between two concurrent fetches overwriting each other's results.
    //
    // v29 preserved: if the session already has in-memory messages
    // (e.g. from a just-ended streaming conversation), the effect
    // skips the fetch and uses the in-memory state directly.
  }, [clearStreamError, setActiveSessionId, setPage]);

  const handleDeleteSession = useCallback(async (id: string) => {
    // 2026-06-26: the previous implementation called `api.deleteSession`
    // with no `try/catch`, so a 5xx from the backend (e.g. the
    // `FOREIGN KEY constraint failed` we hit on sessions with
    // `compression_archives` rows) would propagate as an unhandled
    // rejection. React's unhandled-rejection handler is silent in our
    // Electron/Tauri build, so the user saw the click "do nothing"
    // and the session stayed in the Sidebar forever. Now: log to
    // console + surface a dialog so the user actually knows.
    try {
      await api.deleteSession(id);
      setSessions((prev) => prev.filter((s) => s.id !== id));
      if (activeSessionId === id) setActiveSessionId(null);
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      console.error("[deleteSession] failed for", id, err);
      window.alert(`删除会话失败：${msg}`);
    }
  }, [activeSessionId]);

  /**
   * Settle a pending approval card. Called by the inline card's
   * "允许" / "拒绝" buttons. Three things happen in sequence:
   *   1. resolve the promise the stream layer is awaiting (this is
   *      what unblocks the agent's `await on_pre_tool_dispatch`)
   *   2. mutate the approval message in place so the card transitions
   *      to its "已允许" / "已拒绝" terminal state immediately
   *   3. the stream layer will then send `approval_response` over the
   *      WebSocket; the server's `resolve_approval_message` updates
   *      the persisted row for replay across reloads
   *
   * If no resolver is registered — i.e. the user clicked
   * after the stream already settled (the 5-minute
   * `DEFAULT_APPROVAL_TIMEOUT` fired, the WebSocket closed,
   * or a duplicate click deleted the entry) — we do NOT
   * flip the card to `approved` / `denied`. The backend
   * already auto-denied via the timeout, and showing
   * "已允许" on a card whose decision the agent never
   * received is the exact race the owner reported on
   * 2026-06-14 ("I clicked allow, the card said 已允许, but
   * the LLM said denied"). Instead we surface a distinct
   * `expired` state so the user can see that their click
   * was a no-op and re-issue the prompt if they still want
   * the action to run.
   */
  const resolveApproval = useCallback(
    (
      requestId: string,
      decision: Exclude<ApprovalDecision, null>,
    ): ApprovalDecision | null => {
      const resolver = approvalResolversRef.current.get(requestId);
      if (resolver) {
        resolver(decision);
        approvalResolversRef.current.delete(requestId);
        setSessions((prev) =>
          prev.map((s) => ({
            ...s,
            messages: s.messages.map((m) => {
              if (m.role !== "approval" || m.approval?.requestId !== requestId) return m;
              return {
                ...m,
                approval: {
                  ...(m.approval ?? {
                    requestId,
                    tool: "tool",
                    riskLevel: "caution",
                  }),
                  decision,
                  decidedAt: new Date().toISOString(),
                },
              };
            }),
          })),
        );
        return decision;
      }
      // Stale click — the resolver is already gone. The
      // backend has already auto-denied via the 5-minute
      // timeout. Do NOT flip the card to `approved` /
      // `denied`; mark it `expired` so the user can see
      // their click did not register, and can re-prompt.
      // Skip the downgrade if the card already has a real
      // decision from a previous live click.
      setSessions((prev) =>
        prev.map((s) => ({
          ...s,
          messages: s.messages.map((m) => {
            if (m.role !== "approval" || m.approval?.requestId !== requestId) return m;
            if (m.approval?.decision) return m;
            return {
              ...m,
              approval: {
                ...(m.approval ?? {
                  requestId,
                  tool: "tool",
                  riskLevel: "caution",
                }),
                decision: "expired" as ApprovalDecision,
                decidedAt: new Date().toISOString(),
              },
            };
          }),
        })),
      );
      return null;
    },
    [],
  );

  const handleApproveApproval = useCallback(
    (requestId: string) => {
      resolveApproval(requestId, "approved");
    },
    [resolveApproval],
  );

  const handleDenyApproval = useCallback(
    (requestId: string) => {
      resolveApproval(requestId, "denied");
    },
    [resolveApproval],
  );

  /**
   * User answered a pending clarify card. Resolves the matching
   * `clarify_response` promise so the stream layer can send the
   * answer back over WebSocket, and stamps the card locally so the
   * resolved footer takes over without a server round-trip wait.
   */
  const handleAnswerClarify = useCallback((requestId: string, answer: string) => {
    const resolver = clarifyResolversRef.current.get(requestId);
    if (resolver) {
      resolver(answer);
      clarifyResolversRef.current.delete(requestId);
    }
    setSessions((prev) =>
      prev.map((s) => ({
        ...s,
        messages: s.messages.map((m) => {
          if (m.role !== "clarify" || m.clarify?.requestId !== requestId) return m;
          return {
            ...m,
            clarify: {
              ...(m.clarify ?? {
                requestId,
                question: m.content,
                choices: [],
              }),
              answer,
            },
          };
        }),
      })),
    );
  }, []);

  // ── M5: ACUI directive state + decision bus ─────────────────────
  //
  // The agent emits ACUI (Agent-Controlled UI) directives over the
  // streaming WebSocket (`{ type: "acui", directive: {...} }`). We
  // collect them into `acui.state`, then `ChatView` renders the active
  // cards via `<AcuiPanel state={acui.state} decide={acuiBus} />`.
  //
  // The decision bus bridges approval/clarify card clicks back into
  // the same code path as inline cards: when an ACUI approval card is
  // clicked, `bus.approve(requestId)` calls `handleApproveApproval`,
  // which resolves the WS `RequestApproval` promise — the stream
  // layer then sends `approval_response` to the server.
  const acui = useAcuiState();
  const acuiBus = useMemo<AcuiDecisionBus>(
    () =>
      acui.bind({
        approve: (id) => handleApproveApproval(id),
        deny: (id) => handleDenyApproval(id),
        answer: (id, a) => handleAnswerClarify(id, a),
      }),
    // All three handlers are stable useCallbacks, so this useMemo
    // runs once. We list them for correctness in case a future
    // refactor swaps one for a non-memoized closure.
    [acui, handleApproveApproval, handleDenyApproval, handleAnswerClarify],
  );

  const handleSendMessage = useCallback(async (content: string, _projectId?: string, overrides?: import("./hooks/useStreamChat").ChatOverrides) => {
    // ★ DIAG: trace every send_message_stream call to find the trigger chain
    // Global counter survives remounts/HMR; shows whether handleSendMessage
    // itself was called 1x or 10x. Pair with the timestamp so we can
    // distinguish "10 calls in 50ms" (retry storm) from "10 calls in 60s"
    // (user actually pressed Enter 10x).
    const __diagId = nextDiagId();
    const __diagTs = performance.now().toFixed(1);
    diagLog(`[diag #${__diagId} @${__diagTs}ms] handleSendMessage ENTER`, { content, streamingSessionId, queueLen: messageQueue.length });
    // Note: console.trace removed (was just stack; the 1-line call above is enough).
    // Clear any previous stream error — the user is starting a new turn,
    // so the old error is stale.  If this turn also fails the error
    // will be re-set by the onError callback.
    setStreamError(null);
    let sessionId = activeSessionId;
    if (sessionId && !sessions.some((s) => s.id === sessionId)) { sessionId = null; setActiveSessionId(null); }
    if (!sessionId) {
      // createSession() calls the HTTP server; if the sidecar isn't
      // running (or hermix.exe failed to spawn — see sidecar.rs
      // path resolution) it throws. Wrap it so the user sees a
      // visible error instead of a click that "did nothing".
      const title = content.slice(0, 40) + (content.length > 40 ? "..." : "");
      let session;
      try {
        // 2026-07-12: pass the welcome-state draft workspace so the
        // server records it on the session row from creation
        // (instead of relying on a localStorage-only fallback that
        // vanishes on desktop restart).
        const draftWs = readDraftWorkspace();
        session = await api.createSession(title, draftWs ?? undefined);
      } catch (e) {
        const msg = e instanceof Error ? e.message : String(e);
        setStreamError(
          `后端服务未启动或不可达 (${msg})。` +
          `请检查 hermix.exe 是否在 target/debug/ 下、端口 3000 是否被占用。`,
        );
        return;
      }
      setSessions((prev) => [session, ...prev]);
      setActiveSessionId(session.id);
      sessionId = session.id;

      // Migrate welcome-state draft selections (expert / workspace)
      // into the new session's per-session storage. Without this, the
      // moment ChatView re-renders with the real sessionId, its
      // `useSessionExpert` / `useSessionWorkspace` hooks would read
      // from an empty `hermix.session.<id>.*` slot and the chip text
      // would flip from "专家：xxx" / "目录：xxx" back to the bare
      // placeholders — exactly the regression the user reported.
      try {
        const draftExpert = storage.getItem(STORAGE_KEYS.draftExpertId);
        if (draftExpert) {
          storage.setItem(sessionKey(session.id, "expertId"), draftExpert);
        }
      } catch {}
      try {
        const draftWorkspace = storage.getItem(STORAGE_KEYS.draftWorkspace);
        if (draftWorkspace) {
          storage.setItem(sessionKey(session.id, "workspace"), draftWorkspace);
        }
      } catch {}
    }

// 2026-07-10: Reset per-turn state.
runningToolRef.current.clear();
streamPhaseRef.current = null;

    const userMsg: ChatMessage = { id: `temp-${Date.now()}`, role: "user", content, timestamp: new Date().toISOString() };
    const assistantId = `reply-${Date.now()}`;
    currentAssistantIdRef.current = assistantId;

    // Persist the auto-generated title to the backend fire-and-forget, so
    // a session-list reload (e.g. on next app launch) keeps the title the
    // user just gave it via their first message — instead of resetting to
    // the placeholder "新建对话" the createSession() call seeded.
    const currentSession = sessions.find((s) => s.id === sessionId);
    if (currentSession) {
      const newTitle = computeAutoTitle(currentSession, content);
      if (newTitle !== currentSession.title) {
        api.renameSession(sessionId, newTitle).catch((e) =>
          console.error("renameSession failed", e)
        );
      }
    }

    setSessions((prev) => prev.map((s) => {
      if (s.id !== sessionId) return s;
      // Insert user message + empty assistant placeholder
      return {
        ...s,
        messages: [
          ...s.messages,
          userMsg,
          { id: assistantId, role: "assistant", content: "", timestamp: new Date().toISOString() },
        ],
        updated_at: new Date().toISOString(),
        title: computeAutoTitle(s, content),
      };
    }));

    // Real-time stream: append each chunk to the *currently active*
    // assistant message. 2026-07-10: removed needsNewAssistantRef
    // turn-split logic — all text in an agent loop now goes to ONE
    // assistant message. Tool calls are tracked in the assistant's
    // `execution_timeline` (rendered as a collapsible block) instead
    // of separate `role: "tool"` messages. This ensures streaming
    // and reload produce identical visuals.
    const appendChunk = (chunk: string) => {
      const target = currentAssistantIdRef.current;
      if (!target) return;
      streamPhaseRef.current = "text";
      setSessions((prev) => prev.map((s) => {
        if (s.id !== sessionId) return s;
        return {
          ...s,
          messages: s.messages.map((m) =>
            m.id === target
              ? { ...m, content: m.content + chunk, unified_steps: appendTextStep(m.unified_steps, chunk) }
              : m
          ),
          updated_at: new Date().toISOString(),
        };
      }));
    };

    // 2026-07-10: finalizeToolMessage removed. Tool completion is now
    // tracked via `tool_complete` events in the assistant's
    // `execution_timeline`, not via separate `role: "tool"` messages.

    /**
     * The server has emitted a `RequestApproval` event. Three steps:
     *   1. Freeze the currently-streaming assistant message (just
     *      stop routing chunks to it — its content is what it is)
     *   2. Insert an approval message in the slot right after it,
     *      plus a fresh empty assistant placeholder that subsequent
     *      chunks will land on
     *   3. Return a promise that resolves when the user clicks
     *      allow/deny on the inline card. The stream layer
     *      immediately forwards the decision to the server.
     */
    const onApprovalRequest = (req: {
      requestId: string;
      tool: string;
      args: unknown;
      riskLevel?: string;
      /** Wall-clock deadline (Unix ms) from the server. Persisted
       *  into the approval message and fed to `<InlineApprovalCard>`
       *  so it can render a live "剩余 4m 32s" countdown against
       *  the *original* server-side deadline (not a fresh
       *  re-derivation, which would briefly lie across a reload). */
      expiresAt?: number | null;
      sessionId: string;
    }): Promise<{ decision: ApprovalDecision }> => {
      const previousAssistantId = currentAssistantIdRef.current;
      const nextAssistantId = `reply-${Date.now()}-${Math.random().toString(36).slice(2, 6)}`;
      currentAssistantIdRef.current = nextAssistantId;

      const approvalMsg: ChatMessage = {
        id: `approval-${req.requestId}`,
        role: "approval",
        content: "",
        timestamp: new Date().toISOString(),
        approval: {
          requestId: req.requestId,
          tool: req.tool,
          args: req.args,
          riskLevel: req.riskLevel,
          decision: null,
          decidedAt: null,
          // Persist the server-emitted deadline so a page reload
          // re-renders the countdown against the *original*
          // server-side timeout (not a fresh `Date.now()+5min`
          // re-derivation, which would briefly lie — the back-end
          // may have already auto-denied while the page was
          // reloading). See the comment on
          // `ApprovalMessageMeta::expiresAt` in `types.ts`.
          expiresAt: req.expiresAt ?? null,
          linkedAssistantId: previousAssistantId,
        },
      };
      const newAssistantMsg: ChatMessage = {
        id: nextAssistantId,
        role: "assistant",
        content: "",
        timestamp: new Date().toISOString(),
      };

      setSessions((prev) => prev.map((s) => {
        if (s.id !== sessionId) return s;
        // Insert approval + new assistant placeholder *after* the
        // previous (now-frozen) assistant message. The approval card
        // is the only place in the timeline where the order is
        // literally "assistant, approval, assistant" — a rare but
        // correct shape.
        return {
          ...s,
          messages: [...s.messages, approvalMsg, newAssistantMsg],
          updated_at: new Date().toISOString(),
        };
      }));

    return new Promise<{ decision: ApprovalDecision }>((resolve) => {
      approvalResolversRef.current.set(req.requestId, (decision) => resolve({ decision }));
    });
    };

    const onClarifyRequest = (req: {
      requestId: string;
      question: string;
      choices: string[];
      sessionId: string;
    }): Promise<{ answer: string }> => {
      const nextAssistantId = `reply-${Date.now()}-${Math.random().toString(36).slice(2, 6)}`;
      currentAssistantIdRef.current = nextAssistantId;

      const clarifyMsg: ChatMessage = {
        id: `clarify-${req.requestId}`,
        role: "clarify",
        content: req.question,
        timestamp: new Date().toISOString(),
        clarify: {
          requestId: req.requestId,
          question: req.question,
          choices: req.choices ?? [],
          answer: null,
        },
      };
      const newAssistantMsg: ChatMessage = {
        id: nextAssistantId,
        role: "assistant",
        content: "",
        timestamp: new Date().toISOString(),
      };

      setSessions((prev) => prev.map((s) => {
        if (s.id !== sessionId) return s;
        return {
          ...s,
          messages: [...s.messages, clarifyMsg, newAssistantMsg],
          updated_at: new Date().toISOString(),
        };
      }));

      return new Promise<{ answer: string }>((resolve) => {
        clarifyResolversRef.current.set(req.requestId, (answer) => resolve({ answer }));
      });
    };

    try {
      await sendStream(
        sessionId!,
        content,
        {
          onPhaseChange: (phase) => {
            // Only store the phase for the active session. If the user
            // switches sessions mid-stream, the new session shouldn't
            // inherit the old phase chip.
            if (sessionId === activeSessionId) {
              prevPhase.current = phase;
              setStreamPhase(phase);
            }
          },
          onChunk: appendChunk,
          onError: (err) => {
            // The error frame arrives immediately (no API key,
            // config error, etc.) — React may batch the
            // setStreamingSessionId(sessionId)+null into one render
            // so the catch block below may be too late. Handle here.
            const errText = `⚠️ ${localizeStreamError(err)}`;
            setStreamPhase(null);
            setSessions((prev) => prev.map((s) => {
              if (s.id !== sessionId) return s;
              return {
                ...s,
                messages: s.messages.map((m) =>
                  m.id === currentAssistantIdRef.current
                    ? { ...m, content: m.content || errText, stopReason: "error" as const }
                    : m
                ),
                updated_at: new Date().toISOString(),
              };
            }));
            currentAssistantIdRef.current = null;
            if (sessionId === activeSessionId) {
              setStreamError(localizeStreamError(err));
            }
          },
          onToolStart: ({ toolCallId, tool, content }) => {
            // 2026-07-10: Tool events are now appended to the
            // assistant's `execution_timeline` instead of creating
            // separate `role: "tool"` messages. This ensures:
            //   1. Tools always render BELOW the avatar (inside the
            //      assistant bubble, not as separate messages above it)
            //   2. Streaming and reload produce identical visuals
            //   3. Tools can be rendered as a collapsible block
            if (runningToolRef.current.has(toolCallId)) {
              return;
            }
            runningToolRef.current.set(toolCallId, toolCallId);

            const target = currentAssistantIdRef.current;
            if (!target) return;
            streamPhaseRef.current = "tools";

            const event: import("./types").ExecutionTimelineEvent = {
              type: "tool_start",
              tool,
              arguments: content,
              tool_call_id: toolCallId,
              created_at: new Date().toISOString(),
            };

            setSessions((prev) => prev.map((s) => {
              if (s.id !== sessionId) return s;
              return {
                ...s,
                messages: s.messages.map((m) =>
                  m.id === target
                    ? {
                        ...m,
                        execution_timeline: [...(m.execution_timeline ?? []), event],
                        unified_steps: appendToolStep(m.unified_steps, event),
                      }
                    : m
                ),
                updated_at: new Date().toISOString(),
              };
            }));
          },
          onToolChunk: ({ toolCallId, content }) => {
            // 2026-07-10: Append stdout to the assistant's
            // execution_timeline. We accumulate into a single
            // `tool_stdout` event per tool_call_id for efficiency.
            if (!content) return;
            const target = currentAssistantIdRef.current;
            if (!target) return;

            setSessions((prev) => prev.map((s) => {
              if (s.id !== sessionId) return s;
              return {
                ...s,
                messages: s.messages.map((m) => {
                  if (m.id !== target || !m.execution_timeline) return m;
                  const timeline = [...m.execution_timeline];
                  // Find the existing tool_stdout event for this tool_call_id
                  let found = false;
                  for (let i = timeline.length - 1; i >= 0; i--) {
                    if (timeline[i].type === "tool_stdout" && timeline[i].tool_call_id === toolCallId) {
                      timeline[i] = { ...timeline[i], content: (timeline[i].content ?? "") + content };
                      found = true;
                      break;
                    }
                  }
                  if (!found) {
                    timeline.push({
                      type: "tool_stdout",
                      tool_call_id: toolCallId,
                      content,
                      created_at: new Date().toISOString(),
                    });
                  }
                  return {
                    ...m,
                    execution_timeline: timeline,
                    unified_steps: updateToolStepStdout(m.unified_steps, toolCallId, content),
                  };
                }),
                updated_at: new Date().toISOString(),
              };
            }));
          },
          onToolComplete: ({ toolCallId, content }) => {
            // 2026-07-10: Append a `tool_complete` event to the
            // assistant's execution_timeline. No separate tool message,
            // no needsNewAssistantRef — all text stays in the same
            // assistant bubble.
            runningToolRef.current.delete(toolCallId);

            const target = currentAssistantIdRef.current;
            if (!target) return;

            const event: import("./types").ExecutionTimelineEvent = {
              type: "tool_complete",
              tool_call_id: toolCallId,
              content: content ?? "",
              created_at: new Date().toISOString(),
            };

            setSessions((prev) => prev.map((s) => {
              if (s.id !== sessionId) return s;
              return {
                ...s,
                messages: s.messages.map((m) =>
                  m.id === target
                    ? {
                        ...m,
                        execution_timeline: [...(m.execution_timeline ?? []), event],
                        unified_steps: appendToolStep(m.unified_steps, event),
                      }
                    : m
                ),
                updated_at: new Date().toISOString(),
              };
            }));
          },
          onFileChange: ({ toolCallId, filePath, changeType, fileSummary }) => {
            // Append a `file_change` event to the currently-active
            // assistant message's `execution_timeline`. The chat UI
            // (FileChangeBlock in MessageBubble) reads this timeline
            // via extractFileChanges() and renders a WorkBuddy-style
            // "文件变更 (N个)" row inline in the conversation.
            const target = currentAssistantIdRef.current;
            if (!target) return;
            const event: import("./types").ExecutionTimelineEvent = {
              type: "file_change",
              tool: "file_change",
              file_path: filePath,
              change_type: changeType,
              file_summary: fileSummary,
              created_at: new Date().toISOString(),
            };
            setSessions((prev) => prev.map((s) => {
              if (s.id !== sessionId) return s;
              return {
                ...s,
                messages: s.messages.map((m) => {
                  if (m.id !== target) return m;
                  const timeline = m.execution_timeline ?? [];
                  // De-dupe: same file_path + change_type only
                  // appears once even if the server re-emits.
                  const exists = timeline.some(
                    (e) =>
                      e.type === "file_change" &&
                      e.file_path === filePath &&
                      e.change_type === changeType,
                  );
                  if (exists) return m;
                  return {
                    ...m,
                    execution_timeline: [...timeline, event],
                  };
                }),
              };
            }));
            // toolCallId is accepted for symmetry with other event
            // handlers; file_change events are de-duped by path so
            // the id is informational only here.
            void toolCallId;
          },
          // 2026-07-14: append a `memory_update` event to the active
          // assistant message's `execution_timeline` (falling back to
          // the last assistant message when none is active — e.g. at
          // session end). The chat UI (MemoryUpdateBlock in
          // MessageBubble) reads this timeline via extractMemoryUpdates()
          // and renders an inline "记忆" row so the user can see what
          // the agent is remembering. De-dupe by (action, key) so a
          // re-emitted event doesn't produce a duplicate row.
          onMemoryUpdate: ({ action, memoryType, key, summary, count }) => {
            if (!summary && !key) return;
            setSessions((prev) => prev.map((s) => {
              if (s.id !== sessionId) return s;
              // Prefer the currently-streaming assistant message; if
              // none (session-end summary), use the last assistant msg.
              const target =
                currentAssistantIdRef.current ??
                [...s.messages].reverse().find((m) => m.role === "assistant")?.id ??
                null;
              if (!target) return s;
              return {
                ...s,
                messages: s.messages.map((m) => {
                  if (m.id !== target) return m;
                  const timeline = m.execution_timeline ?? [];
                  const exists = timeline.some(
                    (e) =>
                      e.type === "memory_update" &&
                      e.memory_action === action &&
                      e.memory_key === key,
                  );
                  if (exists) return m;
                  const event: import("./types").ExecutionTimelineEvent = {
                    type: "memory_update",
                    tool: "memory_update",
                    memory_action: action,
                    memory_type: memoryType,
                    memory_key: key,
                    memory_summary: summary,
                    memory_count: count,
                    created_at: new Date().toISOString(),
                  };
                  return {
                    ...m,
                    execution_timeline: [...timeline, event],
                  };
                }),
                updated_at: new Date().toISOString(),
              };
            }));
          },
          // 2026-06-29 (C3 marker protocol): append a
          // `section_gate` event to the active assistant
          // message's `execution_timeline` so the trace panel
          // can render the `SectionGateRow` inline with the
          // tool cards. Mirrors `onFileChange`'s shape: locate
          // the current assistant message via the ref, mutate
          // the session state, persist through `execution_timeline`.
          onSectionGate: (info) => {
            const target = currentAssistantIdRef.current;
            if (!target) return;
            const event: import("./types").ExecutionTimelineEvent = {
              type: "section_gate",
              tool: "section_gate",
              content: `SectionGate: patched=${info.patched.length} pending=${info.pending.length}`,
              created_at: new Date().toISOString(),
              section_gate_report: info,
            };
            setSessions((prev) => prev.map((s) => {
              if (s.id !== sessionId) return s;
              return {
                ...s,
                messages: s.messages.map((m) => {
                  if (m.id !== target) return m;
                  const timeline = m.execution_timeline ?? [];
                  return {
                    ...m,
                    execution_timeline: [...timeline, event],
                  };
                }),
              };
            }));
          },
          onReasoningChunk: (piece) => {
            // Live `<think>...</think>` text from the LLM. Append to
            // the currently-active assistant message's
            // `reasoningContent` so the folding block in
            // MessageBubble can render it (collapsed by default).
            const target = currentAssistantIdRef.current;
            if (!target) return;
            streamPhaseRef.current = "reasoning";
            setSessions((prev) => prev.map((s) => {
              if (s.id !== sessionId) return s;
              return {
                ...s,
                messages: s.messages.map((m) =>
                  m.id === target
                    ? {
                        ...m,
                        reasoningContent: (m.reasoningContent ?? "") + piece,
                        unified_steps: appendReasoningStep(m.unified_steps, piece),
                      }
                    : m
                ),
                updated_at: new Date().toISOString(),
              };
            }));
          },
          onApprovalRequest,
          onClarifyRequest,
          onAcuiDirective: (directive) => acui.apply(directive),
          // Todo (task list) mutations. Forward to the App-level
          // groups store so each plan renders as its own inline
          // card anchored to the originating message. The store
          // updates `todoGroupsByIdRef` synchronously and bumps
          // `todoGroups` state once per logical mutation (group
          // start / upsert / remove) so ChatView re-renders
          // exactly the affected card.
          onTodoChange: (change) => {
            // Commit E (Frontend Audit 2026-07-13): client-side
            // session guard. The WS server already filters by
            // session_id per connection
            // (`hermix-server/src/lib.rs::handle_ws_stream`,
            // ~line 3733), but the TodoPanel still gets every
            // `todo_group_started` / `todo_item_upserted` /
            // `todo_snapshot` that arrives on the wire — if a
            // sibling tab ever shares a WS multiplex, or if a
            // future code path bypasses the per-connection
            // filter, the wrong session's groups would land in
            // `todoGroups` and the inline plan card would render
            // on the wrong assistant message.
            //
            // `change.sessionId` is empty for legacy / non-todo
            // events — accept those (matches the server's
            // "None = legacy fan-out, always accept" policy).
            if (
              change.sessionId &&
              change.sessionId !== activeSessionIdRef.current
            ) {
              return;
            }
            setTodoGroups((prev) => {
              const next = prev.slice();
              const bump = (i: number, mut: (g: TodoGroup) => TodoGroup) => {
                const updated = { ...mut(next[i]) };
                next[i] = updated;
                todoGroupsByIdRef.current.set(updated.groupId, updated);
              };
              // P0-T2 (Frontend Audit 2026-07-13): server-sent
              // `TodoSnapshot` rebuilds groups from authoritative
              // items, preserving first-seen order within each group.
              // Items with no `group_id` (legacy / pre-v2) collapse
              // into a `_legacy` bucket so they still render.
              if (change.kind === "snapshot") {
                todoGroupsByIdRef.current.clear();
                next.length = 0;
                const order: string[] = [];
                const bucket = new Map<string, TodoSnapshotItem[]>();
                for (const it of change.items) {
                  const gid = it.group_id || "_legacy";
                  if (!bucket.has(gid)) {
                    bucket.set(gid, []);
                    order.push(gid);
                  }
                  bucket.get(gid)!.push(it);
                }
                for (const gid of order) {
                  const items = bucket.get(gid)!;
                  const itemsById: Record<string, TodoSnapshotItem> = {};
                  let title: string | null = null;
                  for (const it of items) {
                    itemsById[it.id] = it;
                    if (!title && it.group_title) title = it.group_title;
                  }
                  const g: TodoGroup = {
                    groupId: gid,
                    groupTitle: title,
                    anchorMessageId: null,
                    itemsById,
                  };
                  todoGroupsByIdRef.current.set(gid, g);
                  next.push(g);
                }
                return next;
              }
              if (change.kind === "group_started") {
                // Ignore duplicates for the same group_id.
                if (todoGroupsByIdRef.current.has(change.groupId)) {
                  const i = next.findIndex((g) => g.groupId === change.groupId);
                  if (i >= 0 && (next[i].groupTitle !== change.groupTitle || next[i].anchorMessageId !== change.messageId)) {
                    bump(i, (g) => ({ ...g, groupTitle: change.groupTitle, anchorMessageId: change.messageId }));
                  }
                  return next;
                }
                const g: TodoGroup = {
                  groupId: change.groupId,
                  groupTitle: change.groupTitle,
                  anchorMessageId: change.messageId,
                  itemsById: {},
                };
                todoGroupsByIdRef.current.set(change.groupId, g);
                next.push(g);
                return next;
              }
              if (change.kind === "upsert") {
                // Bug #1 fix (2026-07-08): skip WS echoes for items
                // that are currently in-flight from a user toggle.
                // Without this, rapid double-toggles are silently
                // reverted by a stale echo from the first click.
                if (justPatchedTodoRef.current.has(change.item.id)) {
                  return prev;
                }
                const i = next.findIndex((g) => g.groupId === (change.item.group_id || ""));
                if (i < 0) {
                  // No matching group — synthesize one with empty title
                  // so the item still renders. LLM should normally
                  // emit group_started first, but this is a safety net.
                  const g: TodoGroup = {
                    groupId: change.item.group_id || "_legacy",
                    groupTitle: change.item.group_title ?? null,
                    anchorMessageId: null,
                    itemsById: { [change.item.id]: change.item },
                  };
                  todoGroupsByIdRef.current.set(g.groupId, g);
                  next.push(g);
                  return next;
                }
                bump(i, (g) => ({
                  ...g,
                  itemsById: { ...g.itemsById, [change.item.id]: change.item },
                }));
                return next;
              }
              if (change.kind === "remove") {
                const i = next.findIndex((g) => change.id in g.itemsById);
                if (i < 0) return prev;
                bump(i, (g) => {
                  const items = { ...g.itemsById };
                  delete items[change.id];
                  return { ...g, itemsById: items };
                });
                return next;
              }
              return prev;
            });
          },
          onPrefetch: (info) => {
            setPrefetchIntent(info);
          },
          onDone: (text, stopReason) => {
            setPrefetchIntent(null);
            setStreamPhase(null);
            // 2026-07-09: Multi-turn agent loop fix.
            //
            // `text` here is `fullText` from useStreamChat — the
            // accumulated text from ALL `text` events across ALL LLM
            // turns in this stream. But `appendChunk` has already
            // routed text to the correct per-turn assistant
            // placeholders (via needsNewAssistantRef). Using `text`
            // to overwrite the last assistant's content would:
            //   - Duplicate turn 1's text into the last assistant
            //   - Or replace correctly-streamed text with a garbled
            //     concatenation of all turns' text
            //
            // New approach: trust the in-memory state built by
            // appendChunk. Only use `text` as a FALLBACK when the
            // last assistant's content is empty (e.g. the LLM
            // produced no visible text, or all text arrived in the
            // done event instead of via streaming chunks).
            const assistantIdAtDone = currentAssistantIdRef.current;
            const replaced = (() => {
              if (!assistantIdAtDone) return false;
              setSessions((prev) => prev.map((s) => {
                if (s.id !== sessionId) return s;
                const target = s.messages.find((m) => m.id === assistantIdAtDone);
                if (!target) return s;
                // Only use `text` as fallback if the assistant has
                // no content from streaming (appendChunk never ran
                // or produced empty). This covers the edge case
                // where the LLM's entire response arrived in the
                // done event instead of via text chunks.
                const needsFallbackText =
                  (!target.content || target.content.trim() === "") &&
                  text && text.trim().length > 0;
                if (target.stopReason === stopReason && !needsFallbackText) return s;
                return {
                  ...s,
                  messages: s.messages.map((m) =>
                    m.id === assistantIdAtDone
                      ? {
                          ...m,
                          content: needsFallbackText ? text : m.content,
                          stopReason,
                        }
                      : m
                  ),
                  updated_at: new Date().toISOString(),
                };
              }));
              return true;
            })();
            if (!replaced) {
              // Fallback: stamp the trailing assistant row with
              // `stopReason` (preserves the old behavior when we
              // somehow don't have a target id).
              setSessions((prev) => prev.map((s) => {
                if (s.id !== sessionId) return s;
                const msgs = [...s.messages];
                for (let i = msgs.length - 1; i >= 0; i--) {
                  if (msgs[i].role === "assistant") {
                    msgs[i] = { ...msgs[i], stopReason } as ChatMessage;
                    break;
                  }
                }
                return { ...s, messages: msgs };
              }));
            }
            // 2026-07-09: Removed the 500ms re-fetch that replaced the
            // entire messages array with server-persisted data.
            //
            // Root cause of "工具结果突然消失" bug:
            // During streaming, onToolStart/onToolChunk/onToolComplete
            // build complete tool messages in memory. When onDone fires,
            // the 500ms re-fetch replaced these with server data. If the
            // server hadn't finished persisting tool results yet (race
            // condition), or if the execution_timeline was incomplete,
            // expandTimelineToMessages produced fewer tool messages than
            // what was already in memory — making tool results vanish.
            //
            // The in-memory state from streaming callbacks IS the source
            // of truth for the active session. The server's persisted
            // data will be loaded on the next session switch
            // (getSessionMessages in the activeSessionId effect).

            // Stream is done — drop the assistant id ref so a stale
            // value can't route chunks into the wrong message if
            // the user fires off a new turn later.
            currentAssistantIdRef.current = null;
            // Reject any unresolved approval requests. This can
            // happen if the agent was interrupted (cancel / ws
            // close) mid-approval — without this the resolver map
            // would leak forever.
            approvalResolversRef.current.forEach((resolver) => resolver("denied"));
            approvalResolversRef.current.clear();
            // Same deal for clarify — the agent stopped mid-question
            // (cancel, ws close) so we drop any pending resolver with
            // an empty answer; the server's tool call will time out
            // and the agent loop will surface the error.
            clarifyResolversRef.current.forEach((resolver) => resolver(""));
            clarifyResolversRef.current.clear();
            // M5: clear the ACUI card stack — any cards still in
            // flight belong to the now-finished turn and shouldn't
            // bleed into the next send.
            acui.reset();
          },
        },
        overrides,
      );
    } catch (e) {
      const errText = `⚠️ ${localizeStreamError(String(e))}`;
      setSessions((prev) => prev.map((s) => {
        if (s.id !== sessionId) return s;
        return {
          ...s,
          messages: s.messages.map((m) =>
            m.id === currentAssistantIdRef.current
              ? { ...m, content: m.content || errText }
              : m
          ),
          updated_at: new Date().toISOString(),
        };
      }));
      currentAssistantIdRef.current = null;
      // Surface the error to the active session so ChatView can
      // render a synthetic error row if the optimistic assistant
      // placeholder was filtered out (e.g. the WS rejected the
      // handshake before any chunk landed).
      if (sessionId === activeSessionId) {
        setStreamError(localizeStreamError(e instanceof Error ? e.message : String(e)));
      }
    }
  }, [activeSessionId, sessions, sendStream]);

  // 2026-06-28: pop a queued item out of turn and dispatch it
  // immediately. Defined here (after `handleSendMessage`) so the
  // useCallback deps array can reference it without tripping TSC's
  // "used before declaration" check.
  //
  //   !streaming → call handleSendMessage directly, then drop the
  //                row from the queue. If the send throws, re-prepend
  //                so the user can retry.
  //   streaming  → drop the row, then re-prepend. The drain effect
  //                below (keyed on `[streaming, messageQueue]`) will
  //                pick it up the next tick `streaming` settles.
  const sendFromQueue = useCallback(
    async (idx: number) => {
      // Snapshot the queue atomically inside the updater so we
      // don't race against any in-flight queue mutation (e.g. the
      // drain effect also calls setMessageQueue). Use `undefined`
      // as the initial value so TS keeps the union alive across
      // the updater's optional assignment — `null` would narrow
      // to `never` after the assignment, breaking `.trim()` below.
      let snapshot: string | undefined;
      setMessageQueue((prev) => {
        const found = prev[idx];
        if (typeof found === "string") snapshot = found;
        return prev;
      });
      const item = snapshot;
      if (!item || !item.trim()) return;

      // Pop the item first so the UI updates instantly.
      setMessageQueue((prev) => prev.filter((_, i) => i !== idx));
      if (streaming) {
        setMessageQueue((prev) => [item, ...prev]);
        return;
      }
      try {
        await handleSendMessage(item);
      } catch {
        // Failure → put it back at the head of the queue so the
        // user can retry. We don't share the drain effect's retry
        // tracker because that's keyed on auto-drain failures, not
        // manual sends.
        setMessageQueue((prev) => [item, ...prev]);
      }
    },
    [streaming, handleSendMessage],
  );

  // Auto-send queued messages when streaming completes.
  //
  // Lives here (after `handleSendMessage` is declared) because the
  // effect captures it in its closure and TSC's lexical analysis
  // would otherwise flag "used before declaration" — the React
  // runtime order is fine (useCallback runs before the effect's first
  // execution) but tsc only sees the source-order TDZ.
  //
  // 2026-06-26 v2 fix: was originally placed above `handleSendMessage`
  // without `handleSendMessage` in the deps array, relying on the
  // `[streaming, messageQueue]` triggers to re-bind the closure. That
  // happens to work for `messageQueue` (a React state read by index)
  // but is a footgun: if `handleSendMessage` ever closes over a
  // dep that changes without `streaming`/`messageQueue` changing
  // (e.g. `activeSessionId` flip mid-stream), the effect would call
  // a stale closure and drop the queued message.
  //
  // 2026-06-26 v3 fix: .catch() re-enqueue on failure (previously
  // silently dropped the message). Max 3 retries via ref-based
  // fallback tracker to prevent infinite drain loop.
  const queueRetriesRef = useRef<Record<string, number>>({});
  useEffect(() => {
    if (!streaming && messageQueue.length > 0) {
      const next = messageQueue[0];
      diagLog("[diag] queue drain: firing", { content: next, queueLen: messageQueue.length, streaming });
      setMessageQueue(prev => prev.slice(1));
      handleSendMessage(next).catch((_cause) => {
        diagLog("[diag] queue drain: handleSendMessage failed, re-enqueueing", { next, cause: _cause instanceof Error ? _cause.message : String(_cause) });
        // Limit retries: track per-message so a permanently failing
        // backend doesn't spin forever.
        const key = next.slice(0, 60); // first 60 chars as key
        const retries = queueRetriesRef.current;
        if ((retries[key] ?? 0) >= 3) {
          console.warn("[queue] giving up on message after 3 retries", next);
          delete retries[key];
          return; // drop — user can retype
        }
        retries[key] = (retries[key] ?? 0) + 1;
        setMessageQueue(prev => [next, ...prev]);
      });
    }
  }, [streaming, messageQueue, handleSendMessage]);

  // /undo: soft-delete the most recent user→assistant turn, then re-fetch
  // the active session so the UI reflects the change immediately. The
  // backend is the source of truth for `messages.active`; we just need
  // to re-pull the now-shorter list.
  const handleUndo = useCallback(async () => {
    if (!activeSessionId) return;
    const res = await api.undoSession(activeSessionId, 1);
    if (!res) return;
    try {
      const rawMessages = await api.getSessionMessages(activeSessionId);
      const messages = rawMessages;
      setSessions((prev) => prev.map((s) => s.id === activeSessionId ? { ...s, messages } : s));
    } catch {}
  }, [activeSessionId]);

  // /restore: re-activate the most recently deactivated block. Same
  // refresh pattern as handleUndo.
  const handleRestore = useCallback(async () => {
    if (!activeSessionId) return;
    const res = await api.restoreSession(activeSessionId);
    if (!res) return;
    try {
      const rawMessages = await api.getSessionMessages(activeSessionId);
      const messages = rawMessages;
      setSessions((prev) => prev.map((s) => s.id === activeSessionId ? { ...s, messages } : s));
    } catch {}
  }, [activeSessionId]);

  return (
    <div className="app-shell">
      <TitleBar
        onToggleSidebar={() => setSidebarOpen((v) => !v)}
      />
      <div
        className="app-body"
        data-left-open={sidebarOpen ? "true" : "false"}
        data-right-open={rightOpen ? "true" : "false"}
        style={{
          "--ab-left-w": `${left.width}px`,
          "--ab-right-w": rightOpen ? `${right.width}px` : "0px",
          "--pane-main-min": `${mainMinWidth}px`,
          "--left-min": `${leftMin}px`,
        } as React.CSSProperties}
      >
        {sidebarOpen && <div className="pane-overlay md:hidden" onClick={() => setSidebarOpen(false)} />}

        {/* Left sidebar — grid column 1 controls width; wrapper handles mobile slide */}
        <div
          className={`${sidebarOpen ? "translate-x-0" : "-translate-x-full"} transition-transform duration-200 absolute md:relative z-20 h-full min-h-0 overflow-hidden`}
        >
          <Sidebar
            page={page}
            onPageChange={setPage}
            sessions={sessions}
            activeSessionId={activeSessionId}
            streamingSessionId={streamingSessionId}
            onNewChat={handleNewChat}
            onSelectSession={handleSelectSession}
            onDeleteSession={handleDeleteSession}
            isCreatingSession={isCreatingSession}
            onRoomSelect={setSelectedRoomId}
          />
          <div
            data-tauri-no-drag
            onMouseDown={left.onMouseDown}
            className="pane-resizer pane-resizer-left hidden md:block"
          />
        </div>

        {/* Main — flex: 1 + min-width: 0 lets the right panel claim its
            full width first; if the window can't fit, the right panel
            is auto-closed (see recompute() < 1140px), and the main
            column takes the full remaining width. Main column always
            has `min-w-0` so inner content scrolls instead of getting
            pushed below usable width. Three-column layout never wraps —
            it auto-collapses to two columns at 1140px. */}
        <ErrorBoundary name="主内容区">
        <main className="pane pane-main min-w-0 flex-1">
          {page === "chat" && (
            <ChatView
              session={activeSession}
              onSendMessage={handleSendMessage}
              onNewChat={handleNewChat}
              // v2 (2026-07-07): inline todo cards in the message stream.
              // App owns the groups store; ChatView renders one card per
              // group right after the originating assistant message.
              todoGroups={todoGroups}
              onTodoPatch={async (id, patch) => {
                try {
                  await api.patchTodo(id, patch);
                } catch (e) {
                  console.error('[App] patchTodo failed', e);
                  throw e;
                }
              }}
              justPatchedTodoRef={justPatchedTodoRef}
              onOpenExperts={() => setPage("experts")}
              onOpenSettings={() => setPage("settings")}
              onOpenComputer={openComputer}
              onUndo={handleUndo}
              onRestore={handleRestore}
              onStop={cancelStream}
              isStreaming={isStreamingActiveSession}
              streamingPhase={streamPhase ?? (isStreamingActiveSession ? "streaming" : null)}
              streamingStatus={streamingStatus}
              compactionStatus={compactionStatus}
contextUsage={contextUsage}
              usageSnapshot={usageSnapshot}
              pendingToolsCount={pendingToolsCount}
              environmentLabel="本地Hermix"
              isSandbox={false}
              branch="main"
              workspaceRoot={workspaceRoot}
              onWorkspaceChange={onWorkspaceChange}
              onApproveApproval={handleApproveApproval}
              onDenyApproval={handleDenyApproval}
              onAnswerClarify={handleAnswerClarify}
              acuiState={acui.state}
              acuiBus={acuiBus}
              acuiStats={acui.stats}
              messageQueue={messageQueue}
              onAddToQueue={(msg) => setMessageQueue(prev => prev.includes(msg) ? prev : [...prev, msg])}
              onClearQueue={() => setMessageQueue([])}
              onRemoveFromQueue={removeFromQueue}
              onEditQueueItem={editQueueItem}
              onSendFromQueue={sendFromQueue}
              messageLoadError={
                activeSessionId ? messageLoadErrors.get(activeSessionId) ?? null : null
              }
              onRetryLoad={() => {
                if (activeSessionId) {
                  void handleSelectSession(activeSessionId);
                }
              }}
              streamError={streamError}
              onClearStreamError={clearStreamError}
              prefetchIntent={prefetchIntent}
              rightOpen={rightOpen}
              onToggleRight={handleToggleRight}
              onEnterWelcome={handleEnterWelcome}
              onPathClick={handlePathClick}
              pendingFileAttachment={pendingFileAttachment}
              onConsumeFileAttachment={() => setPendingFileAttachment(null)}
            />
          )}
          {page === "search" && <SearchView sessions={sessions} onSelectSession={handleSelectSession} />}
          {page === "plugins" && <PluginsView />}
          {page === "automation" && <AutomationView />}
          {page === "agents" && (
            <AgentsView
              activeSessionId={activeSessionId}
              onAfterHire={async () => {
                if (!activeSessionId) {
                  try {
                    const session = await api.createSession("新建对话");
                    setSessions((prev) => [session, ...prev]);
                    setActiveSessionId(session.id);
                  } catch { /* fall through */ }
                }
                // Same UX as the dedicated "experts" page: hiring
                // a builtin expert should land the user back in the
                // chat composer with the expert already adopted.
                // Without this, setExpert() updates the sidebar
                // chip but the user is still staring at the agent
                // catalog wondering why the click "did nothing".
                setPage("chat");
              }}
            />
          )}
          {page === "groupchat" && <GroupChatView initialRoomId={selectedRoomId} />}
          {page === "experts" && (
            <AgentsView
              initialTab="builtin"
              activeSessionId={activeSessionId}
              onAfterHire={async () => {
                // Same UX as the old ExpertsView.onBack: ensure there's an
                // active session, then jump back to the chat.
                if (!activeSessionId) {
                  try {
                    const session = await api.createSession("新建对话");
                    setSessions((prev) => [session, ...prev]);
                    setActiveSessionId(session.id);
                  } catch {
                    /* fall through */
                  }
                }
                setPage("chat");
              }}
            />
          )}
          {page === "analytics" && <AnalyticsView />}
          {page === "settings" && <SettingsView />}
        </main>
        </ErrorBoundary>        {/* Right sidebar — grid column 3; 0px when closed (collapsed), full
            width when open. Grid ensures main content (column 2, 1fr) is
            never squeezed — it gets all remaining space after fixed columns. */}
        {rightOpen && (
          <div className="h-full flex border-l border-border-subtle overflow-hidden min-w-0 min-h-0">
            <div
              data-tauri-no-drag
              onMouseDown={right.onMouseDown}
              className="pane-resizer pane-resizer-right hidden md:block"
            />
            <RightSidebar
              open={rightOpen}
              onClose={() => setRightOpen(false)}
              artifacts={artifacts}
              rawCount={rawArtifacts.length}
              selectedId={selectedArtifactId}
              onSelectArtifact={(id) => setSelectedArtifactId(id)}
              workspaceRoot={workspaceRoot}
              activeTab={rightTab}
              onTabChange={setRightTab}
              onAddFileToChat={handleAddFileToChat}
            />
          </div>
        )}
      </div>

      {/* ─── Artifact preview modal (主人原话 2026-06-22) ───
          "阅读界面直接弹出界面显示吧,遮罩主界面。
           不要对产物栏有任何关联,包括返回按钮什么的,
           直接显示一个窗口。"
          当用户在产物栏点文件 / 在 markdown 里点产物链接,
          这里渲染一个全屏 modal,完全遮罩整个 App,
          与右栏没有任何视觉关联——关闭只看 X / ESC / 遮罩点击。 */}
      {selectedArtifactId && (() => {
        const artifact = artifacts.find((a) => a.id === selectedArtifactId) ?? null;
        if (!artifact) return null;
        return (
          <ArtifactPreviewModal
            artifact={artifact}
            workspaceRoot={workspaceRoot}
            onClose={() => setSelectedArtifactId(null)}
          />
        );
      })()}
    </div>
  );
}

/**
 * Full-window modal that displays an artifact's content.
 *
 * Mounted at the App root so it overlays everything — sidebar,
 * right sidebar, titlebar — with a backdrop click + ESC + close
 * button for dismissal. Keeps the preview pane **completely
 * independent** of the artifact sidebar: closing the modal does
 * not touch the sidebar's state, and the sidebar's state does not
 * leak into the modal.
 */
function ArtifactPreviewModal({
  artifact,
  workspaceRoot,
  onClose,
}: {
  artifact: Artifact;
  workspaceRoot?: string | null;
  onClose: () => void;
}) {
  // ESC closes the modal. Effect attaches the listener only while
  // the modal is mounted; cleanup removes it on unmount so we don't
  // leak handlers when the user navigates to other views.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        onClose();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  return (
    <div
      className="fixed inset-0 z-[60] flex items-stretch justify-center"
      // Backdrop click closes the modal. We deliberately use a
      // direct `onClick` (not a stopPropagation wrapper) — child
      // clicks bubble up through the panel but we check `e.target
      // === e.currentTarget` so panel-internal clicks don't
      // accidentally close it.
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
      // Two-layer backdrop: a 65% black scrim for contrast + a soft
      // 2px blur of the underlying app, so the dialog reads as a
      // genuine "floating" surface rather than a hard pop. The blur
      // is small on purpose — the desktop UI behind is the user's
      // own chat, over-blurring it would feel like the app froze.
      style={{
        background: "rgba(0, 0, 0, 0.65)",
        backdropFilter: "blur(2px)",
        WebkitBackdropFilter: "blur(2px)",
      }}
    >
      <div
        // Modal box — premium frame:
        //  • width: 1100px cap (92vw on narrow screens)
        //  • height: 88vh so the desktop titlebar is still visible
        //    for drag/resize affordance
        //  • 16px radius (xl) + 1px border + 2px outline (subtle
        //    light-mode highlight) + heavy shadow
        //  • background uses theme bg-primary (auto light/dark)
        //  • outline gives the panel a hairline highlight that
        //    survives any bg color, so light mode reads "lifted off"
        //    not "floating void"
        className="flex flex-col shadow-2xl
                   w-[min(1100px,92vw)] h-[88vh] mt-[5vh] mb-[5vh]
                   rounded-2xl overflow-hidden
                   border border-[color:var(--color-border-subtle)]
                   outline outline-2 outline-[color:color-mix(in_srgb,var(--color-text-primary)_6%,transparent)]
                   outline-offset-[-2px]"
        style={{
          background: "var(--color-bg-primary)",
          color: "var(--color-text-primary)",
          boxShadow:
            "0 24px 64px -12px rgba(0, 0, 0, 0.45), 0 8px 24px -8px rgba(0, 0, 0, 0.25)",
        }}
      >
        <PreviewPanel
          artifact={artifact}
          workspaceRoot={workspaceRoot}
          onBack={onClose}
        />
      </div>
    </div>
  );
}
