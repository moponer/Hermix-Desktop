import { useState, useRef, useEffect, useMemo, useCallback, memo } from "react";
import {
  PanelRight,
  Lock,
} from "lucide-react";
import type { ChatMessage, TimelineStep, QueuedMessage, QueuedImage } from "../types";
import type { CompressMethod } from "../utils/imageCompressor";
import { CompactionStatusBanner } from "./CompactionStatusBanner";
import type { CompactionStatus } from "./CompactionStatusBanner";

// 2026-06-25: pendingImages 数据结构。
//  - file: 浏览器 File 句柄（几 KB），不存 base64（8MB+）
//  - blobUrl: 浏览器内部引用，src= 直接用，零拷贝
//  - isCompressed / method: UI 角标提示压缩状态
export interface PendingImage {
  file: File;
  blobUrl: string;
  sizeBytes: number;
  mimeType: string;
  isCompressed: boolean;
  method: CompressMethod;
  name: string;
}
import type { AcuiState } from "../acui/types";
import type { AcuiDecisionBus } from "../acui/AcuiRegistry";
import { AcuiPanel } from "../acui/AcuiPanel";
import { MessageBubble } from "./MessageBubble";
import { AgentActivityIndicator, WaitForInputIndicator, QueueItemRow } from "./ChatIndicators";
import { StreamingBubble } from "./StreamingBubble";
import type { StreamPhase } from "../hooks/useStreamChat";
import type { TodoSnapshotItem } from "../hooks/useStreamChat";
import { TodoListCard } from "./TodoListCard";

// Memoized wrapper around the full MessageBubble. Without this, every
// parent re-render (e.g. every streaming chunk) re-mounts the whole
// react-markdown + Prism tree for *every* historical message in the
// session. The default export of MessageBubble is unmemoized; we
// supply our own shallow-equality check that only cares about the
// message identity (id) and the few callback identities that matter.
const MemoMessageBubble = memo(MessageBubble, (prev, next) => {
  if (prev.message !== next.message) return false;
  if (prev.workspaceRoot !== next.workspaceRoot) return false;
  if (prev.onApproveApproval !== next.onApproveApproval) return false;
  if (prev.onDenyApproval !== next.onDenyApproval) return false;
  if (prev.onAnswerClarify !== next.onAnswerClarify) return false;
  // `showHeader` is computed per message from the surrounding
  // context (preceding user/tool/sentinel), so changes here
  // must bust the memo — otherwise turning the header on/off
  // mid-stream wouldn't re-render the bubble.
  if (prev.showHeader !== next.showHeader) return false;
  // `isStreaming` flips when this bubble becomes (or stops being)
  // the live streaming message. Without this, the "等待回复"
  // placeholder would be cached on the wrong row after stream end.
  if (prev.isStreaming !== next.isStreaming) return false;
  // `onPathClick` is the bridge that turns a markdown body path
  // into a RightSidebar preview. ChatView passes a single
  // useCallback from App, so the reference is stable across
  // renders, but the memo must still check it — otherwise stale
  // callback references could ship into a bubble that needs the
  // current one (e.g. a click that lands after a session switch).
  if (prev.onPathClick !== next.onPathClick) return false;
  // PLAN_UI_ENHANCEMENT §1.1 — 专家切换分界时间戳。
  // 用户切换专家后，expertCutoffAt 会变化，需要让 memo 重新渲染。
  if (prev.expertCutoffAt !== next.expertCutoffAt) return false;
  return true;
});

/**
 * Merge consecutive assistant messages into a single rendered bubble.
 *
 * Background: the backend `run_stream` may produce multiple Assistant
 * entries per user turn (one per LLM call within the same turn).  When
 * rendered as-is the chat shows N stacked empty/partial bubbles for one
 * user message.  This function collapses a run of adjacent Assistant
 * messages into a single bubble whose body concatenates the text in
 * order and whose tool timeline concatenates in order.  Empty
 * placeholders (no content and no tool calls) are dropped entirely.
 *
 * Non-assistant messages (User / Approval / Clarify / Status) are never
 * merged and are returned as-is, in original order, separated by the
 * merged assistant runs.
 */
function mergeConsecutiveAssistantMessages(messages: ChatMessage[]): ChatMessage[] {
  const out: ChatMessage[] = [];
  let i = 0;
  while (i < messages.length) {
    const m = messages[i];
    if (m.role !== "assistant") {
      out.push(m);
      i += 1;
      continue;
    }
    // Collect the run of consecutive assistant messages.
    let j = i;
    const run: ChatMessage[] = [];
    while (j < messages.length && messages[j].role === "assistant") {
      run.push(messages[j]);
      j += 1;
    }
    // Filter out empty placeholders.
    // 2026-07-11: Added `reasoningContent` check — rows that only
    // have reasoning (no content, no tools, no timeline) were being
    // silently dropped, causing thinking blocks and body text (when
    // the LLM didn't close its <think> tag, the server stores
    // everything in reasoning_content with empty content) to
    // disappear on reload.
    const nonEmpty = run.filter(
      (r) =>
        (r.content && r.content.trim().length > 0) ||
        (r.tool_calls && r.tool_calls.length > 0) ||
        (r.execution_timeline && r.execution_timeline.length > 0) ||
        (r.reasoningContent && r.reasoningContent.trim().length > 0)
    );
    if (nonEmpty.length === 0) {
      // Whole run is empty placeholders → drop entirely.
      i = j;
      continue;
    }
    if (nonEmpty.length === 1) {
      // 2026-07-11: Build unified_steps even for single-row cases
      // so the rendering path is identical between single-row and
      // multi-row merges. Without this, resolveSteps falls back to
      // buildFallbackSteps which may produce a different step order.
      const single = nonEmpty[0];
      const singleSteps: TimelineStep[] = [];
      const reasoning = (single.reasoningContent || "").replace(/<\/?think>/gi, "").trim();
      const toolEvents = (single.execution_timeline || []).filter(
        (e) => e.type === "tool_start" || e.type === "tool_stdout" || e.type === "tool_complete",
      );
      const hasBody = !!(single.content && single.content.trim());
      // When content has body text: show reasoning as reasoning block + body as text.
      // When content is empty but reasoning has text: the LLM didn't close its
      // <think> tag, so ALL text (including body) is in reasoningContent.
      // Show it as body text (not reasoning block) to match the streaming
      // onDone fallback behavior.
      if (hasBody) {
        if (reasoning) {
          singleSteps.push({ kind: "reasoning", content: reasoning });
        }
        if (toolEvents.length > 0) {
          singleSteps.push({ kind: "tools", events: toolEvents });
        }
        singleSteps.push({ kind: "text", content: single.content });
      } else if (reasoning) {
        if (toolEvents.length > 0) {
          singleSteps.push({ kind: "tools", events: toolEvents });
        }
        singleSteps.push({ kind: "text", content: reasoning });
      } else if (toolEvents.length > 0) {
        singleSteps.push({ kind: "tools", events: toolEvents });
      }
      out.push({
        ...single,
        unified_steps: singleSteps.length > 0 ? singleSteps : single.unified_steps,
      });
      i = j;
      continue;
    }
    // Concatenate contents, execution_timeline, tool_calls, and reasoningContent.
    const mergedContent = nonEmpty
      .map((r) => r.content || "")
      .filter((c) => c.length > 0)
      .join("\n\n");
    const mergedTimeline = nonEmpty.flatMap(
      (r) => r.execution_timeline || []
    );
    const mergedToolCalls = nonEmpty.flatMap(
      (r) => r.tool_calls || []
    );
    const mergedReasoning = nonEmpty
      .map((r) => r.reasoningContent || "")
      .filter((c) => c.length > 0)
      .join("\n\n");
    // Build unified timeline steps from per-iteration data.
    // Each assistant row represents one LLM iteration with its own
    // reasoning / tools / text. By iterating in order and pushing
    // steps per-row, we preserve the chronological flow:
    //   [reasoning 1] → [tools 1] → [reasoning 2] → [tools 2] → [text]
    //
    // CRITICAL: For intermediate rows, reasoning → reasoning block and
    // content → text block are INDEPENDENT. Do NOT show intermediate
    // rows' reasoning as body text — that would display thinking
    // fragments as body text (the "思考过程的片段" bug).
    //
    // Only for the LAST row: if content is empty but reasoning has text,
    // the LLM didn't close its <think> tag so all text (including body)
    // is in reasoningContent. Show it as body text (matching the
    // streaming onDone fallback).
    const unifiedSteps: TimelineStep[] = [];
    for (let ri = 0; ri < nonEmpty.length; ri++) {
      const r = nonEmpty[ri];
      const isLastRow = ri === nonEmpty.length - 1;
      const reasoning = (r.reasoningContent || "").replace(/<\/?think>/gi, "").trim();
      const toolEvents = (r.execution_timeline || []).filter(
        (e) => e.type === "tool_start" || e.type === "tool_stdout" || e.type === "tool_complete",
      );
      const hasBody = !!(r.content && r.content.trim());
      if (hasBody) {
        if (reasoning) {
          unifiedSteps.push({ kind: "reasoning", content: reasoning });
        }
        if (toolEvents.length > 0) {
          unifiedSteps.push({ kind: "tools", events: toolEvents });
        }
        unifiedSteps.push({ kind: "text", content: r.content });
      } else if (isLastRow && reasoning) {
        // Fallback: last row has no content but has reasoning —
        // unclosed <think> tag, show reasoning as body text.
        if (toolEvents.length > 0) {
          unifiedSteps.push({ kind: "tools", events: toolEvents });
        }
        unifiedSteps.push({ kind: "text", content: reasoning });
      } else {
        // Intermediate row with no content: reasoning is genuine
        // thinking, show as reasoning block (not body text).
        if (reasoning) {
          unifiedSteps.push({ kind: "reasoning", content: reasoning });
        }
        if (toolEvents.length > 0) {
          unifiedSteps.push({ kind: "tools", events: toolEvents });
        }
      }
    }
    // Find the last non-empty stopReason in the run (prefer the final
    // message's reason since the server sets it on the last persisted row).
    const mergedStopReason = nonEmpty
      .map((r) => r.stopReason)
      .filter((s): s is string => !!s && s.length > 0)
      .pop();
    // Use the last message's id/timestamp so the bubble looks like the
    // final one and React keying stays stable.
    const last = nonEmpty[nonEmpty.length - 1];
    out.push({
      ...last,
      content: mergedContent,
      tool_calls:
        mergedToolCalls.length > 0 ? mergedToolCalls : last.tool_calls,
      execution_timeline:
        mergedTimeline.length > 0
          ? mergedTimeline
          : last.execution_timeline,
      reasoningContent:
        mergedReasoning.length > 0
          ? mergedReasoning
          : last.reasoningContent,
      unified_steps: unifiedSteps.length > 0 ? unifiedSteps : undefined,
      stopReason: mergedStopReason ?? last.stopReason,
    });
    i = j;
  }
  return out;
}

/**
 * Should the assistant bubble at `idx` render its avatar + meta line?
 *
 * v18 (2026-06-17) — 主人原话：
 *   "所有的消息应该是在模型名称下面的，也就是说每次 agentloop
 *    都应该只在一个模型头像名称下显示，中间不会再出现，除非下一次
 *    的对话或下一次用户发送消息后，才会转入在一个 agentloop"
 *
 * v18 + Round 24 (2026-07-08) — 头像缺失修复：
 *   agent loop 内的**第一条** assistant（紧跟 user）已经显示了头像，
 *   后面的 assistant 不再显示。但 streaming / merge 流程会让第一
 *   条 assistant 被 `mergeConsecutiveAssistantMessages` 当成空
 *   placeholder 过滤掉，剩下唯一的"merged assistant"在 `messagesWithStream`
 *   里渲染 —— 此时 `findIndex` 找到的是合并组**最后**一条的 idx，
 *   它的前一个非 tool 消息就是合并组**第一**条 assistant（也是
 *   assistant），旧规则会误判为"loop 内非起点"。修复：向前跳过
 *   **连续的 assistant 消息**（不只是 tool），只到第一个
 *   非 {tool, assistant} 的消息，再判断是不是 user。
 *
 * 规则：
 *   · agent loop 的边界 = 用户消息 (`role: "user"`)
 *   · 一个 agent loop 内**所有 assistant 消息共享一个 header** —
 *     第一个（紧跟 user）显示，后续的（紧跟 tool / approval /
 *     clarify / 之前那个 assistant）都不显示
 *   · approval / clarify 不算"用户消息"，它们是 loop 内的"等待
 *     用户输入"状态；用户在弹窗里回答后 agent 继续干活，仍然
 *     是同一个 loop
 *
 * 之前用 `(effectivePrev === null || effectivePrev.role !== "assistant")`
 * 会把 approval / clarify 误判成新 loop 起点，让"刚刚 + 头像 + 名称"
 * 在同一个 user→assistant 链里重复出现两到三次。这条函数把边界收
 * 紧到"只有 user 才算" — 跟主人 v18 描述一致。
 *
 * @param idx   index into `messages` (raw session.messages order)
 * @param messages  full ordered chat history
 * @returns true iff the assistant bubble at `idx` should render its header
 */
function shouldShowAssistantHeader(idx: number, messages: ReadonlyArray<{ role?: ChatMessage["role"] }>): boolean {
  const msg = messages[idx];
  if (!msg || msg.role !== "assistant") return false;
  // Skip backward over ALL messages that belong to the same agent-loop
  // turn: assistant (empty or not), tool, approval, clarify.
  //
  // The server persists one assistant row per LLM iteration. A single
  // user turn can produce: [user, assistant1(tool_calls), tool,
  // assistant2(final text)]. After wireArrayToChatMessages merges the
  // tool row into assistant1's timeline, the array becomes [user,
  // assistant1, assistant2]. mergeConsecutiveAssistantMessages then
  // merges them into one message (using assistant2's id). When we look
  // up assistant2 in session.messages, its prev is assistant1 — not a
  // user — so the header would be suppressed. Skipping ALL assistant
  // messages (not just empty ones) ensures we find the turn boundary.
  //
  // The `role?` shape is structural: `ChatMessage[]` (raw reload)
  // matches, and so does the P0-T1 `streamRow` (each element carries
  // either a `msg.role` or no `role` for todo rows). Todo rows with
  // `role === undefined` are skipped via the `r === undefined` branch.
  let prevIdx = idx - 1;
  while (prevIdx >= 0) {
    const r = messages[prevIdx]?.role;
    if (r === undefined) {
      // Interleaved todo row — keep walking back.
      prevIdx--;
      continue;
    }
    if (r === "assistant" || r === "tool" || r === "approval" || r === "clarify") {
      prevIdx--;
    } else {
      break;
    }
  }
  const effectivePrev = prevIdx >= 0 ? messages[prevIdx] : null;
  // agent loop 起点 = 紧跟 user 消息的 assistant
  return effectivePrev === null || effectivePrev.role === "user";
}

import { Send, Square, FolderOpen, ChevronDown, ChevronRight, Sparkles, ShieldCheck, Image, X, ArrowUp, Check, AlertTriangle } from "lucide-react";
import { PrefetchIntentChip } from "./MessageBubble";
import { VoiceButton } from "./VoiceButton";
import { VisionButton } from "./VisionButton";
import {
  compressImage,
  skipCompression,
  formatBytes,
  type CompressResult,
} from "../utils/imageCompressor";
import { PolishButton } from "./PolishButton";
import { PopoverShell, PopoverBody } from "./PopoverShell";
import { usePopover } from "../hooks/usePopover";
import {
  usePermissionLevel,
  useSessionExpert,
  useExpertCatalog,
  useRecentExperts,
  useSessionWorkspace,
  useSessionSkills,
  PERMISSION_LEVELS,
  type PermissionLevel,
} from "../hooks/useExpert";
import { useExpertCutoff } from "../hooks/useExpertCutoff";
import { useWorkspaces } from "../hooks/useWorkspaces";
import { pickDirectory } from "../desktopBridge";
import { getSessionArchiveRefs, getAvailableTools, updateSessionMetadata, apiUrl, type ArchiveRef, type ToolDescriptor } from "../api";
import { ArchiveRefsGraph } from "./ArchiveRefsGraph";
import type { Session } from "../types";
import type { ChatOverrides } from "../hooks/useStreamChat";
const mascotUrl = "/mascot-256.png";
import { storage, STORAGE_KEYS } from "../utils/storageCompat";
import { useCurrentModel, MODEL_CONFIG_CHANGED_EVENT } from "../hooks/useCurrentModel";
import { ProviderIcon } from "./ProviderIcon";

const BUILTIN_PROVIDER_IDS = new Set([
  "minimax", "openai", "deepseek", "dashscope", "kimi",
  "siliconflow", "openrouter", "zhipu", "volcengine",
  "anthropic", "copilot", "qwen", "moonshot",
  // 4 个新增国内厂商
  "baichuan", "tencent", "spark", "yi",
]);

const MODEL_CONFIG_KEY = STORAGE_KEYS.modelConfig;
interface ModelConfigData { enabled: { providerId: string; modelId: string }[]; defaultModel: string }

function useModelSelector() {
  // Read the full model config (enabled list + default) from localStorage
  // via the shared `useCurrentModel` hook. This used to duplicate the read
  // here and in `ModelStatusBar`, and a third time in `SettingsView`'s
  // "后端设置" tab — that divergence is what produced the stale
  // `minimax:MiniMax-M2` badge the user reported. We still keep a local
  // `selectedModel` so the user can override the default per-session
  // without mutating the persisted default.
  //
  // v2 (2026-06-25): 修"模型不见了"bug。
  // 主人反馈：已配置好的模型在 ChatView 弹窗里不显示。
  // 根因：mount 时 useEffect 一次性读 localStorage 后，`config` 状态就被冻结了。
  //       SettingsView 改 defaultModel 时只触发 `MODEL_CONFIG_CHANGED_EVENT`，
  //       `useCurrentModel` 重新读了 defaultModel 字符串，但 useModelSelector
  //       的 config 状态没重读 → 弹窗显示"暂无可用模型"（虽然 selectedModel 不空）。
  // 修法：把"读 localStorage"和"同步 selectedModel"合并到一个事件订阅里。
  //       收到事件就重读完整的 localStorage → 重设 config + selectedModel。
  const { defaultModel } = useCurrentModel();
  // Lazy init: read localStorage synchronously on first render so the
  // model picker is populated immediately, no useEffect round-trip.
  // Fallback chain:
  //   1) hermix.desktop.model_config (the authoritative list)
  //   2) hermix.desktop.model_cache + defaultModel (rebuild from cache
  //      when the persisted config has been wiped — this is the bug we
  //      keep hitting in dev: HMR rewrites wipe state, cache survives)
  //   3) backend /api/model/info (last-resort: ask the Rust server what
  //      model it's actually running with, synthesize a single entry so
  //      the picker isn't permanently empty)
  const [config, setConfig] = useState<ModelConfigData>(() => {
    try {
      const raw = storage.getItem(MODEL_CONFIG_KEY);
      if (raw) {
        const cfg = JSON.parse(raw) as ModelConfigData;
        if (cfg && Array.isArray(cfg.enabled)) {
          if (cfg.enabled.length > 0) return cfg;
          // enabled empty but defaultModel set → synthesize from default
          if (cfg.defaultModel) {
            const parts = cfg.defaultModel.split(":", 2);
            if (parts.length === 2 && parts[0] && parts[1]) {
              return { enabled: [{ providerId: parts[0], modelId: parts[1] }], defaultModel: cfg.defaultModel };
            }
          }
        }
      }
    } catch {}
    // 2) try to rebuild from model_cache
    try {
      const cacheRaw = storage.getItem(STORAGE_KEYS.modelCache);
      const defRaw = storage.getItem(MODEL_CONFIG_KEY);
      const def = defRaw ? ((JSON.parse(defRaw) as ModelConfigData).defaultModel ?? "") : "";
      if (cacheRaw) {
        const cache = JSON.parse(cacheRaw) as Record<string, string[]>;
        const enabled: { providerId: string; modelId: string }[] = [];
        for (const [pid, mids] of Object.entries(cache)) {
          for (const mid of mids || []) enabled.push({ providerId: pid, modelId: mid });
        }
        if (enabled.length > 0) {
          return { enabled, defaultModel: def };
        }
      }
    } catch {}
    return { enabled: [], defaultModel: "" };
  });
  const [selectedModel, setSelectedModel] = useState(() =>
    config.defaultModel || (config.enabled[0] ? `${config.enabled[0].providerId}:${config.enabled[0].modelId}` : "")
  );

  // Single effect: re-read localStorage on mount AND on every model-config
  // change event. This keeps `config.enabled` in lockstep with
  // `SettingsView`'s writes — no stale list, no "暂无可用模型" surprise.
  useEffect(() => {
    const refresh = () => {
      try {
        const raw = storage.getItem(MODEL_CONFIG_KEY);
        if (!raw) return;
        let cfg: ModelConfigData = JSON.parse(raw);
        // Defensive fallback (2026-06-25): if enabled is empty but we
        // have a defaultModel, synthesize a single entry from it so the
        // model picker isn't permanently empty when SettingsView's
        // mount-time fetch fails (backend not ready / network blip).
        if (cfg.enabled.length === 0 && cfg.defaultModel) {
          const parts = cfg.defaultModel.split(":", 2);
          if (parts.length === 2 && parts[0] && parts[1]) {
            cfg = { enabled: [{ providerId: parts[0], modelId: parts[1] }], defaultModel: cfg.defaultModel };
          }
        }
        setConfig(cfg);
        setSelectedModel((prev) => prev || cfg.defaultModel || (cfg.enabled[0] ? `${cfg.enabled[0].providerId}:${cfg.enabled[0].modelId}` : ""));
      } catch {}
    };
    // Don't call refresh() here — the lazy useState init above already
    // reads localStorage synchronously. We only need to listen for
    // SettingsView's later writes (model added/removed/default changed).
    const onChanged = () => refresh();
    window.addEventListener(MODEL_CONFIG_CHANGED_EVENT, onChanged);
    const onStorage = (e: StorageEvent) => {
      if (e.key === MODEL_CONFIG_KEY) refresh();
    };
    window.addEventListener("storage", onStorage);

    // Initial sync from backend. The single source of truth for "which
    // models does this user have access to" is the hermix server's
    // config.yaml — localStorage is just a cache. On first mount (or
    // after a wipe) we ask the backend what model it's running with and
    // seed the cache with that entry. This is NOT a fallback — it's the
    // normal bootstrap path. Without it, the model picker is empty
    // until the user manually opens Settings and clicks "获取模型".
    let cancelled = false;
    if (config.enabled.length === 0) {
      // Fetch default model + all custom provider models from backend.
      // This ensures the model picker shows ALL configured models,
      // not just the active one — fixes "新建对话只有一个模型".
      const defaultModelPromise = fetch(apiUrl("/api/model/info"))
        .then((r) => (r.ok ? r.json() : null))
        .then((info: { model?: string; provider?: string } | null) => {
          if (cancelled || !info?.model || !info?.provider) return null;
          return { providerId: info.provider, modelId: info.model };
        })
        .catch(() => null as { providerId: string; modelId: string } | null);

      const customProvidersPromise = fetch(apiUrl("/api/custom-providers"))
        .then((r) => (r.ok ? r.json() : null))
        .then((providers: { id: string; models: string[] }[] | null) => {
          if (cancelled || !providers) return [] as { providerId: string; modelId: string }[];
          const out: { providerId: string; modelId: string }[] = [];
          for (const p of providers) {
            for (const mid of p.models || []) {
              out.push({ providerId: p.id, modelId: mid });
            }
          }
          return out;
        })
        .catch(() => [] as { providerId: string; modelId: string }[]);

      Promise.all([defaultModelPromise, customProvidersPromise]).then(([def, custom]) => {
        if (cancelled) return;
        const enabled: { providerId: string; modelId: string }[] = [...custom];
        // Add default model if not already in the list
        let defaultId = "";
        if (def) {
          defaultId = `${def.providerId}:${def.modelId}`;
          if (!enabled.some((m) => m.providerId === def!.providerId && m.modelId === def!.modelId)) {
            enabled.push(def);
          }
        }
        // Prefer the default model from config.yaml; if none picks the first available.
        const fallbackId = enabled.length > 0 ? `${enabled[0].providerId}:${enabled[0].modelId}` : "";
        const next: ModelConfigData = { enabled, defaultModel: defaultId || fallbackId };
        storage.setItem(MODEL_CONFIG_KEY, JSON.stringify(next));
        // Also seed the model_cache so availableExtras can show extras
        try {
          const cache: Record<string, string[]> = {};
          for (const m of enabled) {
            if (!cache[m.providerId]) cache[m.providerId] = [];
            if (!cache[m.providerId].includes(m.modelId)) cache[m.providerId].push(m.modelId);
          }
          storage.setItem(STORAGE_KEYS.modelCache, JSON.stringify(cache));
        } catch {}
        setConfig(next);
        setSelectedModel((prev) => prev || defaultId || fallbackId);
        window.dispatchEvent(new CustomEvent(MODEL_CONFIG_CHANGED_EVENT));
      });
    }

    return () => {
      cancelled = true;
      window.removeEventListener(MODEL_CONFIG_CHANGED_EVENT, onChanged);
      window.removeEventListener("storage", onStorage);
    };
  }, []);

  // Defensive: the shared `useCurrentModel` already keeps its own
  // `defaultModel` in sync, but if for any reason the event path above
  // missed a write, fall back to whatever `useCurrentModel` saw.
  // This is a no-op in the common case; only kicks in when the custom
  // event listener hasn't fired yet but the hook has already updated.
  useEffect(() => {
    if (defaultModel) setSelectedModel((prev) => (prev === "" ? defaultModel : prev));
  }, [defaultModel]);

  const models = useMemo(() => {
    return config.enabled.map((m) => ({ id: `${m.providerId}:${m.modelId}`, display: m.modelId, provider: m.providerId }));
  }, [config]);

  // Group models by provider for the picker popover. 2026-06-26:
  //   replaced the previous "builtin / custom" two-section flat list
  //   with a single per-provider collapse tree. Custom providers default
  //   to collapsed so a 5-model `custom-agnes` doesn't push builtin
  //   providers off-screen; builtin providers default to expanded.
  const byProvider = useMemo(() => {
    const map = new Map<string, { provider: string; models: typeof models; isCustom: boolean }>();
    for (const m of models) {
      const isCustom = !BUILTIN_PROVIDER_IDS.has(m.provider);
      const entry = map.get(m.provider);
      if (entry) {
        entry.models.push(m);
      } else {
        map.set(m.provider, { provider: m.provider, models: [m], isCustom });
      }
    }
    return Array.from(map.values());
  }, [models]);

  // v2 (2026-06-25): "已配置的模型列表" 兜底 — 如果 enabled 几乎没有，
  // 但 model_cache 里有用户加过的 provider 的所有模型，则把 cache
  // 里"enabled 没有"的模型作为"其它可用"分组暴露出来，避免用户
  // 配了 provider 但没勾选 enabled 时弹窗一片空。明确标记为"其它"
  // 让用户知道这些没被勾选。
  const availableExtras = useMemo(() => {
    try {
      const raw = storage.getItem(STORAGE_KEYS.modelCache);
      if (!raw) return [] as typeof models;
      const cache = JSON.parse(raw) as Record<string, string[]>;
      const enabledIds = new Set(models.map((m) => m.id));
      const out: typeof models = [];
      for (const [pid, mids] of Object.entries(cache)) {
        for (const mid of mids || []) {
          const id = `${pid}:${mid}`;
          if (enabledIds.has(id)) continue;
          out.push({ id, display: mid, provider: pid });
        }
      }
      return out;
    } catch {
      return [] as typeof models;
    }
  }, [models]);

  const currentDisplay = useMemo(() => {
    // v2 (2026-06-29): 未配置模型时显示明确的中文提示 + 给用户
    // 一个清晰的行动入口（点击 → 跳设置 → 模型管理）。之前显示
    // "选择模型" 用户以为只是个 placeholder，没有"未配置"的紧迫感，
    // 也不知道点哪里去配。
    if (!models || models.length === 0) {
      return "未配置，请先配置模型";
    }
    const found = models.find((m) => m.id === selectedModel);
    return found ? found.display : (selectedModel || "未配置，请先配置模型");
  }, [models, selectedModel]);

  return { models, byProvider, availableExtras, selectedModel, setSelectedModel, currentDisplay };
}

/**
 * Compact banner above the chat list: shows how many messages the
 * Rust side has archived to disk for the current session, and lets
 * the user expand a list of `node:<id>` anchors. Clicking an anchor
 * hands the file path to `pickDirectory` / `read_artifact_file`
 * round-trip — for now we just copy the path to clipboard so the
 * user can paste it into the artifact pane themselves, which is the
 * simplest "no new feature" affordance that satisfies the "show me
 * the archive" intent. A future commit can wire an inline preview
 * directly (open CodeView at the anchor).
 */
function ArchiveRefsBanner({
  refs,
  totalChars,
  open,
  onToggle,
}: {
  refs: ArchiveRef[];
  totalChars: number;
  open: boolean;
  onToggle: () => void;
}) {
  const [copiedId, setCopiedId] = useState<string | null>(null);
  // v3 (2026-06-21): 主人 "继续" → Mermaid Canvas 本地化复刻落地。
  // 复用同一个 banner，list / graph 模式互斥。注意：graph 是手写 SVG，
  // 不是 mermaid 库（不做新依赖，bundle 优先 — 主人 cross-project
  // 偏好："JS gzip 8KB 越小越好"）。
  const [viewMode, setViewMode] = useState<"list" | "graph">("list");
  const handleCopy = async (ref: ArchiveRef) => {
    const anchor =
      ref.lineRange && ref.lineRange[0] === ref.lineRange[1]
        ? `#L${ref.lineRange[0]}`
        : ref.lineRange
          ? `#L${ref.lineRange[0]}-L${ref.lineRange[1]}`
          : "";
    const target = `${ref.filePath}${anchor}`;
    try {
      await navigator.clipboard.writeText(target);
      setCopiedId(ref.nodeId);
      setTimeout(() => setCopiedId((cur) => (cur === ref.nodeId ? null : cur)), 1200);
    } catch {
      // Clipboard API unavailable (rare on desktop Tauri); silently
      // ignore — the user can still read the path from the banner.
    }
  };
  return (
    <div
      className="mx-3 sm:mx-4 mb-2 rounded-lg border"
      style={{
        borderColor: "var(--color-border)",
        background: "var(--color-bg-secondary)",
      }}
    >
      <button
        type="button"
        onClick={onToggle}
        className="w-full flex items-center gap-2 px-3 py-2 text-left"
        style={{ color: "var(--color-text-primary)" }}
        aria-expanded={open}
      >
        <span
          aria-hidden
          style={{
            fontSize: 11,
            transform: open ? "rotate(90deg)" : "rotate(0deg)",
            transition: "transform 120ms ease",
            color: "var(--color-text-secondary)",
          }}
        >
          ▶
        </span>
        <span style={{ fontSize: 12, fontWeight: 600 }}>
          已归档 {refs.length} 条消息
        </span>
        <span style={{ fontSize: 11, color: "var(--color-text-secondary)" }}>
          · 共 {totalChars.toLocaleString("zh-CN")} chars · 点击展开节点
        </span>
        {/* v3 list/graph 切换 tab — 在标题栏右侧 push，onClick stopPropagation
            避免触发外层 onToggle。 */}
        <span
          className="ml-auto flex items-center gap-1"
          onClick={(e) => e.stopPropagation()}
        >
          <button
            type="button"
            onClick={() => setViewMode("list")}
            aria-pressed={viewMode === "list"}
            style={{
              fontSize: 10,
              padding: "1px 6px",
              borderRadius: 4,
              border: "1px solid var(--color-border)",
              color:
                viewMode === "list"
                  ? "var(--color-text-primary)"
                  : "var(--color-text-secondary)",
              background:
                viewMode === "list"
                  ? "var(--color-bg-elevated, rgba(255,255,255,0.06))"
                  : "transparent",
            }}
          >
            列表
          </button>
          <button
            type="button"
            onClick={() => setViewMode("graph")}
            aria-pressed={viewMode === "graph"}
            style={{
              fontSize: 10,
              padding: "1px 6px",
              borderRadius: 4,
              border: "1px solid var(--color-border)",
              color:
                viewMode === "graph"
                  ? "var(--color-text-primary)"
                  : "var(--color-text-secondary)",
              background:
                viewMode === "graph"
                  ? "var(--color-bg-elevated, rgba(255,255,255,0.06))"
                  : "transparent",
            }}
          >
            图谱
          </button>
        </span>
      </button>
      {open && (
        viewMode === "list" ? (
        <ul
          className="px-3 pb-2 max-h-64 overflow-y-auto"
          style={{
            borderTop: "1px solid var(--color-border)",
            fontSize: 11,
            color: "var(--color-text-secondary)",
          }}
        >
          {refs.map((r) => (
            <li
              key={r.nodeId}
              className="flex items-center justify-between gap-2 py-1"
            >
              <span
                className="font-mono truncate flex-1 min-w-0"
                title={`${r.filePath}${
                  r.lineRange
                    ? r.lineRange[0] === r.lineRange[1]
                      ? `#L${r.lineRange[0]}`
                      : `#L${r.lineRange[0]}-L${r.lineRange[1]}`
                    : ""
                }`}
              >
                node:{r.nodeId}
                {r.lineRange && (
                  <span style={{ opacity: 0.7, marginLeft: 6 }}>
                    L{r.lineRange[0]}
                    {r.lineRange[0] !== r.lineRange[1] ? `-L${r.lineRange[1]}` : ""}
                  </span>
                )}
                <span style={{ marginLeft: 8 }}>{r.charCount.toLocaleString("zh-CN")} chars</span>
              </span>
              <button
                type="button"
                onClick={() => handleCopy(r)}
                className="px-2 py-0.5 rounded"
                style={{
                  fontSize: 11,
                  border: "1px solid var(--color-border)",
                  color: copiedId === r.nodeId ? "var(--color-success)" : "var(--color-text-secondary)",
                  background: "transparent",
                }}
              >
                {copiedId === r.nodeId ? "已复制" : "复制路径"}
              </button>
            </li>
          ))}
        </ul>
        ) : (
          <div
            style={{ borderTop: "1px solid var(--color-border)" }}
          >
            <ArchiveRefsGraph refs={refs} />
          </div>
        )
      )}
    </div>
  );
}

// ── ModelStatusBar — top-of-chat status strip ─────────
//
// Surfaces the currently-active default model and whether its provider
// has been configured (env var set). Three states:
//   • green:  configured and matches
//   • amber:  current default uses a provider without a key
//   • red:    no default model at all → "go to Settings"
//
// Without this bar the user only sees the mismatch when they hit
// "send" and get a "missing API key for openai" error, which is too
// late to be actionable.
// ── ModelStatusBar — top-of-chat title strip ───────────
//
// v3 主人反馈：标题要加粗；标题和正文中间不要再有分割线（border-b
// 去掉）。所以现在是干净的纯文字标题区，跟下面 MessageBubble 的
// 第一条消息之间是自然间距，没有任何视觉上的"切割"。
//
// v4 主人反馈：右栏展开按钮只在该对话有产物时才显示，且放在标题栏
// 右侧、不放在主窗口 TitleBar 上（其他页面没有产物，不需要这个按钮）。
function ModelStatusBar({
  title,
  rightOpen,
  onToggleRight,
}: {
  title?: string;
  rightOpen?: boolean;
  onToggleRight?: () => void;
}) {
  if (!title) return null;
  // Truncate long titles to keep the bar compact.
  const displayTitle = title.length > 80 ? title.slice(0, 77) + "…" : title;
  // 对话界面默认就显示右栏按钮，跟有没有产物无关 — 主人原话：
  // "对话界面默认就是有展开的按钮就行了，你管他有没有产物？"
  // 其他页面（agents/experts/settings）没传 onToggleRight，自然不显示。
  const showRightToggle = !!onToggleRight;
  return (
    <div
      className="px-4 sm:px-6 pt-3 pb-2 flex items-center gap-2 rounded-t-xl"
      style={{ color: "var(--color-text-primary)" }}
    >
      <span
        className="truncate flex-1 min-w-0"
        style={{
          fontSize: "14px",
          fontWeight: 600,
          letterSpacing: "-0.005em",
        }}
      >
        {displayTitle}
      </span>
      {showRightToggle && (
        <button
          onClick={onToggleRight}
          className="titlebar-btn"
          title={rightOpen ? "关闭产物栏" : "打开产物栏"}
          aria-label="切换产物栏"
          data-right-open={rightOpen ? "true" : "false"}
        >
          <PanelRight size={14} />
        </button>
      )}
    </div>
  );
}

interface ChatViewProps {
  session: Session | null;
  onSendMessage: (content: string, projectId?: string, overrides?: ChatOverrides, attachments?: { images?: QueuedImage[]; attachedFilePaths?: string[] }) => Promise<void>;
  onNewChat?: () => void;
  /**
   * v2 (2026-07-07): inline todo cards in the message stream. App.tsx
   * owns the groups store; ChatView renders one card per plan right
   * after the originating assistant message (or at the end of the
   * stream when the server did not stamp a message_id).
   */
  todoGroups?: Array<{
    groupId: string;
    groupTitle: string | null;
    anchorMessageId: string | null;
    itemsById: Record<string, TodoSnapshotItem>;
  }>;
  /**
   * Called when the user clicks a checkbox in a TodoListCard. App.tsx
   * is responsible for routing through the backend HTTP layer so the
   * WS forwarder fires and every connected client stays in sync.
   */
  onTodoPatch?: (id: string, patch: { status?: string; title?: string; priority?: string }) => Promise<unknown>;
  /**
   * Shared ref for optimistic todo toggles (2026-07-08).
   * Passed through to TodoListCard; checked in App.tsx's onTodoChange
   * handler to skip stale WS echoes during rapid-toggle races.
   */
  justPatchedTodoRef?: React.MutableRefObject<Set<string>>;
  /**
   * @deprecated The actively-streaming assistant message is routed to
   * `<StreamingBubble />` from inside the message loop (see the body
   * below). The parent no longer needs to thread the streamed text
   * through this prop — it is kept here only so the existing
   * `App.tsx` call site still type-checks. Safe to remove once
   * `App.tsx` is updated to drop the `streamingText=""` literal.
   */
  streamingText?: string;
  isStreaming: boolean;
  environmentLabel?: string;
  isSandbox?: boolean;
  branch?: string;
  usagePct?: number;
  quotaResetHint?: string;
  onUpgrade?: () => void;
  onOpenSessionSettings?: () => void;
  onShareSession?: () => void;
  onOpenExperts?: () => void;
  /** 跳转到「设置」页（模型未配置时点击 chip 用） */
  onOpenSettings?: () => void;
  /** 2026-07-08 (user): 已选工作区时点目录 → 打开右栏文件浏览器切到该目录 */
  onOpenComputer?: () => void;
  onUndo?: () => void;
  onRestore?: () => void;
  /** 停止当前流式生成 */
  onStop?: () => void;
  /** Absolute path used to anchor relative paths the LLM mentions */
  workspaceRoot?: string | null;
  onWorkspaceChange?: (sessionId: string | null | undefined, path: string | null) => void;
  /** Whether the right (artifacts) sidebar is currently open. */
  rightOpen?: boolean;
  /** Toggle the right sidebar. Presence (not the artifact count)
   *  controls whether the toggle button is rendered in the chat header. */
  onToggleRight?: () => void;
  /** Programmatic file open from inside a markdown body. The path
   *  is the absolute (or workspace-relative) file string the user
   *  clicked in an assistant message; App.tsx will add an artifact
   *  record if needed, open the sidebar, and switch it into preview
   *  mode. */
  onPathClick?: (path: string) => void;
  /** Approve a pending approval card — wires to the inline card
   *  rendered by `MessageBubble` (role="approval"). The card's "允许"
   *  button calls this. */
  onApproveApproval?: (requestId: string) => void;
  /** Deny a pending approval card — same wiring as approve. */
  onDenyApproval?: (requestId: string) => void;
  /** Answer a pending clarify card — wires to the inline card
   *  rendered by `MessageBubble` (role="clarify"). */
  onAnswerClarify?: (requestId: string, answer: string) => void;
  /**
   * Error message from the most recent failed attempt to load this
   * session's messages from the backend. When set, takes precedence
   * over the empty-state welcome page so the user knows what went
   * wrong (and isn't just staring at the mascot for hours wondering
   * why their conversation never came back). A "重试" button is
   * rendered alongside.
   */
  messageLoadError?: string | null;
  /** Re-trigger the session-message fetch. Wired to the "重试" button. */
  onRetryLoad?: () => void;
  /**
   * When non-null, the previous send's WebSocket errored before any
   * chunk landed on the optimistic assistant placeholder. We need to
   * surface the error inline so the user sees something — otherwise
   * the optimistic empty assistant row is filtered out by the
   * `isRowRenderable` filter and the chat just sits there silent.
   */
  streamError?: string | null;
  /** Reset the stream error in the parent after the user has seen it. */
  onClearStreamError?: () => void;
  /**
   * 2026-06-25 主人反馈："欢迎页默认是关闭产物栏的"。
   * 进入欢迎态（`!session || session.messages.length === 0`）时通知父级
   * 关闭右栏。仅在 false → true 切换时触发：用户在欢迎页手动开右栏不
   * 会被反复掐。该回调依赖 `props.onEnterWelcome` 引用稳定性 — 用
   * useCallback 包裹保证不会因为父级重渲染而抖动。
   */
  onEnterWelcome?: () => void;
  /** M5: ACUI card state from useAcuiState — drives <AcuiPanel />. */
  acuiState?: AcuiState;
  /** M5: ACUI decision bus, wired to App.tsx approval/clarify handlers. */
  acuiBus?: AcuiDecisionBus;
  /** M5: Live acui stats for the panel header / counters. */
  acuiStats?: {
    total: number;
    resolved: number;
    pending: number;
    decisionCount: number;
  };
  /** H3: Latest prefetch intent data from the WS stream. When present
   *  render a PrefetchIntentChip below the streaming message. */
  prefetchIntent?: {
    intent: string;
    confidence: number;
    results: Array<{
      tool: string;
      args: unknown;
      output: string;
      errored: boolean;
      durationMs: number;
    }>;
    } | null;
  /** Current stream phase from useStreamChat — drives the status
   *  chip on the streaming assistant message so the user sees
   *  "正在生成…" → "响应较慢…" → "已完成" (or "超时")
   *  instead of the old binary isStreaming boolean. */
  streamingPhase?: StreamPhase | null;
  /** Transient status text from backend (e.g. "Still waiting for the model…"). */
  streamingStatus?: string | null;
/** Compaction pipeline status from backend's `compaction_status` WS event. */
compactionStatus?: CompactionStatus | null;
/** Context window usage from backend's `context_usage` WS event. */
contextUsage?: { usedTokens: number; maxTokens: number; thresholdTokens: number; percent: number } | null;
/** 2026-07-14: per-turn token usage snapshot from the backend's
 *  `usage_update` WS event. Drives the live cache-hit chip next
 *  to the context-usage bar. */
usageSnapshot?: {
  promptTokens: number;
  completionTokens: number;
  cacheReadTokens: number;
  cacheCreationTokens: number;
  reasoningTokens: number;
} | null;
  /** Number of `role="tool"` messages currently in `running` state
   *  in the active session. Drives the post-stream "执行工具中…"
   *  indicator so the user doesn't see a blank gap between the
   *  LLM settling (`done`) and the next agent-loop turn beginning. */
  pendingToolsCount?: number;
  /** Message queue for mid-stream input */
  messageQueue?: QueuedMessage[];
  /** Add a message to the queue */
  onAddToQueue?: (msg: QueuedMessage) => void;
  /** Clear the message queue */
  onClearQueue?: () => void;
  /** Remove a single message at the given index */
  onRemoveFromQueue?: (idx: number) => void;
  /** Replace the text at the given index */
  onEditQueueItem?: (idx: number, newText: string) => void;
  /** Send the message at the given index immediately (out of turn) */
  onSendFromQueue?: (idx: number) => void;
  /** External file path to attach to the chat input (e.g. from right-sidebar
   *  file tree right-click “加入对话”). When set, ChatView adds it to the
   *  pending filePaths and calls onConsumeFileAttachment to clear it. */
  pendingFileAttachment?: string | null;
  /** Called after pendingFileAttachment is consumed so the parent can clear it. */
  onConsumeFileAttachment?: () => void;
}

export function ChatView({
  session,
  onSendMessage,
  onNewChat,
  streamingText: _legacyStreamingText,
  streamingPhase,
  isStreaming,
  onOpenExperts,
  onOpenSettings,
  onOpenComputer,
  onUndo,
  onRestore,
  onStop,
  workspaceRoot,
  onWorkspaceChange,
  rightOpen,
  onToggleRight,
  todoGroups, onTodoPatch, justPatchedTodoRef,
  onApproveApproval,
  onDenyApproval,
  onPathClick,
  onAnswerClarify,
  messageLoadError,
  onRetryLoad,
  streamError,
  onClearStreamError,
  prefetchIntent,
  acuiState,
  acuiBus,
  messageQueue,
  onAddToQueue,
  onClearQueue,
  onRemoveFromQueue,
  onEditQueueItem,
  onSendFromQueue,
  pendingFileAttachment,
  onConsumeFileAttachment,
  pendingToolsCount = 0,
  streamingStatus,
  compactionStatus = null,
contextUsage = null,
  usageSnapshot = null,
  onEnterWelcome,
}: ChatViewProps) {
  const [input, setInput] = useState("");
  const [sending, setSending] = useState(false);
  const messagesEndRef = useRef<HTMLDivElement>(null);
  // 2026-07-06: ref to the scrollable message container for
  // user-scroll detection. Only auto-scroll if the user is near
  // the bottom, so browsing history doesn't get interrupted by
  // new streaming chunks.
  const scrollContainerRef = useRef<HTMLDivElement>(null);
  const isNearBottom = useCallback(() => {
    const el = scrollContainerRef.current;
    if (!el) return true; // container not mounted → scroll anyway
    const threshold = 150; // px from bottom
    return el.scrollHeight - el.scrollTop - el.clientHeight < threshold;
  }, []);

  /**
   * Post-stream activity heuristic. The agent loop runs many turns
   * inside a single user→assistant round trip; between turns, the
   * LLM stream settles (`isStreaming=false`) but the agent loop is
   * still working (parsing tools, executing them, deciding whether
   * to call LLM again). Without an indicator, that gap reads as
   * "Hermix stopped responding" — confusing when the next text
   * chunk shows up 200ms later.
   *
   * Heuristic: when `isStreaming` transitions from true → false and
   * there are pending tool calls OR an unresolved LLM call, show
   * "AI 正在分析 / 执行工具中…" for up to 4s. The indicator hides
   * when (a) a new stream begins, (b) pending tools go to zero and
   * no new stream started within the window, or (c) the user sends
   * a new message (covered by `sending`).
   */
  const [postStreamActivity, setPostStreamActivity] = useState(false);
  const prevStreamingRef = useRef(isStreaming);
  useEffect(() => {
    const wasStreaming = prevStreamingRef.current;
    prevStreamingRef.current = isStreaming;
    // Only arm on the streaming→settled edge, not on the very first
    // mount (when `wasStreaming=false` and stays false).
    if (wasStreaming && !isStreaming) {
      setPostStreamActivity(true);
    } else if (isStreaming) {
      // New stream began → hide the gap indicator immediately.
      setPostStreamActivity(false);
    }
  }, [isStreaming]);

  /**
   * Auto-hide the gap indicator when tools have settled AND no new
   * streaming session arrived within the 4s budget. Most agent
   * turns resolve in <500ms; the 4s ceiling covers slow tool runs
   * (search_files / patch) without leaving the user staring at
   * "AI 正在分析" forever after the loop genuinely ended.
   */
  useEffect(() => {
    if (!postStreamActivity) return;
    const t = setTimeout(() => setPostStreamActivity(false), 4000);
    return () => clearTimeout(t);
  }, [postStreamActivity, pendingToolsCount]);

  /**
   * Hide the gap indicator immediately if pending tools drop to 0
   * AND no new streaming session began. We give the agent loop a
   * 600ms grace window so a fast "tool → next turn" sequence
   * doesn't flash the "已完成" pill before the next StreamingBubble
   * mounts.
   */
  useEffect(() => {
    if (!postStreamActivity) return;
    if (pendingToolsCount !== 0 || isStreaming) return;
    const t = setTimeout(() => setPostStreamActivity(false), 600);
    return () => clearTimeout(t);
  }, [postStreamActivity, pendingToolsCount, isStreaming]);
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  // ── Expert / permission / workspace / skills state ─────────────────
  const sessionId = session?.id ?? null;
  const { expert, setExpert } = useSessionExpert(sessionId);
  // PLAN_UI_ENHANCEMENT §1.1 — 专家切换分界。
  // 每次 setExpert 都触发 recordCutoff：之后的 AssistantMessage 拿这个时间戳做"新旧分界"。
  const { cutoffAt: expertCutoffAt, recordCutoff } = useExpertCutoff(sessionId);
  const setExpertWithCutoff = (id: string | null) => {
    setExpert(id);
    if (id) markRecentUsed(id);
    recordCutoff(id);
    if (sessionId) {
      updateSessionMetadata(sessionId, { expert_id: id }).catch(() => {});
    }
  };

  // ── Archived context banner (v2 short-term unload, 2026-06-21) ───
  //
  // When a session's context crosses the 80% pressure threshold, the
  // Rust side archives the middle of the conversation to disk and
  // appends one JSON-Lines row per archived message to
  // `<hermix_home>/sessions/<sid>/archive-index.jsonl`. We fetch
  // that index once when the session changes and render a compact
  // banner above the messages — clicking an anchor opens the
  // archive file at that exact node in CodeView, which the user can
  // then hand back to the agent if they want to revisit the
  // archived turn.
  //
  // Empty array → no banner (the common case for short sessions).
  // `nodeCount` is derived from `refs.length` so the headline stays
  // accurate even after a manual `read_artifact_file` deletion
  // cleans some rows up on disk.
  const [archiveRefs, setArchiveRefs] = useState<ArchiveRef[]>([]);
  const [archiveOpen, setArchiveOpen] = useState(false);
  useEffect(() => {
    if (!sessionId) {
      setArchiveRefs([]);
      setArchiveOpen(false);
      return;
    }
    let cancelled = false;
    getSessionArchiveRefs(sessionId).then((refs) => {
      // Stale-response guard: a fast session-switch could let the
      // older fetch resolve last and clobber the newer banner. The
      // flag is set when *this* effect is torn down.
      if (!cancelled) setArchiveRefs(refs);
    });
    return () => {
      cancelled = true;
    };
  }, [sessionId]);

  // T2026-06-21 P1: dynamic tool discovery. We pull the master list
  // from `GET /v1/tools` on mount and refresh it whenever the window
  // regains focus — the registry is the single source of truth, so
  // new tools show up without any front-end change. The cache in
  // `api.ts` already throttles to 30s, so focus re-fires are cheap.
  const [availableTools, setAvailableTools] = useState<ToolDescriptor[]>([]);
  useEffect(() => {
    let cancelled = false;
    const load = () => {
      getAvailableTools().then((tools) => {
        if (!cancelled) setAvailableTools(tools);
      });
    };
    load();
    window.addEventListener("focus", load);
    return () => {
      cancelled = true;
      window.removeEventListener("focus", load);
    };
  }, []);
  const archiveTotalChars = useMemo(
    () => archiveRefs.reduce((sum, r) => sum + (r.charCount || 0), 0),
    [archiveRefs],
  );
  const [permissionLevel, setPermissionLevel] = usePermissionLevel();
  const { workspace, setWorkspace: setWorkspaceRaw } = useSessionWorkspace(sessionId);
  // 2026-06-26 主人反馈：「目录进入对话界面后就无法设置了，在进入前没有设置过」。
  //
  // v1 根因：原 useMemo 靠 localStorage 读 `hermix.session.{id}.workspace`，但
  //   useSessionWorkspace 的 useState 初始化时机 vs useEffect 同步 vs session.workspace
  //   回填（line 1119-1133）三个时序不一致，useMemo 重算时机不可控。
  // v1 修法：直接用 React state `workspace` 作为唯一真相源，与 line 1759 的
  //   `<Composer workspaceLocked>` prop 共享同一判定 `Boolean(workspace)`，
  //   保证 setWorkspace 包装层与 Composer 渲染走同一锁。v1 只解决了"state
  //   来源不统一"的问题，没有解决"锁了之后如何解锁"。
  //
  // v2 二次根因：v1 的"workspaceLocked = Boolean(workspace)"在以下场景会让
  //   按钮看起来"没设过但又锁着"——chip 文字是 activeWorkspace.name || "目录"
  //   占位符，workspaceLocked 却 = true（workspace state 是 non-null 但
  //   activeWorkspace=null 找不到匹配条目）。可能来源：
  //     (a) 上次 session 留下的 `hermix.session.<id>.workspace` 还在
  //         localStorage，新 sessionId 命中旧值；
  //     (b) backend session.workspace 被旧测试 set 过，line 1119-1133 的
  //         restore effect 把它写回了 state；
  //     (c) 欢迎态下 useSessionWorkspace 读 DRAFT_WORKSPACE_KEY，draft 有值。
  //   用户视觉上看到"目录"占位符 → 以为没设过 → 点不动 → 「进入对话界面后
  //   就无法设置了」的体感。
  //   而且就算去掉 button 的 disabled，v1 的 setWorkspace 包装器也会
  //   `if (workspaceLocked) return;` 静默拒绝 ——popover 里点"不绑定目录"无效。
  //
  // v3 修法（2026-07-08 最终版）: 取消"按 session 锁死"和"按已选锁死"。
  //   欢迎界面无论是否选过工作区，按钮都可点 → 弹 popover 列表
  //   （可重新选择/不绑定）。只有「对话已有消息 / 发送中 / 流式」时
  //   才锁定——即 Msg1+ 之后不可再改工作区。
  //   "添加新工作区"选完后 popover 自动关闭（setProjOpen(false)）。
  //
  // 2026-07-07 workspaceLocked + onWorkspaceChange
  const workspaceLocked =
    (session?.messages?.length ?? 0) > 0 || sending || isStreaming;

  const setWorkspace = useCallback(
    (path: string | null) => {
      if (workspaceLocked) return;
      setWorkspaceRaw(path);
      if (sessionId) {
        updateSessionMetadata(sessionId, { workspace: path }).catch(() => {});
      }
      onWorkspaceChange?.(sessionId, path);
    },
    [setWorkspaceRaw, sessionId, workspaceLocked, onWorkspaceChange],
  );
  // 欢迎态检测：会话刚切到空 → 通知父级关右栏。仅在 false→true 切换
  // 时触发，避免每次 ChatView 自身重渲染都重复关。用 useRef 持有上一
  // 帧的 isWelcome 状态。session 引用每次 render 都可能换，但只要
  // messages 长度和存在性不变就不触发 effect — 通过 ref 比较即可。
  // lastIsWelcomeRef.current === null 表示"还没观察过"，mount 时第一
  // 次进入欢迎页也视为一次切换（null → true），符合"欢迎页默认关
  // 产物栏"的语义。
  const lastIsWelcomeRef = useRef<boolean | null>(null);
  useEffect(() => {
    const isWelcome = !session || session.messages.length === 0;
    if (isWelcome && lastIsWelcomeRef.current !== true) {
      onEnterWelcome?.();
    }
    lastIsWelcomeRef.current = isWelcome;
  }, [session, onEnterWelcome]);
  const { skills: activeSkills } = useSessionSkills(sessionId);
  const { experts, categories, byCategory } = useExpertCatalog();
  const { recent: recentExperts, markUsed: markRecentUsed } = useRecentExperts();
  const { workspaces, add: addWorkspace, remove: removeWorkspace } = useWorkspaces();
  const activeWorkspace = useMemo(
    () => (workspace ? workspaces.find((w) => w.path === workspace) ?? null : null),
    [workspace, workspaces],
  );
  // 2026-06-25 主人反馈："进历史对话，目录不显示了"。
  // 根因：useSessionWorkspace 读到的 path 在 useWorkspaces 全局列表里没记录
  // （可能是 hermes→hermix 升级时 session key 走了 legacy fallback，
  // 或者之前是别的入口写入）。修法：发现这种孤儿 path 时把它自动写回
  // 全局列表（名字取路径最后一段），保持 chip 显示目录名而不是占位符"目录"。
  // 只在 mount/重 mount 触发一次，避免每次渲染都 add（add 会写 localStorage）。
  const reconciledRef = useRef<string | null>(null);
  useEffect(() => {
    if (!workspace) { reconciledRef.current = null; return; }
    if (workspaces.find((w) => w.path === workspace)) return;
    if (reconciledRef.current === workspace) return;
    reconciledRef.current = workspace;
    addWorkspace("", workspace);
  }, [workspace, workspaces, addWorkspace]);

const { models, byProvider, selectedModel, setSelectedModel, currentDisplay } = useModelSelector();

// 2026-06-25 包装 setSelectedModel：用户切换模型后也持久化到后端 session。
const setSelectedModelAndPersist = useCallback(
  (id: string) => {
    setSelectedModel(id);
    if (sessionId) {
      updateSessionMetadata(sessionId, { model: id }).catch(() => {});
    }
  },
  [setSelectedModel, sessionId],
);

// 2026-06-25 主人反馈："进历史对话，目录/模型/默认模型都消失"。
// 根因：SessionSummary 现在带 workspace/expert_id/model 字段，但 ChatView
//       加载 session 时没把这些值写回 localStorage，导致 useSessionWorkspace /
//       useSessionExpert / useModelSelector 读到的还是上一次的（或空的）状态。
// 修法：session 变化时，把后端返回的值同步到 localStorage。
//       用 useRef 防重入：同一 sessionId 只恢复一次，避免每次 render 都写盘。
const restoredRef = useRef<string | null>(null);
useEffect(() => {
  if (!session?.id) { restoredRef.current = null; return; }
  if (restoredRef.current === session.id) return;
  restoredRef.current = session.id;

  if (session.workspace) {
    setWorkspaceRaw(session.workspace);
  }
  // Always sync expert from backend — even when expert_id is null,
  // we must clear the stale DRAFT_EXPERT_KEY from the previous session.
  setExpert(session.expert_id ?? null);
  if (session.model) {
    setSelectedModel(session.model);
  }
// v2 (2026-06-29): 主人反馈「对话中 agent 不看工作区目录」。
// 同时监听 session.workspace 字段本身变化（user 在其他 tab 选了目录 →
// 后端 PATCH 改了 session.workspace → 但 React state 不会自动更新），
// 把后端最新值拉回前端 state。否则会出现"后端显示绑了目录，但前端
// 渲染没绑、handleSubmit 也不传 overrides.workspace"的撕裂。
}, [session?.id, session?.workspace, session?.expert_id, session?.model, setWorkspaceRaw, setExpert, setSelectedModel]);

const [pendingImages, setPendingImages] = useState<PendingImage[]>([]);
const [filePaths, setFilePaths] = useState<string[]>([]);

// P0-9: Consume external file attachment from right-sidebar “加入对话”.
// When the parent sets `pendingFileAttachment`, add it to `filePaths`
// (which renders as a chip above the input) and clear the parent state.
useEffect(() => {
if (pendingFileAttachment) {
setFilePaths((prev) => prev.includes(pendingFileAttachment) ? prev : [...prev, pendingFileAttachment]);
onConsumeFileAttachment?.();
}
}, [pendingFileAttachment, onConsumeFileAttachment]);

/**
 * 防 WebView2 内存泄漏：组件卸载时清理所有 blob URL。
 * pendingImages 变化时不能再 revoke 当前 state 里的（用户可能还在看预览），
 * 所以只在 unmount 时统一清。
 */
useEffect(() => {
  return () => {
    setPendingImages((prev) => {
      prev.forEach((img) => URL.revokeObjectURL(img.blobUrl));
      return [];
    });
  };
}, []);

/**
 * 用户在"保持原画质"开关关闭时走自动压缩，开关打开时跳过压缩。
 */
const [keepOriginalImage, setKeepOriginalImage] = useState(false);

/**
 * 多图上传入口。异步并行压缩，每张图独立 blob URL。
 * File 句柄直接进 state（几 KB），不进 base64 字符串（8MB）。
 */
const handleFilesAttach = useCallback(
  async (files: File[]) => {
    // 分离图片和非图片文件
    const images: File[] = [];
    const nonImages: File[] = [];
    for (const f of files) {
      if (f.type.startsWith("image/")) images.push(f);
      else nonImages.push(f);
    }

    // 图片走压缩 + blobUrl 预览（保持现有逻辑）
    if (images.length > 0) {
      const results: CompressResult[] = await Promise.all(
        images.map((f) => (keepOriginalImage ? skipCompression(f) : compressImage(f))),
      );
      const newImages: PendingImage[] = results.map((r) => ({
        file: r.file,
        blobUrl: URL.createObjectURL(r.file),
        sizeBytes: r.compressedSize,
        mimeType: r.file.type,
        isCompressed: r.isCompressed,
        method: r.method,
        name: r.file.name,
      }));
      setPendingImages((prev) => [...prev, ...newImages]);
    }

    // 非图片文件：通过 Tauri IPC 获取本地路径
    if (nonImages.length > 0) {
      const paths: string[] = [];
      for (const f of nonImages) {
        // Tauri WebView2 暴露 file.path 属性（非标准 Chromium API）
        const filePath = (f as any).path;
        if (filePath && typeof filePath === "string") {
          paths.push(filePath);
        }
      }
      if (paths.length > 0) {
        setFilePaths((prev) => [...prev, ...paths]);
      }
    }
  },
  [keepOriginalImage],
);

const removePendingImage = useCallback((index: number) => {
  setPendingImages((prev) => {
    const target = prev[index];
    if (target) URL.revokeObjectURL(target.blobUrl);
    return prev.filter((_, i) => i !== index);
  });
}, []);

// Clears the input-box attachment previews. Unlike `removePendingImage`
// (which revokes a single blob the user explicitly dropped), this does
// NOT revoke the blob URLs: the send path transfers ownership of the
// previews to the user message bubble (ChatMessage.images) so the same
// image the user saw in the input box appears in the sent bubble. Blob
// URLs are in-memory only and are released when the document unloads.
const clearPendingImages = useCallback(() => {
  setPendingImages([]);
}, []);

  /**
   * Defensive filter for empty placeholders.
   *
   * Background: `handleSendMessage` optimistically appends a
   * `content: ""` assistant placeholder to the session so chunks can
   * land on it (keyed by `assistantId`). If the stream gets cancelled
   * before any chunk arrives, or if the WS drops mid-flight, that
   * placeholder stays in the array.
   *
   * What we drop:
   *  - `role: "user"` with `content: ""` (legacy rows / never-written).
   *  - `role: "assistant"` with no content / no tool_calls / no
   *    execution_timeline AND no `stopReason` (i.e. true placeholders).
   *    Interrupted / truncated messages get to keep their chip even
   *    if empty so the user can see "上次中断了" / "已截断" on reload.
   *
   * The merge function (`mergeConsecutiveAssistantMessages`) is
   * *supposed* to filter the assistant case but only checks
   * `content.trim().length`, `tool_calls.length`, and
   * `execution_timeline.length` — so a pre-filter here is the
   * defensive net.
   */
  function isRowRenderable(m: ChatMessage): boolean {
    if (m.role === "user") {
      return typeof m.content === "string" && m.content.trim().length > 0;
    }
    if (m.role !== "assistant") return true;
    if (m.stopReason && m.stopReason.length > 0) return true;
    const hasContent =
      typeof m.content === "string" && m.content.trim().length > 0;
    const hasToolCalls =
      Array.isArray(m.tool_calls) && m.tool_calls.length > 0;
    const hasTimeline =
      Array.isArray(m.execution_timeline) && m.execution_timeline.length > 0;
    // 2026-07-11: Also check reasoningContent — when the LLM doesn't
    // close its <think> tag, the body text is routed to
    // reasoning_content and content is empty. Without this check,
    // such messages are silently dropped on reload, causing the
    // "重启后正文消失" bug.
    const hasReasoning =
      typeof m.reasoningContent === "string" && m.reasoningContent.trim().length > 0;
    return hasContent || hasToolCalls || hasTimeline || hasReasoning;
  }

  // Stream-error banner (rendered above the input) auto-clears when
  // the user sends a new message (handleSubmit → onClearStreamError)
  // or clicks the dismiss (X) button.  The parent also resets
  // streamError on session change.

  // P0-3: Ref-based auto-scroll. Previously the effect depended on
  // [session?.messages] which fires 30+ times per second during
  // streaming. Each firing creates a new closure, schedules rAF,
  // and tears down the previous effect. The rAF coalescing handled
  // the scroll frequency, but the closure churn was wasteful.
  //
  // New approach: a ref tracks "scroll pending", and a single rAF
  // loop runs for the lifetime of the streaming session. Non-streaming
  // message additions use a separate lightweight effect.
  const scrollPendingRef = useRef(false);

  useEffect(() => {
    scrollPendingRef.current = true;
  }, [session?.messages]);

  useEffect(() => {
    if (!isStreaming) return;
    let raf = 0;
    const tick = () => {
      if (scrollPendingRef.current) {
        scrollPendingRef.current = false;
        const node = messagesEndRef.current;
        if (node && isNearBottom()) {
          node.scrollIntoView({ block: "end" });
        }
      }
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    return () => {
      cancelAnimationFrame(raf);
    };
  }, [isStreaming, isNearBottom]);

  // 2026-07-12: Restore-conversation auto-scroll.
  // On initial mount / F5 reload / session switch, the recovered
  // history must land with the viewport scrolled to the last message.
  // The pre-existing effect relied on `isNearBottom()`, but a freshly
  // mounted container has `scrollTop = 0` and `messagesEndRef.current`
  // is null until React commits — so the call fired before the DOM
  // existed and the scroll never happened. Two changes:
  //   (a) `justRestoredRef` latches true on every session id change
  //       and forces a single unconditional scroll on the next message
  //       count change.
  //   (b) `hasUserScrolledUpRef` (driven by `onScroll` below) blocks
  //       the force-scroll once the user has navigated into history,
  //       so we don't yank them back to the bottom on every new chunk.
  const prevSessionIdRef = useRef<string | undefined>(undefined);
  const justRestoredRef = useRef(false);
  const hasUserScrolledUpRef = useRef(false);

  useEffect(() => {
    const currentId = session?.id;
    if (prevSessionIdRef.current === currentId) return;
    prevSessionIdRef.current = currentId;
    hasUserScrolledUpRef.current = false;
    justRestoredRef.current = true;
  }, [session?.id]);

  const handleContainerScroll = useCallback(() => {
    // After a programmatic scrollIntoView, isNearBottom() is true and
    // this flag is cleared naturally on the same onScroll tick — no
    // need to suppress the event.
    hasUserScrolledUpRef.current = !isNearBottom();
  }, [isNearBottom]);

  // Non-streaming: scroll on message count change (new message, session switch).
  // Three branches:
  //   1. `justRestoredRef` true (initial mount / reload / session switch):
  //      unconditional force-scroll, ignore user's prior scroll position
  //      because there is no prior session in this container.
  //   2. User has scrolled up to browse history: stay put, do not disturb.
  //   3. Otherwise (user is still near bottom): follow the new content.
  useEffect(() => {
    if (isStreaming) return;
    const node = messagesEndRef.current;
    if (!node) return;
    if (!justRestoredRef.current && hasUserScrolledUpRef.current) return;

    // Double rAF: first frame lets React commit the message DOM, second
    // frame lets images / web fonts settle so `scrollHeight` is final.
    // Without this, restoring a long history lands at the wrong offset
    // because the container is still growing as child rows mount.
    requestAnimationFrame(() => {
      requestAnimationFrame(() => {
        node.scrollIntoView({ block: "end" });
        justRestoredRef.current = false;
      });
    });
  }, [session?.messages?.length, isStreaming, isNearBottom]);

  useEffect(() => {
    if (textareaRef.current) {
      textareaRef.current.style.height = "auto";
      textareaRef.current.style.height = Math.min(textareaRef.current.scrollHeight, 200) + "px";
    }
  }, [input]);

  // ── Streaming row detection & message pipeline ──────────────
  // MUST be above all early returns to comply with React Hooks rules.
  // When session is null or messages is empty, the memo safely
  // returns [] — the value is simply unused by the early-return paths.
  const streamingEntry =
    isStreaming && session
      ? (() => {
          for (let i = session.messages.length - 1; i >= 0; i--) {
            const m = session.messages[i];
            if (m.role === "assistant") {
              return { id: m.id, msg: m };
            }
          }
          return null;
        })()
      : null;
  const streamingAssistantId = streamingEntry?.id ?? null;
  const streamingMsg = streamingEntry?.msg ?? null;
  // P0-2: Wrap the entire message-processing pipeline in useMemo so
  // it only recomputes when the messages array or streaming state
  // actually changes. Previously this ran on every render — including
  // every streaming chunk — even though the intermediate values
  // (filter, merge, splice) are O(n) and produce the same result
  // when only unrelated state (e.g. input text) changed.
  const messagesWithStream = useMemo<ChatMessage[]>(() => {
    const nonStreamingRaw = streamingAssistantId
      ? session?.messages.filter((m) => m.id !== streamingAssistantId) ?? []
      : session?.messages ?? [];
    const visibleNonStreaming = nonStreamingRaw.filter(isRowRenderable);
    const restMsgs = mergeConsecutiveAssistantMessages(visibleNonStreaming);
    if (!streamingAssistantId || !streamingMsg) return restMsgs;
    const rawIdx = session?.messages.findIndex((m) => m.id === streamingAssistantId) ?? -1;
    if (rawIdx < 0) return [...restMsgs, streamingMsg];
    const restIds = new Set(restMsgs.map((m) => m.id));
    let insertIdx = 0;
    for (let i = 0; i < rawIdx; i++) {
      if (restIds.has(session!.messages[i].id)) insertIdx++;
    }
    const result = [...restMsgs];
    result.splice(insertIdx, 0, streamingMsg);
    return result;
  }, [session?.messages, streamingAssistantId, streamingMsg]);

  // P0-T1 (Frontend Audit 2026-07-13): anchor every TodoListCard to
  // its originating assistant message. The server currently
  // hard-codes `originating_message_id = None` (see
  // hermix-server/src/lib.rs todo_event listener) so a freshly-
  // created plan group arrives without an anchor. We fall back to
  // "earliest item.created_at ⇒ the latest assistant message
  // whose timestamp ≤ that" which is good enough in practice: the
  // group_started event fires during the same streaming turn that
  // produced the assistant message, so the timing is tight.
  //
  // Once the server threads real message_id through (future patch),
  // anchorMessageId will be non-null and the explicit lookup below
  // wins. Both paths share the same downstream renderer, so the
  // transition is invisible to users.
  const streamRow = useMemo<
    Array<
      | { kind: "message"; msg: ChatMessage; role: ChatMessage["role"] }
      | { kind: "todo"; group: NonNullable<typeof todoGroups>[number]; role: undefined }
    >
  >(() => {
    const rows: Array<
      | { kind: "message"; msg: ChatMessage; role: ChatMessage["role"] }
      | { kind: "todo"; group: NonNullable<typeof todoGroups>[number]; role: undefined }
    > = messagesWithStream.map((msg) => ({ kind: "message", msg, role: msg.role } as
      | { kind: "message"; msg: ChatMessage; role: ChatMessage["role"] }
      | { kind: "todo"; group: NonNullable<typeof todoGroups>[number]; role: undefined }
    ));

    const groups = todoGroups ?? [];
    if (groups.length === 0) return rows;

    // Pre-compute assistant message index for anchor lookup.
    const assistantIdxById = new Map<string, number>();
    messagesWithStream.forEach((m, i) => {
      if (m.role === "assistant") assistantIdxById.set(m.id, i);
    });

    type Pending = { group: NonNullable<typeof todoGroups>[number]; insertAfter: number };
    const anchored: Pending[] = [];
    const unanchored: NonNullable<typeof todoGroups>[number][] = [];

    for (const g of groups) {
      if (g.anchorMessageId && assistantIdxById.has(g.anchorMessageId)) {
        anchored.push({ group: g, insertAfter: assistantIdxById.get(g.anchorMessageId)! });
        continue;
      }
      // Fallback: anchor to the latest assistant message whose
      // timestamp ≤ the earliest item's created_at. If no assistant
      // message is older than the first item (e.g. todos created
      // during the user-message turn before any assistant reply),
      // the group falls through to the unanchored bucket and
      // appends to the end of the stream.
      const items = Object.values(g.itemsById);
      if (items.length === 0) {
        unanchored.push(g);
        continue;
      }
      let earliest = items[0].created_at;
      for (const it of items) {
        if (it.created_at < earliest) earliest = it.created_at;
      }
      let chosen = -1;
      for (let i = 0; i < messagesWithStream.length; i++) {
        const m = messagesWithStream[i];
        if (m.role !== "assistant") continue;
        if (m.timestamp <= earliest) chosen = i;
        else break; // timestamps are monotonically non-decreasing in this list
      }
      if (chosen < 0) unanchored.push(g);
      else anchored.push({ group: g, insertAfter: chosen });
    }

    // Stable order: groups that anchor at the same message keep
    // their original order (declared by App.tsx onTodoChange).
    anchored.sort((a, b) => a.insertAfter - b.insertAfter);

    // Splice anchored groups right after their anchor message.
    // Walk insertAfter descending so earlier splices don't shift
    // later indices.
    const out = rows.slice();
    for (let i = anchored.length - 1; i >= 0; i--) {
      const { group, insertAfter } = anchored[i];
      out.splice(insertAfter + 1, 0, { kind: "todo", group, role: undefined as undefined });
    }
    // Append unanchored groups (legacy / pre-tool-call / snapshot
    // hydrate with no anchoring data) at the very end of the stream,
    // in their declared order.
    for (const g of unanchored) out.push({ kind: "todo", group: g, role: undefined as undefined });
    return out;
  }, [messagesWithStream, todoGroups]);

  const handleSubmit = async () => {
    const trimmed = input.trim();
    if (!trimmed) return;
    // Snapshot the current attachments up-front so both the enqueue
    // (streaming) and direct-send paths share one source of truth.
    const attachedImages: QueuedImage[] = pendingImages.map((img) => ({
      file: img.file,
      blobUrl: img.blobUrl,
      mimeType: img.mimeType,
      name: img.name,
    }));
    const attachedPaths: string[] = filePaths.length > 0 ? [...filePaths] : [];
    const attachments: { images?: QueuedImage[]; attachedFilePaths?: string[] } | undefined =
      attachedImages.length > 0 || attachedPaths.length > 0
        ? {
            images: attachedImages.length > 0 ? attachedImages : undefined,
            attachedFilePaths: attachedPaths.length > 0 ? attachedPaths : undefined,
          }
        : undefined;
    // Clear any previous stream error — the user is starting a new
    // turn, so the old error is stale.  The banner will disappear
    // immediately and, if this turn also fails, re-appear via the
    // onError callback in App.tsx.
    onClearStreamError?.();
    setInput("");
    // If currently streaming OR a previous send is still in flight,
    // enqueue instead of sending directly. The App.tsx auto-drain
    // `useEffect` ([streaming, messageQueue, handleSendMessage]) will
    // pull the queued message and call `handleSendMessage(next)` once
    // the streaming turn completes.
    //
    // Both `sending` (local React state, set synchronously) and
    // `isStreaming` (derived from the WS hook) must be checked: a
    // rapid double-Enter before the hook has committed
    // `streamingSessionId` would see `isStreaming=false` and the
    // pure `sending` check would see the stale closure value
    // `false` too, leading to a double-fire. Combining them covers
    // both the streaming-in-progress case and the within-Render-
    // Tick race.
    if (isStreaming || sending) {
      // Enqueue the FULL payload (text + images + file paths) so the
      // queued message is sent exactly like a direct one once the
      // stream settles. Detach the images from the input box WITHOUT
      // revoking their blob URLs — they travel on the queued message
      // and render in the user bubble after the drain.
      onAddToQueue?.({
        text: trimmed,
        images: attachedImages.length > 0 ? attachedImages : undefined,
        attachedFilePaths: attachedPaths.length > 0 ? attachedPaths : undefined,
      });
      clearPendingImages();
      setFilePaths([]);
      return;
    }
    setSending(true);
    try {
      const overrides: ChatOverrides = { permissionLevel };
      // 2026-06-26: 把用户在下拉里选的模型塞进 overrides，让 backend 真的切到
      // 那个 provider（之前 bug：UI 看着切了，但 payload 不带 model，backend
      // 永远在跑 config.yaml 默认模型）。格式 "provider:model"。
      if (selectedModel) overrides.model = selectedModel;
      if (expert) {
        overrides.systemPromptOverride = expert.systemPrompt;
        markRecentUsed(expert.id);
      }
      // T2026-06-21 P1 (rev): dynamic tool allow-list, single source of
      // truth. We derive the tool list from the live `GET /v1/tools`
      // response — no hard-coded names anywhere on the front-end. The
      // expert's `toolset` tag is **advisory metadata** (shown in the
      // UI, used for distribution probability) — it is **not** a tool
      // filter, because (a) builtin expert toolset tags like
      // `development`/`balanced` rarely match the registered toolset
      // tag (`browser`/`system`/`memory`/...) and (b) the user has
      // already explicitly toggled `isEnabled` per toolset in the
      // Settings UI. Trusting the registry's `isEnabled` flag is the
      // only way the LLM can ever discover the full set of tools
      // without a front-end code change.
      const enabledToolNames = availableTools
        .filter((t) => t.isEnabled)
        .map((t) => t.name);
      if (enabledToolNames.length > 0) {
        overrides.tools = enabledToolNames;
      }
      void expert?.toolset; // expert.toolset is shown in UI / used by
                            // backend routing but not as a filter here.
      // v2 (2026-06-29): 主人反馈「选了目录对话时 agent 还是只找 C 盘」。
      // 根因：useSessionWorkspace 的 state 在 session 切换、welcome ↔ chat 切换、
      // HMR 重置等时机下可能短暂为 null，但 localStorage / session.workspace / draft 槽位
      // 已经有真实路径。state 为空时直接放过 → overrides.workspace 没传 → 后端
      // 落回 server process CWD（C 盘）。
      //
      // 修法：state 为空时，按优先级回退到最权威的"该用哪个目录"信号：
      //   1. useSessionWorkspace 自己的 state（最优先，UI 即时反馈）
      //   2. session?.workspace （后端持久化值，可能比 localStorage 更新）
      //   3. window.localStorage 直接读（兜底，应对 useState 初始化竞态）
      //   4. useWorkspaceRoot 全局值（用户顶部导航设过的工作区）
      // 任意一个非空就传给后端，让 LLM 在系统 prompt 看到 "## Active Workspace"，
      // 文件/终端工具也走 `TERMINAL_CWD` 而不是 server CWD。
      const fallbackWorkspace =
        (workspace && workspace.trim()) ||
        (session?.workspace && session.workspace.trim()) ||
        (() => {
          try {
            const raw = storage.getItem(
              sessionId
                ? `hermix.session.${sessionId}.workspace`
                : STORAGE_KEYS.draftWorkspace,
            );
            return raw && raw.trim() ? raw : null;
          } catch {
            return null;
          }
        })();
      if (fallbackWorkspace) overrides.workspace = fallbackWorkspace;
      if (activeSkills.length > 0) overrides.skills = activeSkills;
      // Images + attached file paths are forwarded via `attachments` so
      // App.handleSendMessage can build the backend `overrides.images`
      // bytes and the in-bubble `ChatMessage.images` from a single
      // source. (Previously the bytes were built here and the bubble
      // never received the preview, so images were sent but invisible.)
      await onSendMessage(trimmed, undefined, overrides, attachments);
    } finally {
      setSending(false);
      clearPendingImages();
      setFilePaths([]);
    }
  };

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      handleSubmit();
    }
  };

  // Empty state — but a message-load error takes precedence over the
  // welcome page. If the backend is unreachable (typical right after a
  // desktop+server restart race), the user would otherwise see the
  // mascot forever with no signal that something went wrong.
  if (messageLoadError) {
    return (
      <div
        className="flex-1 flex flex-col items-center justify-center px-8 min-h-0"
        style={{ background: "var(--color-bg-primary)", overflow: "visible" }}
      >
        <div className="w-full max-w-md flex flex-col items-center py-12 text-center">
          <div
            className="w-14 h-14 rounded-full flex items-center justify-center mb-5"
            style={{
              background: "var(--color-warning-bg, rgba(245, 158, 11, 0.12))",
              color: "var(--color-warning, #d97706)",
            }}
          >
            <AlertTriangle size={26} />
          </div>
          <h2 className="text-lg font-semibold text-text-primary mb-2">
            加载对话失败
          </h2>
          <p className="text-sm text-text-secondary mb-1">
            后端暂时无法返回这条对话的记录。常见原因：服务还在启动、
            网络瞬断、或数据库暂时不可用。
          </p>
          <p
            className="text-xs text-text-tertiary mb-6 font-mono break-all max-w-sm"
            title={messageLoadError}
          >
            {messageLoadError}
          </p>
          <div className="flex items-center gap-2">
            <button
              onClick={() => onRetryLoad?.()}
              className="btn btn-primary"
              type="button"
            >
              <ArrowUp size={14} />
              <span>重试</span>
            </button>
            <button
              onClick={onNewChat}
              className="btn btn-secondary"
              type="button"
            >
              <span>新建对话</span>
            </button>
          </div>
        </div>
      </div>
    );
  }

  // v2 (2026-06-29): 主人反馈「点停止跳到欢迎页，回不去」。
  // 根因：停止流后 500ms re-fetch 在 server 那边偶尔返回空数组（cancel
  // 时机太早 user_msg 还没持久化），setSessions 把内存 messages 替换成
  // 空数组 → `session.messages.length === 0` → 下面那个 Empty State
  // 判断条件满足 → ChatView 渲染吉祥物欢迎页。视觉上像"被踢出去"。
  //
  // Loading guard: only show spinner when an EXISTING session (with
  // message_count > 0 in the backend) has messages that haven't loaded
  // into memory yet. New sessions (message_count === 0 or undefined)
  // should show the welcome page, not the spinner.
  const isLoadingExisting = session
    && session.messages.length === 0
    && ((session as any).message_count ?? 0) > 0;
  if (isLoadingExisting) {
    return (
      <div
        className="flex-1 flex flex-col items-center justify-center px-8 min-h-0"
        style={{ background: "var(--color-bg-primary)" }}
      >
        <div className="flex flex-col items-center gap-3 text-center">
          <div
            className="w-9 h-9 rounded-full border-2 border-border-primary border-t-text-muted animate-spin"
            aria-hidden="true"
          />
          <p className="text-[13px] text-text-muted">正在恢复对话...</p>
        </div>
      </div>
    );
  }

  // Empty state — show welcome when no session or session has no messages
  if (!session || session.messages.length === 0) {
    return (
      <div className="flex-1 flex flex-col items-center justify-center px-8 min-h-0" style={{ background: "var(--color-bg-primary)", overflow: "visible" }}>
        <div className="w-full max-w-3xl flex flex-col items-center py-12">
          {/* Hero — mascot */}
          <div className="relative mb-4">
            <img
              src={mascotUrl}
              alt="Hermix吉祥物"
              className="w-[140px] h-[140px] object-contain select-none"
              draggable={false}
            />
          </div>

          {/* Hero slogan */}
          <h1 className="text-2xl font-bold text-text-primary mb-2 tracking-tight">你好！我是 Hermix</h1>
          <p className="text-sm text-text-secondary text-center mb-6 max-w-xs leading-relaxed">
            一个智能 AI 助手。告诉我你的目标，我来帮你拆解并完成。
          </p>

          <div className="w-full">
            <Composer
              input={input} setInput={setInput} handleKeyDown={handleKeyDown} handleSubmit={handleSubmit}
              textareaRef={textareaRef} sending={sending} isStreaming={false}
              activeWorkspace={activeWorkspace} workspaces={workspaces} workspaceLocked={workspaceLocked}
              onSelectWorkspace={(p) => setWorkspace(p)}
              onAddWorkspace={async () => {
                const path = await pickDirectory();
                if (path) {
                  const ws = addWorkspace("", path);
                  setWorkspace(ws.path);
                }
              }}
              onRemoveWorkspace={removeWorkspace}
              models={models} byProvider={byProvider} selectedModel={selectedModel} setSelectedModel={setSelectedModelAndPersist}
              currentDisplay={currentDisplay}
              expert={expert} setExpert={setExpertWithCutoff}
              recentExperts={recentExperts}
              experts={experts} categories={categories} byCategory={byCategory}
              onPickRecent={(id) => { markRecentUsed(id); setExpertWithCutoff(id); }}
              permissionLevel={permissionLevel} setPermissionLevel={setPermissionLevel}
              onOpenExperts={onOpenExperts}
              onOpenSettings={onOpenSettings}
              onOpenComputer={onOpenComputer}
              onUndo={onUndo}
              onRestore={onRestore}
              onStop={onStop}
              pendingImages={pendingImages}
              onRemoveImage={removePendingImage}
              onFilesAttach={handleFilesAttach}
              keepOriginalImage={keepOriginalImage}
              setKeepOriginalImage={setKeepOriginalImage}
              placeholder="Hermix帮你做事 — 写作、规划、分析、学习、开发..."
            />

            {/* Stream error banner — same component as the
                post-session branch uses, so the welcome page also
                tells the user "last attempt failed, here's why"
                instead of the mascot silently absorbing the error. */}
            {streamError && (
              <div className="stream-error-banner mt-3" role="alert">
                <span className="stream-error-icon" aria-hidden="true"><AlertTriangle size={14} /></span>
                <div className="stream-error-content">
                  <span className="stream-error-title">发送失败</span>
                  <span className="stream-error-detail">
                    {streamError.length > 160 ? streamError.slice(0, 160) + "…" : streamError}
                  </span>
                </div>
                <button
                  onClick={onClearStreamError}
                  className="stream-error-dismiss"
                  title="关闭"
                  type="button"
                  aria-label="关闭错误提示"
                >
                  <X size={14} />
                </button>
              </div>
            )}
          </div>
        </div>
      </div>
    );
  }

  return (
    <div className="flex-1 flex flex-col overflow-hidden min-h-0" style={{ background: "var(--color-bg-primary)" }}>
      <ModelStatusBar
        title={session?.title}
        rightOpen={rightOpen}
        onToggleRight={onToggleRight}
      />
      {archiveRefs.length > 0 && (
        <ArchiveRefsBanner
          refs={archiveRefs}
          totalChars={archiveTotalChars}
          open={archiveOpen}
          onToggle={() => setArchiveOpen((v) => !v)}
        />
      )}
      {/* P0-T1 (Frontend Audit 2026-07-13): todo cards are now
        spliced inline next to their originating assistant message
        (see `streamRow` useMemo above). The previous top-of-stream
        strip is gone. */}
      <div ref={scrollContainerRef} onScroll={handleContainerScroll} className="flex-1 overflow-y-auto px-3 sm:px-4 py-5 min-h-0">
        <div className="max-w-[720px] lg:max-w-[800px] xl:max-w-[920px] mx-auto space-y-5">
          {streamRow.map((row, idx) => {
            if (row.kind === "todo") {
              return (
                <div key={`todo-${row.group.groupId}`} className="todo-anchor">
                  <TodoListCard
                    groupId={row.group.groupId}
                    groupTitle={row.group.groupTitle}
                    itemsById={row.group.itemsById}
                    onPatch={onTodoPatch}
                    justPatchedRef={justPatchedTodoRef}
                  />
                </div>
              );
            }
            const msg = row.msg;
            if (streamingAssistantId && msg.id === streamingAssistantId && streamingMsg) {
              // Streaming placeholder shares the same header rule as
              // the post-stream MemoMessageBubble — see
              // `shouldShowAssistantHeader`. Without this, a fresh
              // placeholder created mid-loop (e.g. after a tool run)
              // would render its own avatar + "正在生成…" header and
              // produce the "header pops in twice for one user turn"
              // bug the user reported on 2026-06-17.
              //
              // The streaming placeholder is *not* in `messagesWithStream`
              // at this index (it's spliced in from session.messages),
              // so we compute its header by its raw position in
              // `session.messages`.
              const rawIdx = session?.messages.findIndex((m) => m.id === streamingAssistantId) ?? -1;
              const showStreamHeader = rawIdx >= 0
                ? shouldShowAssistantHeader(rawIdx, session!.messages)
                : true;
              return (
                <StreamingBubble
                  key={msg.id}
                  text={streamingMsg.content}
                  phase={streamingPhase}
                  statusText={streamingStatus}
                  showHeader={showStreamHeader}
                  reasoningContent={streamingMsg.reasoningContent}
                  executionTimeline={streamingMsg.execution_timeline}
                  unifiedSteps={streamingMsg.unified_steps}
                />
              );
            }
            // showHeader rule: render the avatar/name/stop-chip on the
            // FIRST assistant message after a `user` message (i.e. the
            // start of an agent loop). Subsequent assistant messages
            // within the same loop — including ones that come after a
            // tool, an approval card, or a clarify card — share the
            // same header and the body text just continues below the
            // first bubble's avatar column.
            //
            // We compute shouldShowAssistantHeader from streamRow
            // (the merged array) and find the position of the current
            // message in it. Since shouldShowAssistantHeader now
            // skips ALL same-turn roles (assistant/tool/approval/clarify),
            // it correctly finds the turn boundary (user message) even
            // when mergeConsecutiveAssistantMessages has collapsed
            // multiple server-persisted assistant rows into one.
            const showHeader = msg.role !== "assistant" || shouldShowAssistantHeader(idx, streamRow);
            return (
              <MemoMessageBubble
                key={msg.id}
                message={msg}
                workspaceRoot={workspaceRoot}
                onApproveApproval={onApproveApproval}
                onDenyApproval={onDenyApproval}
                onAnswerClarify={onAnswerClarify}
                showHeader={showHeader}
                isStreaming={!!streamingAssistantId && msg.id === streamingAssistantId}
                onPathClick={onPathClick}
                expertCutoffAt={expertCutoffAt}
              />
            );
          })}

          {/* The streaming assistant message is now rendered inline in
              the loop above (so tool cards visually precede streamed
              text). Don't render a duplicate StreamingBubble here. */}

          {streamingMsg && prefetchIntent && (
            <PrefetchIntentChip
              intent={prefetchIntent.intent}
              confidence={prefetchIntent.confidence}
            />
          )}

          {/* v19 / 2026-06-19: Race-safe streaming fallback.
              The normal path renders the streaming assistant row inside
              the `messagesWithStream.map(...)` loop above (via
              `<StreamingBubble>`). That works whenever
              `streamingEntry` has already picked up the empty
              placeholder row from `session.messages`. There is a
              narrow race window — typically the first render after
              `sendStream` fires its `setStreamingSessionId(sessionId)`
              but before App.tsx's earlier `setSessions` push of the
              empty assistant placeholder has been committed — where
              `streamingMsg` is still null even though `isStreaming` is
              true.

              The previous version of this block rendered a SECOND
              indicator (`<ThoughtStreamCard>` with its own EKG + hint)
              in that race, which produced the "正在生成… / 正在思考…"
              duplication the user reported on 2026-06-19 (a separate
              `<StreamingBubble>` was already rendering in the loop
              below because the placeholder row had been picked up by
              the time the map ran). To eliminate the double indicator
              we now render a single fallback `<StreamingBubble>` here
              — the same component the loop uses, so visually it's
              indistinguishable from the steady state and never
              duplicates itself. The placeholder row in `session.messages`
              that didn't get picked up by `streamingEntry` will be
              picked up on the next render (next microtask), at which
              point this fallback unmounts and the loop's
              `<StreamingBubble>` takes over without a visual jump. */}

          {isStreaming && !streamingMsg && session && (
            <StreamingBubble
              key="streaming-fallback"
              text=""
              phase={streamingPhase}
              statusText={streamingStatus}
              showHeader
            />
          )}

          {/* Post-stream gap indicator. Shown after the LLM stream
              settles but before the agent loop either ends or starts
              its next turn. Replaces the silent blank gap that used
              to make Hermix look "stuck" between turns. */}
          {!isStreaming && !streamingMsg && postStreamActivity && (
            <AgentActivityIndicator
              pendingToolsCount={pendingToolsCount}
              visible={postStreamActivity}
            />
          )}

          {/* "Waiting for input" pill. Shown when the entire agent
              loop has fully ended (no stream, no pending tools, no
              post-stream gap window). Replaces the silent "done"
              state so the user knows Hermix is idle and ready. */}
          {!isStreaming && !streamingMsg && !postStreamActivity && pendingToolsCount === 0 && (
            <WaitForInputIndicator visible={!!session && session.messages.length > 0} />
          )}

          <div ref={messagesEndRef} />
        </div>
      </div>

      <div className="px-3 sm:px-4 pb-4 shrink-0">
        <div className="max-w-[720px] lg:max-w-[800px] xl:max-w-[920px] mx-auto">
          {/* M5: ACUI panel — sits directly above the composer so it
              tracks with the input bar visually and never scrolls
              out of view mid-stream. Renders nothing when no cards
              are active. */}
          {acuiState && acuiState.cards.length > 0 && (
            <AcuiPanel state={acuiState} decide={acuiBus} />
          )}

          {/* Stream error banner — shown above the input until dismissed
              or a new message is sent.  Compact, dismissable, and
              clearly separated from the conversation history. */}
          {streamError && (
            <div className="stream-error-banner" role="alert">
              <span className="stream-error-icon" aria-hidden="true"><AlertTriangle size={14} /></span>
              <div className="stream-error-content">
                <span className="stream-error-title">发送失败</span>
                <span className="stream-error-detail">
                  {streamError.length > 160 ? streamError.slice(0, 160) + "…" : streamError}
                </span>
              </div>
              <button
                onClick={onClearStreamError}
                className="stream-error-dismiss"
                title="关闭"
                type="button"
                aria-label="关闭错误提示"
              >
                <X size={14} />
              </button>
            </div>
          )}

{/* Day 1-B: compaction pipeline status banner. Rendered above
the composer so it stays in view while the user types.
Today the prop is always null (no backend wiring yet) so
the banner is a no-op. Day 2 will plumb the WS event. */}
<CompactionStatusBanner status={compactionStatus} />

{/* Context usage indicator — shows current context window usage
as a small bar with token count and percentage. Turns amber
at ≥85% and red at ≥95% of the compression threshold. */}
{contextUsage && contextUsage.maxTokens > 0 ? (
  <div className="mb-2 flex items-center gap-2 px-1 text-[11px] text-text-secondary">
    {(() => {
      const fmt = (n: number) => n >= 1_000_000 ? `${(n / 1_000_000).toFixed(1)}M` : n >= 1_000 ? `${Math.round(n / 1_000)}k` : String(n);
      const pct = Math.min(100, contextUsage.percent);
      const color = pct >= 95 ? "bg-rose-500" : pct >= 85 ? "bg-amber-500" : "bg-emerald-500";
      const textColor = pct >= 95 ? "text-rose-600 dark:text-rose-400" : pct >= 85 ? "text-amber-600 dark:text-amber-400" : "";
      // 2026-07-14: derive the live cache-hit rate from the most
      // recent `usage_update` snapshot. Anthropic-style semantics —
      // hit rate = cache_read / (cache_read + uncached prompt). The
      // chip only renders when there's at least one snapshot, so
      // the bar doesn't shift width mid-stream when it appears.
      let cacheChip: React.ReactNode = null;
      if (usageSnapshot) {
        const cacheRead = usageSnapshot.cacheReadTokens;
        const uncached = usageSnapshot.promptTokens;
        const total = cacheRead + uncached;
        const hasData = total > 0;
        const rate = hasData ? (cacheRead / total) * 100 : 0;
        // Tiered colors: green ≥80%, amber 50–80%, muted below.
        // Zero hits with non-zero prompt tokens → "未命中" muted grey.
        let rateText: string;
        let rateTone: string;
        if (!hasData) {
          rateText = "—";
          rateTone = "text-text-faint";
        } else if (cacheRead === 0) {
          rateText = "未命中";
          rateTone = "text-text-faint";
        } else if (rate >= 80) {
          rateText = `${Math.round(rate)}% 缓存命中`;
          rateTone = "text-emerald-600 dark:text-emerald-400";
        } else if (rate >= 50) {
          rateText = `${Math.round(rate)}% 缓存命中`;
          rateTone = "text-amber-600 dark:text-amber-400";
        } else {
          rateText = `${Math.round(rate)}% 缓存命中`;
          rateTone = "text-text-secondary";
        }
        cacheChip = (
          <span
            data-testid="cache-hit-chip"
            className={`shrink-0 tabular-nums px-1.5 py-0.5 rounded-md border border-border-subtle transition-colors duration-300 ${rateTone}`}
            title={
              hasData
                ? `缓存读取 ${cacheRead.toLocaleString()} tok / 输入 ${(cacheRead + uncached).toLocaleString()} tok`
                : "暂无缓存数据"
            }
          >
            {rateText}
          </span>
        );
      }
      return (<>
        <div className="flex-1 h-1.5 rounded-full bg-black/5 dark:bg-white/10 overflow-hidden">
          <div className={`h-full rounded-full transition-all duration-300 ${color}`} style={{ width: `${pct}%` }} />
        </div>
        <span className={`shrink-0 tabular-nums ${textColor}`}>
          {fmt(contextUsage.usedTokens)} / {fmt(contextUsage.maxTokens)} tokens ({Math.round(pct)}%)
        </span>
        {cacheChip}
      </>);
    })()}
  </div>
) : !isStreaming && (
  // 2026-07-16: 恢复的会话（非流式）没有活跃 WS 流，后端不推送
  // context_usage，因此显示占位而非整块静默消失，避免与 live 会话
  // 行为不一致。一旦用户发送消息、收到首个 context_usage 事件即被
  // 上面的真实进度条替换。
  <div className="mb-2 flex items-center gap-2 px-1 text-[11px] text-text-faint">
    <div className="flex-1 h-1.5 rounded-full bg-black/5 dark:bg-white/10" />
    <span className="shrink-0">上下文用量将在发送消息后更新</span>
  </div>
)}

          <Composer
            input={input} setInput={setInput} handleKeyDown={handleKeyDown} handleSubmit={handleSubmit}
            textareaRef={textareaRef} sending={sending} isStreaming={isStreaming}
            activeWorkspace={activeWorkspace} workspaces={workspaces} workspaceLocked={workspaceLocked}
            onSelectWorkspace={(p) => setWorkspace(p)}
            onAddWorkspace={async () => {
              const path = await pickDirectory();
              if (path) {
                const ws = addWorkspace("", path);
                setWorkspace(ws.path);
              }
            }}
            onRemoveWorkspace={removeWorkspace}
            models={models} byProvider={byProvider} selectedModel={selectedModel} setSelectedModel={setSelectedModelAndPersist}
            currentDisplay={currentDisplay}
            expert={expert} setExpert={setExpertWithCutoff}
            recentExperts={recentExperts}
            experts={experts} categories={categories} byCategory={byCategory}
            onPickRecent={(id) => { markRecentUsed(id); setExpertWithCutoff(id); }}
            permissionLevel={permissionLevel} setPermissionLevel={setPermissionLevel}
            onOpenExperts={onOpenExperts}
            onOpenSettings={onOpenSettings}
            onOpenComputer={onOpenComputer}
            onUndo={onUndo}
            onRestore={onRestore}
            onStop={onStop}
            pendingImages={pendingImages}
            onRemoveImage={removePendingImage}
            onFilesAttach={handleFilesAttach}
            keepOriginalImage={keepOriginalImage}
            setKeepOriginalImage={setKeepOriginalImage}
            placeholder="输入消息..."
            messageQueue={messageQueue}
            onAddToQueue={onAddToQueue}
            onClearQueue={onClearQueue}
            onRemoveFromQueue={onRemoveFromQueue}
            onEditQueueItem={onEditQueueItem}
            onSendFromQueue={onSendFromQueue}
          />
        </div>
      </div>
    </div>
  );
}

// ── Composer (shared input area) ─────────────────────

interface ComposerProps {
  input: string;
  setInput: (v: string) => void;
  handleKeyDown: (e: React.KeyboardEvent) => void;
  handleSubmit: () => void;
  textareaRef: React.RefObject<HTMLTextAreaElement | null>;
  sending: boolean;
  isStreaming: boolean;
  activeWorkspace: import("../hooks/useWorkspaces").Workspace | null;
  workspaceLocked?: boolean;
  workspaces: import("../hooks/useWorkspaces").Workspace[];
  onSelectWorkspace: (path: string | null) => void;
  onAddWorkspace: () => Promise<void>;
  onRemoveWorkspace: (id: string) => void;
  models: Array<{ id: string; display: string; provider: string }>;
  selectedModel: string;
  setSelectedModel: (id: string) => void;
  currentDisplay: string;
  byProvider: Array<{ provider: string; isCustom: boolean; models: Array<{ id: string; display: string; provider: string }> }>;
  expert: import("../data/builtinExperts").ExpertDef | null;
  setExpert: (id: string | null) => void;
  recentExperts: import("../data/builtinExperts").ExpertDef[];
  experts: import("../data/builtinExperts").ExpertDef[];
  categories: import("../data/builtinExperts").ExpertCategory[];
  byCategory: Record<string, import("../data/builtinExperts").ExpertDef[]>;
  onOpenExperts?: () => void;
  /** 跳转到「设置」页（模型未配置时点击 chip 用） */
  onOpenSettings?: () => void;
  /** 2026-07-08 (user): 已选工作区时点目录 → 打开右栏文件浏览器切到该目录 */
  onOpenComputer?: () => void;
  onPickRecent?: (id: string) => void;
  onUndo?: () => void;
  onRestore?: () => void;
  onFilesAttach?: (files: File[]) => void;
  pendingImages?: PendingImage[];
  onRemoveImage?: (index: number) => void;
  keepOriginalImage?: boolean;
  setKeepOriginalImage?: (v: boolean) => void;
  onStop?: () => void;
  permissionLevel: PermissionLevel;
  setPermissionLevel: (lvl: PermissionLevel) => void;
  placeholder: string;
  messageQueue?: QueuedMessage[];
  onAddToQueue?: (msg: QueuedMessage) => void;
  onClearQueue?: () => void;
  onRemoveFromQueue?: (idx: number) => void;
  onEditQueueItem?: (idx: number, newText: string) => void;
  onSendFromQueue?: (idx: number) => void;
}

function Composer(p: ComposerProps) {
  const [projOpen, setProjOpen, projContainerRef, projTriggerRef, projToggle] = usePopover();
  const [draggingFiles, setDraggingFiles] = useState(false);
  const dragCounter = useRef(0);
  const [modelOpen, setModelOpen, modelContainerRef, modelTriggerRef, modelToggle] = usePopover();
  const [expertOpen, setExpertOpen, expertContainerRef, expertTriggerRef, expertToggle] = usePopover();
  const [permOpen, setPermOpen, permContainerRef, permTriggerRef, permToggle] = usePopover();

  // Model picker provider-collapse state (2026-06-26): each provider gets
  // a single collapsible row in the picker popover. Builtin providers
  // default to expanded; custom providers default to collapsed so a
  // 50-model `custom-agnes` doesn't push builtin providers off-screen.
  // If the currently selected model belongs to a provider, that provider
  // is auto-expanded so the user can see what's currently active.
  const [expandedProviders, setExpandedProviders] = useState<Record<string, boolean>>({});
  const toggleProvider = useCallback((provider: string) => {
    setExpandedProviders((prev) => ({ ...prev, [provider]: !prev[provider] }));
  }, []);

  // ── Toolbar layout (v23: 坚决不允许两行) ───────────────────────────
  //
  // v22 通过 ResizeObserver + toolbarCompact 折叠部分文字标签来防止换行，
  // 但极端窄宽度下仍可能换行。v23: toolbar 强制 flex-nowrap + whitespace-nowrap，
  // 配合更激进的折叠阈值，彻底杜绝第二行。
  const toolbarRef = useRef<HTMLDivElement>(null);
  const [toolbarCompact, setToolbarCompact] = useState(false);
  useEffect(() => {
    const el = toolbarRef.current;
    if (!el || typeof ResizeObserver === "undefined") return;
    const calc = () => {
      // 容器宽 < 700 → 折叠左侧 chip 文字（更高的阈值，提前折叠）
      setToolbarCompact(el.clientWidth < 700);
    };
    calc();
    const ro = new ResizeObserver(calc);
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  return (
    <div
      className="rounded-2xl transition-all relative"
      style={{
        background: "var(--color-bg-primary)",
        border: "1px solid var(--color-border-primary)",
        boxShadow: "0 1px 3px rgba(0,0,0,0.04), 0 1px 2px rgba(0,0,0,0.03)",
        // 保留 popover 完整可见（之前 overflow-hidden 会切掉底部弹层）
        overflow: "visible",
      }}
      onDragEnter={(e) => {
        if (!e.dataTransfer?.types?.includes("Files")) return;
        dragCounter.current += 1;
        setDraggingFiles(true);
      }}
      onDragOver={(e) => {
        if (e.dataTransfer?.types?.includes("Files")) {
          e.preventDefault();
          e.dataTransfer.dropEffect = "copy";
        }
      }}
      onDragLeave={() => {
        dragCounter.current = Math.max(0, dragCounter.current - 1);
        if (dragCounter.current === 0) setDraggingFiles(false);
      }}
      onDrop={(e) => {
        dragCounter.current = 0;
        setDraggingFiles(false);
        const files = Array.from(e.dataTransfer?.files ?? []);
        if (files.length === 0) return;
        e.preventDefault();
        // 直接传 File 数组，零拷贝；父组件负责压缩 + blob URL
        p.onFilesAttach?.(files);
      }}
    >
      {/* 拖放附件时的视觉提示遮罩 */}
      {draggingFiles && (
        <div
          className="absolute inset-0 z-10 rounded-2xl flex items-center justify-center pointer-events-none"
          style={{
            background: "rgba(15, 110, 86, 0.08)",
            border: "2px dashed rgba(15, 110, 86, 0.5)",
          }}
        >
          <div className="text-[13px] text-[#0F6E56] font-medium bg-bg-elevated/90 px-3 py-1.5 rounded-lg">
            松开以添加文件
          </div>
        </div>
      )}

      {/* Image preview (above textarea when attached) */}
      {p.pendingImages && p.pendingImages.length > 0 && (
        <div className="px-3 pt-3 pb-1">
          <div className="flex items-center gap-2 overflow-x-auto">
            {p.pendingImages.map((img, idx) => (
              <div
                key={`${img.blobUrl}-${idx}`}
                className="relative w-14 h-14 rounded-lg overflow-hidden flex items-center justify-center shrink-0"
                style={{ background: "var(--color-bg-tertiary)", border: "1px solid var(--color-border-subtle)" }}
                title={img.name}
              >
                <img
                  src={img.blobUrl}
                  alt={img.name}
                  className="max-w-full max-h-full object-contain"
                />
                {img.isCompressed && (
                  <span className="absolute bottom-0 left-0 right-0 text-[8px] leading-tight bg-black/65 text-white text-center">
                    {formatBytes(img.sizeBytes)}
                  </span>
                )}
                <button
                  type="button"
                  onClick={() => p.onRemoveImage?.(idx)}
                  className="absolute top-0.5 right-0.5 p-0.5 rounded-md bg-bg-elevated/90 text-text-muted hover:text-error transition-colors"
                  title="移除附件"
                >
                  <X size={10} />
                </button>
              </div>
            ))}
          </div>
          <div className="flex items-center gap-2 mt-1.5">
            <span className="h-tiny text-text-muted flex items-center gap-1">
              <Image size={10} />
              已附加 {p.pendingImages.length} 个附件
            </span>
            <label className="flex items-center gap-1 h-tiny text-text-faint cursor-pointer">
              <input
                type="checkbox"
                checked={!!p.keepOriginalImage}
                onChange={(e) => p.setKeepOriginalImage?.(e.target.checked)}
                className="w-3 h-3"
              />
              <span>保持原画质</span>
            </label>
          </div>
        </div>
      )}

      {/* Queue display — shown when messages are queued.
       * 2026-06-28: redesigned from "N 条消息 + 清空" stub into a
       * full per-item list with edit / send-now / remove affordances.
       * The textarea below remains the primary input; queued items
       * are auto-drained by App.tsx's `useEffect` once `streaming`
       * settles. Manual "send now" bypasses the queue by calling
       * handleSendMessage directly (and re-enqueues on failure). */}
      {p.messageQueue && p.messageQueue.length > 0 && (
        <div className="px-4 pt-2 pb-1.5 border-t border-border-subtle">
          <div className="flex items-center justify-between mb-1.5">
            <span className="h-tiny text-text-muted">
              队列中 {p.messageQueue.length} 条消息
            </span>
            <button
              onClick={() => p.onClearQueue?.()}
              className="h-tiny text-text-muted hover:text-text-primary px-1.5 py-0.5 rounded hover:bg-bg-tertiary"
            >
              清空
            </button>
          </div>
          <ul className="space-y-1 max-h-32 overflow-y-auto">
            {p.messageQueue.map((msg, idx) => (
              <QueueItemRow
                key={`${idx}-${msg.text.slice(0, 24)}`}
                index={idx}
                total={p.messageQueue!.length}
                text={msg.text}
                canEdit={!!p.onEditQueueItem}
                canRemove={!!p.onRemoveFromQueue}
                canSend={!!p.onSendFromQueue}
                onEdit={(t) => p.onEditQueueItem?.(idx, t)}
                onRemove={() => p.onRemoveFromQueue?.(idx)}
                onSend={() => p.onSendFromQueue?.(idx)}
              />
            ))}
          </ul>
        </div>
      )}

      {/* Textarea — sit on the gray container, no own border/background. */}
      <textarea
        ref={p.textareaRef}
        value={p.input}
        onChange={(e) => p.setInput(e.target.value)}
        onKeyDown={p.handleKeyDown}
        onPaste={(e) => {
          // 粘贴上传：从剪贴板提取图片文件
          const files = Array.from(e.clipboardData?.files ?? []);
          const imageFiles = files.filter((f) => f.type.startsWith("image/"));
          if (imageFiles.length > 0) {
            e.preventDefault();
            p.onFilesAttach?.(imageFiles);
          }
        }}
        placeholder={p.isStreaming ? "输入消息加入队列..." : p.placeholder}
        data-no-border
        className="w-full bg-transparent text-text-primary placeholder-text-muted resize-none outline-none px-4 pt-3.5 pb-1.5 min-h-[72px]"
        style={{
          fontSize: "var(--font-size-body)",
          lineHeight: 1.5,
          border: "none",
          borderRadius: 0,
          boxShadow: "none",
          background: "transparent",
        }}
        rows={3}
      />

      {/* Tool chips row + actions row.
        *
        * v4 (主人反馈: "输入框下面的功能按钮给我回复成文字+图标的啊")
        * ─────────────────────────────────────────────────
        * 之前在主列 < 580px 时会切到 compact 模式，把所有文字标签隐
        * 藏只显示图标。主人认为纯图标是反产品方向 —— 用户无法识
        * 别按钮用途。
        *
        * 修复策略:
        *   · 移除 compact 模式（state + ResizeObserver + data-compact + group/ct 全部删除）
        *   · 文字标签永远显示
        *   · 减小 padding / gap / max-width 让文字+图标在 580px 主列里也能放下
        *   · 极端窄列用外层 flex-wrap 自动换行（左边 3 chip 一行，右边模型+动作按钮一行）
        *   · 关闭过窄的"激活态长名"截断 max-w
        *
        * 视觉规则沿用 v3:
        *   · 默认态：纯文字 + 小尺寸图标 hint，灰
        *   · 激活态：文字带强调色（仍然无背景填充），用图标颜色区分
        *   · 无 chevron（popover 用 hover 提示就够了）
        *   · hover 时显示浅灰底
        */}
      <div
        ref={toolbarRef}
        className="px-3 pb-3 pt-1 flex flex-wrap items-center gap-x-1 gap-y-1"
        data-compact={toolbarCompact ? "true" : "false"}
      >
        {/* Left: tool chips — 名称模式（永远显示文字） */}
        <div className="flex items-center gap-0.5 min-w-0 shrink-0">
          <div className="relative">
            <button
              ref={expertTriggerRef as React.RefObject<HTMLButtonElement>}
              onClick={expertToggle}
              className={`flex items-center shrink-0 text-[12.5px] transition-colors rounded-md ${
                toolbarCompact ? "px-1.5 py-1" : "gap-1.5 px-2 py-1 max-w-[140px]"
              } ${
                p.expert
                  ? "text-[#185FA5] hover:bg-[#E6F1FB]/70 font-medium"
                  : "text-text-secondary hover:bg-bg-hover hover:text-text-primary"
              }`}
              title={p.expert ? `已雇佣：${p.expert.name}` : "雇佣专家"}
            >
              <Sparkles size={12} className={p.expert ? "text-[#185FA5]" : "text-text-muted"} />
              <span
                className={toolbarCompact ? "max-w-0 overflow-hidden whitespace-nowrap" : "truncate"}
              >
                {p.expert ? p.expert.name : "专家"}
              </span>
              {p.expert && (
                <span
                  role="button"
                  onClick={(e) => { e.stopPropagation(); p.setExpert(null); }}
                  className="ml-0.5 hover:opacity-70 shrink-0 text-text-muted"
                >
                  <X size={11} />
                </span>
              )}
            </button>
            {expertOpen && (
              <PopoverShell containerRef={expertContainerRef} triggerRef={expertTriggerRef} width="sm" align="left" side="top">
                {p.recentExperts.length > 0 ? (
                  <>
                    <PopoverBody maxHeight="max-h-52">
                      {p.recentExperts.slice(0, 5).map((e) => (
                        <button
                          key={e.id}
                          onClick={() => { p.setExpert(e.id); setExpertOpen(false); }}
                          className={`flex items-center gap-2.5 w-full px-2.5 py-2.5 text-left rounded-lg hover:bg-bg-hover transition-colors ${
                            p.expert?.id === e.id ? "bg-accent/10" : ""
                          }`}
                        >
                          <div className="w-7 h-7 rounded-lg bg-accent/20 text-accent flex items-center justify-center text-[12px] font-medium shrink-0">
                            {e.avatar || e.name.slice(0,2)}
                          </div>
                          <span className="text-[13px] text-text-primary truncate flex-1">{e.name}</span>
                          {p.expert?.id === e.id && <Check size={14} className="text-accent shrink-0" />}
                        </button>
                      ))}
                    </PopoverBody>
                    <div className="mx-2.5 my-1 border-t border-border-primary" />
                  </>
                ) : null}
                <button
                  onClick={() => { p.onOpenExperts?.(); setExpertOpen(false); }}
                  className="flex items-center justify-center w-full px-2.5 py-2 text-[11px] text-accent hover:bg-bg-hover rounded-md transition-colors"
                >
                  召唤专家
                </button>
              </PopoverShell>
            )}
          </div>

          {/* Workspace chip — v4 (2026-07-08)：
              · 未锁定（welcome / 新会话，无论是否已选工作区）：accent 色 +
                名字，可点开 popover（"添加新工作区 / 工作目录列表 / 不绑定目录"）。
                选完工作区后再次点击仍弹此列表，可重新选择或不绑定。
              · 锁定中（messages.length>0 || sending || isStreaming）：灰度
                禁用样式 + cursor-not-allowed + onClick 短路 + title 提示
                "对话进行中，工作区已锁定"。Lock 图标取代 FolderOpen。
              · 选择工作区后 popover 自动收起。 */}
          <div className="relative">
            <button
              ref={projTriggerRef as React.RefObject<HTMLButtonElement>}
              onClick={() => {
                if (p.workspaceLocked) return;
                // 2026-07-08 (user, 修订 v3): 无论是否已选工作区，点目录都弹
                // chooser 列表（可重新选/不绑定）。只在「对话已开始」时
                // workspaceLocked=true 禁用。
                projToggle();
              }}
              disabled={p.workspaceLocked}
              className={`flex items-center shrink-0 text-[12.5px] transition-colors rounded-md ${
                toolbarCompact ? "px-1.5 py-1" : "gap-1.5 px-2 py-1 max-w-[160px]"
              } ${
                p.workspaceLocked
                  ? "text-text-faint cursor-not-allowed opacity-60"
                  : p.activeWorkspace
                  ? "text-[#0F6E56] hover:bg-[#E1F5EE] font-medium cursor-pointer"
                  : "text-text-secondary hover:bg-bg-hover hover:text-text-primary cursor-pointer"
              }`}
              title={
                p.workspaceLocked
                  ? "对话进行中，工作区已锁定"
                  : p.activeWorkspace
                  ? `已限制到：${p.activeWorkspace.path}（点击可重新选择）`
                  : "限制当前对话的读写范围（推荐）"
              }
            >
              {p.workspaceLocked ? (
                <Lock size={12} className="text-text-faint" />
              ) : (
                <FolderOpen size={12} className={p.activeWorkspace ? "text-[#0F6E56]" : "text-text-faint"} />
              )}
              <span
                className={toolbarCompact ? "max-w-0 overflow-hidden whitespace-nowrap" : "truncate"}
              >
                {p.activeWorkspace ? p.activeWorkspace.name : "目录"}
              </span>
            </button>
            {projOpen && (
              <PopoverShell containerRef={projContainerRef} triggerRef={projTriggerRef} width="sm" side="top">
                <button
                  onClick={async () => { await p.onAddWorkspace(); setProjOpen(false); }}
                  className="flex items-center justify-center gap-1.5 w-full px-2.5 py-2 text-[12px] text-[#0F6E56] hover:bg-[#E1F5EE] rounded-lg transition-colors font-medium"
                  title="选择本地目录添加为工作区"
                >
                  添加新工作区
                </button>
                {p.workspaces.length > 0 && (
                  <>
                    <div className="my-1 border-t border-border-primary" />
                    <div className="px-2.5 py-1 text-[11.5px] text-text-secondary font-medium">工作目录</div>
                    <PopoverBody maxHeight="max-h-48">
                      {p.workspaces.map((ws) => (
                        <button
                          key={ws.id}
                          onClick={() => { p.onSelectWorkspace(ws.path); setProjOpen(false); }}
                          className={`flex items-center gap-2.5 w-full px-2.5 py-2.5 text-left rounded-lg hover:bg-bg-hover transition-colors ${
                            p.activeWorkspace?.id === ws.id ? "bg-[#E1F5EE]/60" : ""
                          }`}
                          title={ws.path}
                        >
                          <FolderOpen size={15} className="shrink-0 text-[#0F6E56] opacity-70" />
                          <div className="flex-1 min-w-0">
                            <div className="text-[12.5px] text-text-primary truncate">{ws.name}</div>
                            <div className="text-[10.5px] text-text-faint truncate">{ws.path}</div>
                          </div>
                          {p.activeWorkspace?.id === ws.id && <Check size={14} className="text-[#0F6E56] shrink-0" />}
                        </button>
                      ))}
                    </PopoverBody>
                    <div className="border-t border-border-primary" />
                    <button
                      onClick={() => { p.onSelectWorkspace(null); setProjOpen(false); }}
                      className="flex items-center justify-center w-full px-2.5 py-2 text-[11px] text-text-muted hover:bg-bg-hover rounded-lg transition-colors"
                    >
                      不绑定目录
                    </button>
                  </>
                )}
              </PopoverShell>
            )}
          </div>

          {/* Permission chip — 名称模式（永远显示文字） */}
          <div className="relative">
            <button
              ref={permTriggerRef as React.RefObject<HTMLButtonElement>}
              onClick={permToggle}
              className={`flex items-center shrink-0 text-[12.5px] transition-colors rounded-md ${
                toolbarCompact ? "px-1.5 py-1" : "gap-1.5 px-2 py-1 max-w-[160px]"
              } ${
                p.permissionLevel === "full"
                  ? "text-[#993C1D] hover:bg-[#FAECE7]/70 font-medium"
                  : "text-text-secondary hover:bg-bg-hover hover:text-text-primary"
              }`}
              title="权限级别"
            >
              {p.permissionLevel === "full"
                ? <AlertTriangle size={12} className="text-[#993C1D]" />
                : <ShieldCheck size={12} className="text-text-muted" />}
              <span
                className={toolbarCompact ? "max-w-0 overflow-hidden whitespace-nowrap" : "truncate"}
              >
                {p.permissionLevel === "full" ? "完全访问" : "默认权限"}
              </span>
            </button>
            {permOpen && (
              <PopoverShell containerRef={permContainerRef} triggerRef={permTriggerRef} width="md" side="top">
                <div className="px-2.5 py-1 text-[11.5px] text-text-secondary font-medium">权限级别</div>
                <PopoverBody maxHeight="max-h-48">
                  {PERMISSION_LEVELS.map((lvl) => (
                    <button
                      key={lvl.id}
                      onClick={() => { p.setPermissionLevel(lvl.id); setPermOpen(false); }}
                      className={`w-full px-2.5 py-3 text-left rounded-lg hover:bg-bg-hover transition-colors ${
                        p.permissionLevel === lvl.id ? "bg-[#F1EFE8]/60" : ""
                      }`}
                    >
                      <div className="flex items-start gap-2">
                        <span className="shrink-0 mt-0.5">
                          {lvl.id === "full"
                            ? <AlertTriangle size={16} className="text-[#993C1D]" />
                            : <ShieldCheck size={16} className="text-[#0F6E56]" />}
                        </span>
                        <div className="flex-1 min-w-0">
                          <div className="text-[12.5px] text-text-primary font-medium">{lvl.label}</div>
                          <div className="text-[10.5px] text-text-faint mt-0.5 leading-snug">{lvl.description}</div>
                        </div>
                        {p.permissionLevel === lvl.id && <Check size={14} className="text-[#185FA5] shrink-0 mt-0.5" />}
                      </div>
                    </button>
                  ))}
                </PopoverBody>
              </PopoverShell>
            )}
          </div>
        </div>

        {/* Right: model + history + voice/image + send.
         *
         * `ml-auto` pushes this group to the right edge of its row
         * (replacing the old `justify-between` behavior, which only
         * worked when left+right shared one row). Combined with the
         * parent `flex-wrap`, the whole group drops to a new line
         * cleanly when there isn't enough horizontal room — no more
         * dangling individual chips.
         */}
        <div className="flex items-center gap-0.5 ml-auto shrink-0">
          <div className="relative">
            <button
              ref={modelTriggerRef as React.RefObject<HTMLButtonElement>}
              onClick={(p.byProvider && p.byProvider.length > 0) ? modelToggle : () => { p.onOpenSettings?.(); setModelOpen(false); }}
              className={`flex items-center gap-1.5 px-2 py-1 text-[12.5px] transition-colors rounded-md max-w-[240px] ${
                (p.byProvider && p.byProvider.length > 0)
                  ? "text-text-secondary hover:bg-bg-hover hover:text-text-primary cursor-pointer"
                  : "text-[#993C1D] hover:bg-[#FAECE7]/60 cursor-pointer"
              }`}
              title={(p.byProvider && p.byProvider.length > 0) ? "切换模型" : "未配置模型，点击前往 设置 → 模型管理"}
            >
              <ProviderIcon providerId={p.selectedModel?.split(":")[0] || "custom"} size={14} title={p.selectedModel} />
              <span className="truncate">{p.currentDisplay}</span>
              <ChevronDown size={11} className="shrink-0 opacity-60" />
            </button>
            {modelOpen && (
              <PopoverShell containerRef={modelContainerRef} triggerRef={modelTriggerRef} align="right" width="md" side="top">
                <div className="px-3 pt-2.5 pb-2 flex items-center justify-between border-b border-border-primary mb-1">
                  <div className="flex items-center gap-2">
                    <span className="inline-block w-1 h-4 rounded-sm bg-accent" aria-hidden="true" />
                    <span className="text-[13px] text-text-primary font-semibold tracking-wide">切换模型</span>
                  </div>
                  <span className="text-[10.5px] text-text-muted font-mono">
                    {p.byProvider.reduce((acc, g) => acc + g.models.length, 0)} 个 · {p.byProvider.length} 厂商
                  </span>
                </div>
                <PopoverBody maxHeight="max-h-64">
                  {!p.byProvider || p.byProvider.length === 0 ? (
                    <div className="px-2.5 py-3 text-center">
                      <p className="text-[12px] text-text-muted">暂无可用模型</p>
                      <p className="text-[10.5px] text-text-muted mt-1">请到「设置 → 模型管理」添加</p>
                    </div>
                  ) : (
                    <>
                      {p.byProvider.map((group) => {
                        // Default: builtin expanded, custom collapsed.
                        // Auto-expand if the selected model lives in this provider
                        // so the user always sees what's currently active.
                        const isSelectedHere = p.selectedModel
                          ? p.selectedModel.startsWith(group.provider + ":")
                          : false;
                        const expanded =
                          expandedProviders[group.provider] ??
                          (!group.isCustom || isSelectedHere);
                        const currentModelDisplay = isSelectedHere
                          ? p.selectedModel!.slice(group.provider.length + 1)
                          : null;
                        return (
                          <div key={group.provider} className="mb-0.5">
                            <button
                              type="button"
                              onClick={() => toggleProvider(group.provider)}
                              className="flex items-center gap-2 w-full px-2 py-1.5 text-left rounded-md hover:bg-bg-hover transition-colors"
                              title={group.isCustom ? `自定义：${group.provider}（点击展开/折叠）` : `点击展开/折叠 ${group.provider}`}
                              aria-expanded={expanded}
                            >
                              {expanded
                                ? <ChevronDown size={11} className="text-text-muted shrink-0" />
                                : <ChevronRight size={11} className="text-text-muted shrink-0" />
                              }
                              <ProviderIcon providerId={group.provider} size={13} title={group.provider} />
                              <span className="text-[11.5px] text-text-secondary font-medium truncate">
                                {group.provider}
                                {group.isCustom && (
                                  <span className="ml-1 text-[10px] text-text-faint">· 自定义</span>
                                )}
                              </span>
                              <span className="text-[10px] text-text-faint font-mono ml-auto shrink-0">
                                {group.models.length}
                                {currentModelDisplay && (
                                  <span className="ml-1.5 text-text-muted truncate">
                                    · {currentModelDisplay}
                                  </span>
                                )}
                              </span>
                            </button>
                            {expanded && (
                              <div className="pl-4 pr-1 pb-1">
                                {group.models.map((m) => (
                                  <button
                                    key={m.id}
                                    onClick={() => { p.setSelectedModel(m.id); setModelOpen(false); }}
                                    className={`flex items-center gap-2 w-full px-2 py-2 text-left rounded-md hover:bg-bg-hover transition-colors ${
                                      p.selectedModel === m.id ? "bg-[#F1EFE8]/60" : ""
                                    }`}
                                  >
                                    <span className="truncate font-mono text-[12px] text-text-primary">{m.display}</span>
                                    {p.selectedModel === m.id && <Check size={13} className="text-[#185FA5] shrink-0" />}
                                  </button>
                                ))}
                              </div>
                            )}
                          </div>
                        );
                      })}
                    </>
                  )}
                </PopoverBody>
              </PopoverShell>
            )}
          </div>
          <VisionButton onFilesAttach={(files) => p.onFilesAttach?.(files)} />
          <VoiceButton onTranscribe={(text) => p.setInput(p.input + text)} />
          {/* PLAN_UI_ENHANCEMENT §1.3 — 润色按钮：覆盖原值，原文不计入上下文 */}
          <PolishButton
            draft={p.input}
            onResult={(polished) => p.setInput(polished)}
            disabled={p.sending || p.isStreaming}
          />

          {/* Send / Stop / Interrupt — v2 style: square 32×32, ink primary */}
          {p.isStreaming && p.messageQueue && p.messageQueue.length > 0 ? (
            <button
              onClick={() => p.onStop?.()}
              className="flex items-center justify-center w-8 h-8 rounded-lg transition-all hover:opacity-90"
              style={{
                background: "var(--color-error)",
                color: "white",
              }}
              title="插话：停止当前回复并发送队列消息"
            >
              <Square size={12} fill="currentColor" />
            </button>
          ) : p.isStreaming ? (
            <button
              onClick={() => p.onStop?.()}
              className="flex items-center justify-center w-8 h-8 rounded-lg transition-all hover:opacity-90"
              style={{
                background: "var(--color-error)",
                color: "white",
              }}
              title="停止生成"
            >
              <Square size={12} fill="currentColor" />
            </button>
          ) : (
            <button
              onClick={p.handleSubmit}
              disabled={!p.input.trim() && (!p.messageQueue || p.messageQueue.length === 0) || p.sending}
              className="flex items-center justify-center w-8 h-8 rounded-lg transition-all disabled:opacity-30 disabled:cursor-not-allowed"
              style={{
                background: (p.input.trim() || (p.messageQueue && p.messageQueue.length > 0)) ? "var(--color-accent)" : "var(--color-bg-tertiary)",
                color: (p.input.trim() || (p.messageQueue && p.messageQueue.length > 0)) ? "var(--color-accent-on-text)" : "var(--color-text-faint)",
              }}
              title="发送 (Enter)"
            >
              <Send size={13} />
            </button>
          )}
        </div>
      </div>
    </div>
  );
}


