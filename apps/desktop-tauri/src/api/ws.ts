// Hermix Desktop — streaming-channel helpers (stdio transport).
//
// History: this file used to expose `resolveWebSocketUrl()` /
// `resolveGroupChatWebSocketUrl()` which built `ws://127.0.0.1:3000/...`
// URLs and the streaming hooks opened raw `new WebSocket(...)` sockets.
// Migrated to the stdio bridge: every "WebSocket" is now a logical
// channel on the bridge's stdout, identified by name.
//
// Channel naming convention (matches the server-side router in
// `hermix-server/src/stdio_session.rs::spawn_connection_handler`):
//
//   * `stream:<session_id>`    — single-session streaming chat
//   * `groupchat:<room_id>`    — group chat dispatch + streaming
//   * any other string         — legacy "main" channel, runs
//                                `handle_ws` on the server
//
// The downstream side is handled by `listenStreamChannel` /
// `listenGroupChatChannel` — they wrap Tauri's `listen("stdio-frame")`
// and filter by channel name. Upstream frames go through
// `sendToStreamChannel` / `sendToGroupChatChannel`, which call
// `invoke("backend_channel_send")`.

import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";

/** Channel-name prefix that the server's stdio handler routes into
 *  `handle_ws_stream(session_id)`. Mirrors the prefix in
 *  `stdio_session.rs::spawn_connection_handler`. */
export const STREAM_CHANNEL_PREFIX = "stream:";
export const GROUPCHAT_CHANNEL_PREFIX = "groupchat:";

/** Build the channel name for a single-session stream. The same string
 *  is reused for every frame sent on this session's upstream pipe,
 *  so the server-side `connections` map keeps the handler alive for
 *  the lifetime of the stream. */
export function streamChannelName(sessionId: string): string {
  return `${STREAM_CHANNEL_PREFIX}${sessionId}`;
}

export function groupchatChannelName(roomId: string): string {
  return `${GROUPCHAT_CHANNEL_PREFIX}${roomId}`;
}

/** Subscribe to all frames whose `channel` matches the given exact
 *  name. The Tauri event `stdio-frame` carries a `FrontendFrame`
 *  (`{ channel, payload }`) where `payload` is a JSON-encoded string
 *  the caller parses for the actual stream event (chunk / done /
 *  approval_request / etc.). The unlisten fn tears down the listener
 *  when the stream ends. */
export async function listenStreamChannel(
  channel: string,
  onFrame: (payloadText: string) => void,
): Promise<UnlistenFn> {
  const unlisten = await listen<{ channel: string; payload: string }>(
    "stdio-frame",
    (event) => {
      if (event.payload.channel !== channel) return;
      onFrame(event.payload.payload);
    },
  );
  return unlisten;
}

/** Send a JSON Lines frame upstream on a non-HTTP channel. The
 *  payload is the verbatim JSON the server expects on that channel —
 *  `SendMessageRequest` for `stream:<sid>`, `{ type: "send", text }`
 *  for `groupchat:<room>`, etc. The bridge does NOT wrap or parse
 *  the payload, so callers own their protocol shape. */
export async function sendToStreamChannel(
  channel: string,
  payload: unknown,
): Promise<void> {
  const text = typeof payload === "string" ? payload : JSON.stringify(payload);
  await invoke<void>("backend_channel_send", { channel, payload: text });
}

/** Legacy wrapper kept for the existing `sendMessageStream` call
 *  sites — the user-facing stream REST POST that the bridge replies
 *  to with the first WS chunk on the `stream:<sid>` channel. */
export const sendMessageStream = (id: string, content: string) =>
  // Re-export from `./rest` so callers that do
  // `import { sendMessageStream } from "../api"` keep working
  // without a parallel import path.
  // The actual implementation is the same as `sendMessage` — it
  // POSTs to `/v1/sessions/:id/messages` and the server opens the
  // `stream:<id>` channel for the reply frames.
  import("./rest").then(({ sendMessage }) => sendMessage(id, content));
