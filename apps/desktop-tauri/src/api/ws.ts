// Hermix Desktop — streaming-channel helpers (stdio transport).
//
// History: this file used to expose `resolveWebSocketUrl()` which
// built `ws://127.0.0.1:3000/...`
// URLs and the streaming hooks opened raw `new WebSocket(...)` sockets.
// Migrated to the stdio bridge: every "WebSocket" is now a logical
// channel on the bridge's stdout, identified by name.
//
// Channel naming convention (matches the server-side router in
// `hermix-server/src/stdio_session.rs::spawn_connection_handler`):
//
//   * `stream:<session_id>`    — single-session streaming chat
//   * any other string         — legacy "main" channel, runs
//                                `handle_ws` on the server
//
// The downstream side is handled by `listenStreamChannel` — it wraps
// Tauri's `listen("stdio-frame")`
// and filters by channel name. Upstream frames go through
// `sendToStreamChannel`, which calls
// `invoke(IPC.backendChannelSend)`.

import { listen, type UnlistenFn } from "@tauri-apps/api/event";

/** Channel-name prefix that the server's stdio handler routes into
 *  `handle_ws_stream(session_id)`. Mirrors the prefix in
 *  `stdio_session.rs::spawn_connection_handler`. */
export const STREAM_CHANNEL_PREFIX = "stream:";

/** Build the channel name for a single-session stream. The same string
 *  is reused for every frame sent on this session's upstream pipe,
 *  so the server-side `connections` map keeps the handler alive for
 *  the lifetime of the stream. */
export function streamChannelName(sessionId: string): string {
  return `${STREAM_CHANNEL_PREFIX}${sessionId}`;
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

// sendToStreamChannel 于 2026-10-18 移除：调用点全部直接
// `invoke(IPC.backendChannelSend, …)`（useStreamParser /
// useStreamChat），封装零调用、徒增一层间接。

/** Send a JSON Lines frame upstream on a non-HTTP channel. Callers
 *  invoke `backend_channel_send` directly — the bridge does NOT wrap
 *  or parse the payload, so callers own their protocol shape. */

