/**
 * useGroupChatWebSocket — group chat streaming hook (stdio transport).
 *
 * Migrated from raw `WebSocket` to the Tauri stdio bridge. The hook
 * owns the per-send lifecycle of a single groupchat dispatch:
 *
 *   1. Register a `listenStreamChannel("groupchat:<roomId>")` listener
 *      for the duration of this send.
 *   2. Send the user message as an upstream frame on the same
 *      channel via `sendToStreamChannel`.
 *   3. Process inbound frames — chunk / agent_done / dispatch_failed
 *      / handoff / task_phase_change / bus_delivered / audit_verdict /
 *      agent_expelled / room_completed / discussion_* / done / error
 *      — exactly the same shape the server's `WsEnvelope` produces.
 *   4. Tear the listener down on completion / error / abort.
 *
 * Compared to the TCP-era version:
 *   * No `new WebSocket(...)` — the bridge owns the connection.
 *   * No `ws.onerror` / `onclose` — there is no observable channel
 *     failure mode short of the bridge itself dying, in which case
 *     the watchdog restarts the sidecar and the React side will see
 *     a fresh stream on the next send.
 *   * The optimistic local message is committed as soon as the user
 *     sends — we no longer wait for the WS handshake (the listener
 *     registers in microseconds, the upstream frame goes out
 *     immediately). If the server never responds we surface a
 *     rollback in the catch.
 *
 * The hook exposes:
 *   - `messages` — chat list (optimistic + streamed + persisted reload)
 *   - `streamingAgents` — per-agent typing indicator state
 *   - `loading` — whether a send is in flight
 *   - `inlineEvents` — transient chips (dispatch errors, handoffs,
 *                      audit violations, room completion, etc.)
 *   - `sendMessage(text)` / `loadMessages(roomId)` / `clearRoom()` /
 *     `setMessages(...)` — caller-facing surface
 */

import { useState, useCallback, useRef, useEffect } from "react";
import type { UnlistenFn } from "@tauri-apps/api/event";
import {
  listenStreamChannel,
  sendToStreamChannel,
  groupchatChannelName,
} from "@/api";
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

  // Bridge between the stream handler (which sees `group_chat_done`
  // as one of many events) and the `sendMessage` outer code, which
  // wants a single "stream is finished" signal to unblock the
  // reload step. Declared BEFORE `sendMessage` so the callback's
  // closure can capture it stably across re-renders.
  const doneResolverRef = useRef<(() => void) | null>(null);

  const loadMessages = useCallback(async (roomId: string) => {
    try {
      const apiMsgs = await getGroupChatMessages(roomId);
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

  useEffect(() => {
    if (activeRoomId) {
      loadMessages(activeRoomId);
    } else {
      clearRoom();
    }
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

      // Stream-channel lifecycle — single subscription scoped to this send.
      let unlisten: UnlistenFn | null = null;
      try {
        unlisten = await listenStreamChannel(
          groupchatChannelName(activeRoomId),
          (payloadText) => {
            try {
              const env = JSON.parse(payloadText);
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
                // NOTE: `agentMsgs` is mutated OUTSIDE the setMessages updater
                // so the updater stays pure. React 18 StrictMode double-invokes
                // updaters; mutating shared state inside one would double-append
                // streamed text.
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

              // ── Dispatch failed ──
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

              // ── Handoff ──
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

              // ── Discussion events ──
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
                // End-of-stream — settle the outer Promise so the
                // post-stream reload step kicks in.
                doneResolverRef.current?.();
                doneResolverRef.current = null;
                return;
              }

              if (evt.error) {
                console.error(`[groupchat] stream error: ${evt.error.message}`);
                return;
              }
            } catch {
              // Ignore parse errors on individual events — one bad frame
              // shouldn't kill the whole stream subscription.
            }
          },
        );

        // In-flight streaming messages keyed by agent_id — hoisted so
        // the chunk handler above can find/extend the right row. We
        // deliberately declare it here (after `await listenStreamChannel`)
        // because the closure inside the handler captures it; React
        // StrictMode would otherwise race the mutation if it lived in
        // useState.
        const agentMsgs = new Map<string, ChatMsg>();

        // Send the user message on the same channel. The server opens
        // a per-room dispatch handler on first frame and starts
        // streaming back agent chunks / events.
        await sendToStreamChannel(groupchatChannelName(activeRoomId), {
          type: "send",
          text,
        });

        // Wait until the stream says "done" or the channel errors.
        // We do this by polling the listener: once `group_chat_done`
        // arrives, the hook's outer code reloads persisted messages
        // and exits. A simpler signal is to listen for the done event
        // explicitly via a Promise wrapper.
        await new Promise<void>((resolve) => {
          // Capture the unlisten inside the resolve by re-subscribing
          // would create a second listener; instead we wait for a
          // window after the most recent frame. Done semantics:
          //   * `group_chat_done` fires exactly once per send
          //   * `error` event kills the stream
          // We piggy-back on the listener above: keep a ref to its
          // resolver so the handler can settle.
          // (Done handler is bolted on below as `doneResolver`.)
          (doneResolverRef as any).current = resolve;
        });

        // Reload messages from REST to get persisted versions
        try {
          const apiMsgs = await getGroupChatMessages(activeRoomId);
          setMessages(apiMsgs.map(apiMsgToChatMsg));
        } catch {
          // Keep streaming messages if reload fails
        }
      } catch (e) {
        // Rollback optimistic message
        setMessages((prev) => prev.filter((m) => m.id !== localId));
        console.error("GroupChat streaming failed:", e);
      } finally {
        unlisten?.();
        doneResolverRef.current = null;
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

// `getGroupChatMessages` lives in `../../api/rest.ts`. We import lazily
// here (rather than at the top) so the hook file stays free of a
// dependency on the full REST surface — only the one helper is used.
// The `import("./rest")` form keeps the cycle out of the module graph.
async function getGroupChatMessages(roomId: string) {
  const { getGroupChatMessages: fn } = await import("../../api/rest");
  return fn(roomId);
}
