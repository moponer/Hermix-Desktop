import { resolveWebSocketUrl } from "../api";
import type { StreamChatSendContext } from "./useStreamParser";

/** Auto-retry cap for WS failures that occur before any content arrives. */
const MAX_RETRIES = 3;
/** Backoff steps between retry attempts (ms). */
const RETRY_DELAYS_MS = [500, 1000, 2000];

/**
 * Build the WS connection factory (`doConnect`) as a standalone function.
 *
 * This is a verbatim lift of the original `doConnect` body from
 * `useStreamChat.ts`: it opens `new WebSocket(resolveWebSocketUrl(...))`,
 * wires the supplied `onMessage` handler plus the retry-aware
 * `onerror` / `onclose` handlers, and (before any content arrives) retries
 * up to `MAX_RETRIES` times. Once content has arrived (`hasAnyStreamEvent`
 * / `hasContent` / `sentToServerRef`), it stops retrying and settles
 * "partial" / "error" instead — duplicating a live agent loop would double
 * the assistant reply.
 *
 * Returns the `connect` function so the caller can both kick off the first
 * attempt and schedule retries (`setTimeout(connect, delay)`).
 */
export function createConnector(
  ctx: StreamChatSendContext,
  onMessage: (evt: MessageEvent) => void,
): () => void {
  const connect = () => {
    // Stale retry guard — if a new `send()` has started during the retry
    // delay, abort the reconnection attempt so we don't overwrite the new
    // send's WS in `wsRef.current`.
    if (ctx.activeSendIdRef.current !== ctx.mySendId) return;
    try {
      const ws = new WebSocket(resolveWebSocketUrl(ctx.sessionId));
      ctx.wsRef.current = ws;
      ctx.wsSessionIdRef.current = ctx.sessionId;

      // Approval responses are routed back to the original
      // `callbacks.onApprovalRequest(...)` Promise through a closure; the
      // request id flows in the WS frame and the corresponding resolver lives
      // in `App.tsx` via `approvalResolversRef`. Stale resolver cleanup
      // therefore happens in App.tsx when a stream settles or the session
      // changes — not here.

      ws.onmessage = onMessage;

      ws.onerror = () => {
        // Stale WS guard.
        if (ctx.activeSendIdRef.current !== ctx.mySendId) return;
        if (!ctx.doneRef.current) {
          // 流式进行中（已收到内容/已发给后端）断连：自动重连续跑而非
          // settle("partial") 卡住。后端 takeover 会 abort 仍在 grace
          // 宽限期内盲跑的旧 agent，新 send 从持久化历史续跑；重连耗尽才
          // 保留 partial。
          if (ctx.hasAnyStreamEvent || ctx.hasContent) {
            if (ctx.sentToServerRef.current && ctx.retryCount < MAX_RETRIES) {
              ctx.retryCount++;
              const delay = RETRY_DELAYS_MS[Math.min(ctx.retryCount - 1, RETRY_DELAYS_MS.length - 1)];
              ctx.updatePhase("reconnecting");
              ctx.wsRef.current = null;
              setTimeout(connect, delay);
              return;
            }
            ctx.settle(ctx.fullText, undefined, undefined, "partial");
            return;
          }
          // if the message was already sent to the server (we received
          // `connected` and called `ws.send(payload)`), DO NOT retry — the
          // server is already processing it.
          if (ctx.sentToServerRef.current) {
            ctx.settle("", "Connection lost while server was processing the message", undefined, "error");
            return;
          }
          if (ctx.retryCount < MAX_RETRIES) {
            ctx.retryCount++;
            const delay = RETRY_DELAYS_MS[Math.min(ctx.retryCount - 1, RETRY_DELAYS_MS.length - 1)];
            ctx.updatePhase("reconnecting");
            ctx.wsRef.current = null;
            setTimeout(connect, delay);
            return;
          }
          ctx.settle("", "WebSocket error", undefined, "error");
        }
      };
      ws.onclose = () => {
        // Stale WS guard — this is the critical fix. When settle() closes the
        // WS, the `onclose` event is queued as a macro task. By the time it
        // fires, a new `send()` may have already started.
        if (ctx.activeSendIdRef.current !== ctx.mySendId) return;
        if (!ctx.doneRef.current) {
          if (ctx.hasAnyStreamEvent || ctx.hasContent) {
            // 流式进行中（已收到内容/已发给后端）断连：自动重连续跑，
            // 不再 settle("partial") 卡住让用户手动发消息。后端 takeover
            // 会 abort 仍在 grace 宽限期内盲跑的旧 agent，新 send 从
            // 持久化历史续跑；重连次数耗尽才保留 partial。
            if (ctx.sentToServerRef.current && ctx.retryCount < MAX_RETRIES) {
              ctx.retryCount++;
              const delay = RETRY_DELAYS_MS[Math.min(ctx.retryCount - 1, RETRY_DELAYS_MS.length - 1)];
              ctx.updatePhase("reconnecting");
              ctx.wsRef.current = null;
              setTimeout(connect, delay);
              return;
            }
            ctx.settle(ctx.fullText, undefined, undefined, "partial");
          } else {
            // same hardening as onerror — if the message was already sent,
            // don't retry.
            if (ctx.sentToServerRef.current) {
              ctx.settle("", "Connection closed while server was processing the message", undefined, "error");
              return;
            }
            if (ctx.retryCount < MAX_RETRIES) {
              ctx.retryCount++;
              const delay = RETRY_DELAYS_MS[Math.min(ctx.retryCount - 1, RETRY_DELAYS_MS.length - 1)];
              ctx.updatePhase("reconnecting");
              ctx.wsRef.current = null;
              setTimeout(connect, delay);
              return;
            }
            ctx.settle("", "Connection closed", undefined, "error");
          }
        }
        // Note: stale approval resolvers are cleared by App.tsx when
        // `streamingSessionId` flips to null. We deliberately do not
        // re-implement that cleanup here — `useStreamChat` doesn't own those
        // resolvers, App.tsx does.
      };
    } catch (e) {
      // WS constructor threw (unlikely — URL parsing error, etc.) Try once
      // more before settling.
      if (!ctx.hasAnyStreamEvent && !ctx.hasContent && ctx.retryCount < MAX_RETRIES) {
        ctx.retryCount++;
        const delay = RETRY_DELAYS_MS[Math.min(ctx.retryCount - 1, RETRY_DELAYS_MS.length - 1)];
        ctx.updatePhase("reconnecting");
        ctx.wsRef.current = null;
        setTimeout(connect, delay);
        return;
      }
      ctx.settle("", e instanceof Error ? e.message : String(e));
    }
  };

  return connect;
}
