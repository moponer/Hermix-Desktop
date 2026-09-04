import { listenStreamChannel, streamChannelName } from "../api";
import type { StreamChatSendContext } from "./useStreamParser";

/**
 * Build the `stream:<sessionId>` subscription factory (`doConnect`).
 *
 * Migrated to the stdio bridge: instead of opening a raw `WebSocket`
 * with retry/backoff, we register a single `listen("stdio-frame")`
 * filter on the bridge and store the unlisten fn in `ctx.wsRef`.
 * The bridge owns the underlying transport — there is no per-channel
 * reconnect to worry about, because the bridge is single-connection;
 * if the sidecar dies the watchdog restarts the whole process and
 * the React side will see the new channel as a fresh send (a re-render
 * of the chat panel picks up the new subscription automatically).
 *
 * Compared to the TCP-era version, this drops:
 *   * `MAX_RETRIES` / `RETRY_DELAYS_MS` — there is nothing to retry
 *     against; a bridge round-trip either succeeds or returns an
 *     error.
 *   * `ws.onopen` / `onerror` / `onclose` — the bridge emits a single
 *     `stdio-frame` event per server frame and never closes the
 *     stream from underneath us while the sidecar is alive.
 *   * `hasReportedLost` / `reportLost` / `reportRestored` — there is
 *     no observable "lost → restored" cycle at the channel layer.
 *     If the sidecar dies, the user sees the splash screen toast
 *     from `BackendStatus` instead of an in-stream banner.
 *
 * The returned `connect()` is still a no-arg function so the caller
 * (`useStreamChat.ts`) can fire-and-forget it from inside its send
 * closure without restructuring.
 */
export function createConnector(
  ctx: StreamChatSendContext,
  onMessage: (payloadText: string) => void,
): () => void {
  const connect = () => {
    // Stale retry guard — if a new `send()` has started during the
    // tiny window between `connect()` being scheduled and the
    // listener actually registering, abort the subscription so we
    // don't leak listeners for a dead send.
    if (ctx.activeSendIdRef.current !== ctx.mySendId) return;

    // Single async registration — wrap in an IIFE so the rest of
    // the function can stay synchronous-looking and matches the
    // shape the caller expects.
    void (async () => {
      try {
        const unlisten = await listenStreamChannel(
          streamChannelName(ctx.sessionId),
          (payloadText) => {
            if (ctx.activeSendIdRef.current !== ctx.mySendId) return;
            onMessage(payloadText);
          },
        );

        // Another send replaced us while we were awaiting the
        // listener — drop immediately so we don't accumulate
        // subscriptions for dead sessions.
        if (ctx.activeSendIdRef.current !== ctx.mySendId) {
          unlisten();
          return;
        }

        ctx.wsRef.current = unlisten;
        ctx.wsSessionIdRef.current = ctx.sessionId;
      } catch (e) {
        // Listener registration failed (bridge not initialised,
        // window torn down, etc.). Surface the failure to the
        // caller so the UI can show "网络中断" NOW instead of
        // waiting on the server-side 180s hard timeout.
        if (ctx.activeSendIdRef.current !== ctx.mySendId) return;
        ctx.settle(
          "",
          e instanceof Error ? e.message : String(e),
          undefined,
          "error",
        );
      }
    })();
  };

  return connect;
}
