import { listenStreamChannel, streamChannelName } from "../api";
import { subscribeSessionSnapshot } from "../api/rest";
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

        // ── Dispatch now that the listener is live ───────────────
        // This is the stdio-era replacement for "wait for the socket
        // to open, then send". The bridge creates ONE channel per
        // session and keeps it alive across sends, so the server only
        // emits `connected` for the first send on a session. Waiting
        // for `connected` on every subsequent send deadlocked the
        // UI: nothing was ever dispatched upstream, no event ever
        // came back, and the bubble sat on "正在思考…" forever.
        //
        // Ordering matters — the listener must be registered BEFORE
        // the frame goes out, otherwise a fast server reply could
        // race ahead of the subscription and be dropped by the
        // channel filter in `listenStreamChannel`. `dispatchUserMessage`
        // is idempotent, so when the server DOES still send
        // `connected` (first send on a session), that path finds the
        // flag already set and does nothing.
        ctx.dispatchUserMessage();
      } catch (e) {
        // Listener registration failed (bridge not initialised,
        // window torn down, etc.). Surface the failure to the
        // caller so the UI can show "网络中断" NOW instead of
        // waiting on the server-side 180s hard timeout.
        if (ctx.activeSendIdRef.current !== ctx.mySendId) return;

        // P5 L3 seq 对账（设计 §3.5 / §6.3 P7）：连接丢失后拉一次
        // 订阅快照对账——携本 attachment 的本地 seq 水位
        // (`ctx.lastLocalSeq`)作 since_seq。服务端按 §3.1 双分支
        // 回：已追上（空增量 + fullSnapshot=false）客户端只刷新水位；
        // 落后（全量快照 + fullSnapshot=true）客户端整体替换本地
        // 会话状态。best-effort——快照端点本身走 stdio bridge 的
        // http 通道，若 bridge 已死此调用会 reject，吞掉异常不
        // 影响 settle（对账是"锦上添花"，settle 才是"必须有"）。
        try {
          const snap = await subscribeSessionSnapshot(
            ctx.sessionId,
            ctx.lastLocalSeq,
          );
          // 刷新本地水位到服务端持久 seq（跨重启有效）——下次
          // 重连的 since_seq 锚。max 语义：不降。
          if (snap.lastSeq > ctx.lastLocalSeq) ctx.lastLocalSeq = snap.lastSeq;
          ctx.callbacks.onReconciled?.({
            sessionId: snap.sessionId,
            messages: snap.messages,
            lastSeq: snap.lastSeq,
            fullSnapshot: snap.fullSnapshot,
          });
        } catch {
          // bridge 死 / 快照端点 4xx —— 对账不可得，settle 仍走
          // 原错误路径（下方）。不影响主流程。
        }

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
