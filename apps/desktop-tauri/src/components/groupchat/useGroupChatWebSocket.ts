/**
 * useGroupChatWebSocket — WebSocket hook for group chat streaming.
 *
 * Extracted from GroupChatView.tsx during the 2026-07 refactor.
 *
 * Manages the one-shot WS connection lifecycle: opens a connection,
 * sends the user message, processes incoming stream events (chunks,
 * agent done, task-engine events, dispatch failures, handoffs), and
 * cleans up on completion or error.
 *
 * The hook owns:
 *   - `messages` — the chat message list (local optimistic + streamed)
 *   - `streamingAgents` — per-agent typing indicator state
 *   - `loading` — whether a send is in flight
 *   - `inlineEvents` — transient dispatch errors + handoff separators
 *
 * The caller provides `onEngineEvent` for forwarding task-engine events
 * to the TaskPanel. Agent display-name resolution is handled by the
 * rendering layer (MessageBubble), not this hook.
 */

import { useState, useCallback, useRef, useEffect } from "react";
import * as api from "@/api";
import type { TaskEngineEvent } from "../TaskPanel";
import {
  type ChatMsg,
  type InlineEvent,
  apiMsgToChatMsg,
  LEADER_SENDER_ID,
} from "./types";

export interface UseGroupChatWebSocketReturn {
  messages: ChatMsg[];
  streamingAgents: Map<string, { name: string; role: "leader" | "member" }>;
  loading: boolean;
  inlineEvents: InlineEvent[];
  sendMessage: (text: string) => Promise<void>;
  loadMessages: (roomId: string) => Promise<void>;
  clearRoom: () => void;
  setMessages: React.Dispatch<React.SetStateAction<ChatMsg[]>>;
}

export function useGroupChatWebSocket(
  activeRoomId: string | null,
  onEngineEvent: (roomId: string, ev: TaskEngineEvent) => void,
): UseGroupChatWebSocketReturn {
  const [messages, setMessages] = useState<ChatMsg[]>([]);
  const [streamingAgents, setStreamingAgents] = useState<
    Map<string, { name: string; role: "leader" | "member" }>
  >(() => new Map());
  const [loading, setLoading] = useState(false);
  const [inlineEvents, setInlineEvents] = useState<InlineEvent[]>([]);
  const inlineEventCounter = useRef(0);

  const loadMessages = useCallback(async (roomId: string) => {
    try {
      const apiMsgs = await api.getGroupChatMessages(roomId);
      setMessages(apiMsgs.map(apiMsgToChatMsg));
    } catch {
      setMessages([]);
    }
  }, []);

  const clearRoom = useCallback(() => {
    setMessages([]);
    setStreamingAgents(new Map());
    setInlineEvents([]);
  }, []);

  // Load messages when room changes
  useEffect(() => {
    if (activeRoomId) {
      loadMessages(activeRoomId);
    } else {
      clearRoom();
    }
    // Clear streaming state on room switch
    setStreamingAgents(new Map());
    setInlineEvents([]);
  }, [activeRoomId, loadMessages, clearRoom]);

  const sendMessage = useCallback(
    async (text: string) => {
      if (!text.trim() || !activeRoomId) return;

      setLoading(true);

      // Optimistic local message
      const localId = `local-${Date.now()}`;
      const localMsg: ChatMsg = {
        id: localId,
        roomId: activeRoomId,
        senderId: "user",
        senderName: "你",
        content: text,
        messageType: "chat",
        timestamp: Date.now(),
      };
      setMessages((prev) => [...prev, localMsg]);

      let ws: WebSocket | null = null;
      try {
        ws = new WebSocket(api.resolveGroupChatWebSocketUrl(activeRoomId));

        // Wait for the socket to open
        await new Promise<void>((resolve, reject) => {
          if (!ws) return reject(new Error("ws is null"));
          ws.onerror = () => reject(new Error("WebSocket connection failed"));
          const to = setTimeout(() => reject(new Error("WebSocket timeout")), 10_000);
          ws.onopen = () => {
            clearTimeout(to);
            resolve();
          };
        });

        // Send the user message
        ws.send(JSON.stringify({ type: "send", text }));

        // In-flight streaming messages keyed by agent_id
        const agentMsgs = new Map<string, ChatMsg>();

        // Process incoming events
        await new Promise<void>((resolve, reject) => {
          if (!ws) return reject(new Error("ws is null"));
          let settled = false;
          const settle = (err?: Error) => {
            if (settled) return;
            settled = true;
            if (err) reject(err);
            else resolve();
          };
          ws.onerror = () => settle(new Error("WebSocket error"));
          ws.onclose = () => settle();
          ws.onmessage = (event) => {
            try {
              const env = JSON.parse(event.data);
              const raw = env?.event;
              if (!raw || typeof raw.type !== "string") return;
              // Backend serializes `StreamEvent` via `#[serde(tag = "type")]`
              // inside `WsEnvelope`, i.e. `{ "type": "group_chat_chunk",
              // "agent_id": …, … }` (flat). The handlers below were written
              // for the internally-nested shape `{ "group_chat_chunk": { … } }`,
              // so normalize once here. This matches the convention already
              // used by `useStreamChat` (`data.event.type`).
              const evt: any = { [raw.type]: raw };

              if (evt.connected) return;

              // ── Streaming chunk ──
              if (evt.group_chat_chunk) {
                const { agent_id, agent_name, content } = evt.group_chat_chunk;
                const isLeaderChunk = agent_id === LEADER_SENDER_ID;
                setStreamingAgents((prev) => {
                  if (prev.has(agent_id)) return prev;
                  const next = new Map(prev);
                  next.set(agent_id, {
                    name: agent_name || (isLeaderChunk ? "项目总管" : agent_id),
                    role: isLeaderChunk ? "leader" : "member",
                  });
                  return next;
                });
                // Accumulate ALL chunks (leader + member) as visible messages.
                // The leader's streamed text IS the reply — thinking traces
                // are sent separately via GroupChatThinking events (which
                // we intentionally don't render as chat messages).
                // NOTE: `agentMsgs` is mutated OUTSIDE the setMessages updater so the
                // updater stays pure. React 18 StrictMode double-invokes updaters;
                // mutating shared state inside one would double-append streamed text.
                const existing = agentMsgs.get(agent_id);
                const nextMsg: ChatMsg = existing
                  ? { ...existing, content: existing.content + content }
                  : {
                      id: `stream-${agent_id}-${Date.now()}`,
                      roomId: activeRoomId,
                      senderId: agent_id,
                      senderName: agent_name || (isLeaderChunk ? "项目总管" : agent_id),
                      content,
                      messageType: "chat",
                      timestamp: Date.now(),
                    };
                agentMsgs.set(agent_id, nextMsg);
                setMessages((prev) =>
                  existing
                    ? prev.map((m) => (m.id === existing.id ? nextMsg : m))
                    : [...prev, nextMsg],
                );
                return;
              }

              // ── Agent done ──
              if (evt.group_chat_agent_done) {
                const { agent_id } = evt.group_chat_agent_done;
                setStreamingAgents((prev) => {
                  if (!prev.has(agent_id)) return prev;
                  const next = new Map(prev);
                  next.delete(agent_id);
                  return next;
                });
                return;
              }

              // ── Dispatch failed (2026-07 refactor) ──
              if (evt.group_chat_dispatch_failed) {
                const { agent_name, reason, is_leader } = evt.group_chat_dispatch_failed;
                const id = `dispatch-failed-${inlineEventCounter.current++}`;
                setInlineEvents((prev) => [
                  ...prev,
                  {
                    id,
                    kind: "dispatch_failed" as const,
                    agentName: agent_name,
                    reason,
                    isLeader: is_leader,
                  },
                ]);
                return;
              }

              // ── Handoff (2026-07 refactor) ──
              if (evt.group_chat_handoff) {
                const { to_agent_name, reason, kind } = evt.group_chat_handoff;
                const id = `handoff-${inlineEventCounter.current++}`;
                setInlineEvents((prev) => [
                  ...prev,
                  {
                    id,
                    kind: "handoff" as const,
                    agentName: "",
                    toAgentName: to_agent_name,
                    reason,
                    isLeader: false,
                    handoffKind: kind,
                  },
                ]);
                return;
              }

              // ── Task-engine events ──
              if (evt.task_phase_change) {
                const { node_id, description, phase, assigned_agent } = evt.task_phase_change;
                onEngineEvent(activeRoomId, {
                  kind: "task_phase_change",
                  node_id,
                  description,
                  phase,
                  assigned_agent,
                });
                return;
              }
              if (evt.bus_delivered) {
                const { agent_id, message_kind, text } = evt.bus_delivered;
                onEngineEvent(activeRoomId, {
                  kind: "bus_delivered",
                  agent_id,
                  message_kind,
                  text,
                });
                return;
              }
              if (evt.audit_verdict) {
                const { agent_id, violation_id, level, score_delta, score_after, reason } =
                  evt.audit_verdict;
                onEngineEvent(activeRoomId, {
                  kind: "audit_verdict",
                  agent_id,
                  violation_id,
                  level,
                  score_delta,
                  score_after,
                  reason,
                });
                // Only violations (not benign scoring updates) get an
                // inline chip — keeps the stream free of low-signal noise.
                if (level === "violation") {
                  setInlineEvents((prev) => [
                    ...prev,
                    {
                      id: `audit-verdict-${inlineEventCounter.current++}`,
                      kind: "audit_verdict" as const,
                      agentName: agent_id,
                      reason,
                      isLeader: false,
                      auditLevel: level,
                      auditScoreDelta: score_delta,
                    },
                  ]);
                }
                return;
              }
              if (evt.agent_expelled) {
                const { agent_id, agent_name, reason } = evt.agent_expelled;
                onEngineEvent(activeRoomId, {
                  kind: "agent_expelled",
                  agent_id,
                  agent_name,
                  reason,
                });
                // High-signal event — surface in the chat stream too so
                // the user knows the team shrunk even with TaskPanel closed.
                setInlineEvents((prev) => [
                  ...prev,
                  {
                    id: `agent-expelled-${inlineEventCounter.current++}`,
                    kind: "agent_expelled" as const,
                    agentName: agent_name,
                    reason,
                    isLeader: false,
                  },
                ]);
                return;
              }
              if (evt.room_completed) {
                onEngineEvent(activeRoomId, { kind: "room_completed" });
                // Inline ribbon: high-signal "task finished" marker so the
                // user sees the outcome in the chat stream even if they
                // never opened the TaskPanel. task_phase_change events are
                // intentionally NOT injected here — one per subtask would
                // flood the stream with low-value noise.
                setInlineEvents((prev) => [
                  ...prev,
                  {
                    id: `task-completed-${inlineEventCounter.current++}`,
                    kind: "task_completed" as const,
                    agentName: "",
                    reason: "任务已完成",
                    isLeader: true,
                  },
                ]);
                return;
              }

              // ── Discussion events (P3.1) ──
              if (evt.discussion_started) {
                const { discussion_id, topic, participants, max_rounds } = evt.discussion_started;
                onEngineEvent(activeRoomId, {
                  kind: "discussion_started",
                  discussion_id,
                  topic,
                  participants,
                  max_rounds,
                });
                return;
              }
              if (evt.discussion_chunk) {
                const { agent_id, agent_name, delta } = evt.discussion_chunk;
                const isLeaderChunk = agent_id === LEADER_SENDER_ID;
                setStreamingAgents((prev) => {
                  if (prev.has(agent_id)) return prev;
                  const next = new Map(prev);
                  next.set(agent_id, {
                    name: agent_name || (isLeaderChunk ? "项目总管" : agent_id),
                    role: isLeaderChunk ? "leader" : "member",
                  });
                  return next;
                });
                // Accumulate discussion chunks as visible messages.
                const existing = agentMsgs.get(agent_id);
                const nextMsg: ChatMsg = existing
                  ? { ...existing, content: existing.content + delta }
                  : {
                      id: `discussion-${agent_id}-${Date.now()}`,
                      roomId: activeRoomId,
                      senderId: agent_id,
                      senderName: agent_name || (isLeaderChunk ? "项目总管" : agent_id),
                      content: delta,
                      messageType: "discussion",
                      timestamp: Date.now(),
                    };
                agentMsgs.set(agent_id, nextMsg);
                setMessages((prev) =>
                  existing
                    ? prev.map((m) => (m.id === existing.id ? nextMsg : m))
                    : [...prev, nextMsg],
                );
                return;
              }
              if (evt.discussion_message_saved) {
                // A participant's full message is saved — nothing to do
                // here since we already streamed it chunk-by-chunk above.
                // The persisted version will be fetched on reload.
                return;
              }
              if (evt.discussion_round_summary) {
                const { round, summary, next_action } = evt.discussion_round_summary;
                const id = `round-summary-${inlineEventCounter.current++}`;
                setInlineEvents((prev) => [
                  ...prev,
                  {
                    id,
                    kind: "task_completed" as const,
                    agentName: "",
                    reason: `第${round}轮 · ${summary} · ${next_action}`,
                    isLeader: true,
                  },
                ]);
                return;
              }
              if (evt.discussion_concluded) {
                const { total_rounds } = evt.discussion_concluded;
                onEngineEvent(activeRoomId, {
                  kind: "discussion_concluded",
                  total_rounds,
                });
                setInlineEvents((prev) => [
                  ...prev,
                  {
                    id: `discussion-concluded-${inlineEventCounter.current++}`,
                    kind: "task_completed" as const,
                    agentName: "",
                    reason: `讨论结束（共${total_rounds}轮）`,
                    isLeader: true,
                  },
                ]);
                return;
              }

              if (evt.group_chat_done) {
                settle();
                return;
              }

              if (evt.error) {
                settle(new Error(evt.error.message || "Stream error"));
                return;
              }
            } catch {
              // Ignore parse errors on individual events
            }
          };
        });

        // Close WS
        ws.close();

        // Reload messages from REST to get persisted versions
        try {
          const apiMsgs = await api.getGroupChatMessages(activeRoomId);
          setMessages(apiMsgs.map(apiMsgToChatMsg));
        } catch {
          // Keep streaming messages if reload fails
        }
      } catch (e) {
        // Rollback optimistic message
        setMessages((prev) => prev.filter((m) => m.id !== localId));
        console.error("GroupChat streaming failed:", e);
      } finally {
        ws?.close();
        setLoading(false);
        setStreamingAgents(new Map());
      }
    },
    [activeRoomId, onEngineEvent],
  );

  return {
    messages,
    streamingAgents,
    loading,
    inlineEvents,
    sendMessage,
    loadMessages,
    clearRoom,
    setMessages,
  };
}
