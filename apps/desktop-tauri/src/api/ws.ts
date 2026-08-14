// Hermix Desktop API Client — WebSocket 相关（从 api.ts 拆分；REST 见 ./rest）
import { WS_BASE, sendMessage } from "./rest";

// ── WebSocket Streaming ────────────────────────────

export function resolveWebSocketUrl(sessionId: string): string {
  return `${WS_BASE}/v1/ws-stream/${encodeURIComponent(sessionId)}`;
}

/** WebSocket URL for the group chat streaming endpoint.
 *  Opens a new connection per message. */
export function resolveGroupChatWebSocketUrl(roomId: string): string {
  return `${WS_BASE}/v1/ws-groupchat/${encodeURIComponent(roomId)}`;
}

export const sendMessageStream = (id: string, content: string) => sendMessage(id, content);
