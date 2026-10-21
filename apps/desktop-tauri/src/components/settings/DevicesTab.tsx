import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { AlertTriangle, Building2, MessageCircle, RefreshCw, Smartphone, Trash2 } from "lucide-react";
import { fetchJSON } from "../../api/rest";
import { toastError, toastSuccess } from "../../hooks/useToast";
import { ImChannelsCard } from "./ImChannelsCard";
import { channelOf, type ChannelId, type PairedDevice, type RevokeOutcome } from "./channelOf";
import { ConfirmDialog } from "../ui";
import { LOADING, failure } from "../strings";

/** 列表标题跟着选中渠道走——固定写「已配对设备」会让人以为看到的是全局表。 */
const CHANNEL_TITLES: Record<ChannelId, string> = {
  qq: "QQ 已连接的账号",
  wecom: "企业微信已连接的账号",
  mobile: "手机端已连接的设备",
};

/**
 * 行首图标跟渠道走。此前一律用手机图标，于是 QQ 账号前面摆着一部手机——
 * 列表已经按渠道切分之后，这个图标不再需要「说明这是不是 IM」，只需要对得上
 * 上面左栏选中的那个渠道。
 */
const CHANNEL_ICONS: Record<ChannelId, typeof Smartphone> = {
  qq: MessageCircle,
  wecom: Building2,
  mobile: Smartphone,
};

const CHANNEL_EMPTY: Record<ChannelId, string> = {
  qq: "还没有 QQ 账号连进来。在上面完成绑定即可。",
  wecom: "还没有企业微信账号连进来。在上面完成绑定即可。",
  mobile: "还没有手机连进来。在上面生成配对码，用手机扫一次即可。",
};

/** `RevokeOutcome` 住在 ./channelOf —— ImChannelsCard 撤销手机时也要读它。 */

function formatUnixSec(sec: number | null): string {
  if (!sec) return "—";
  return new Date(sec * 1000).toLocaleString();
}

/**
 * 设备管理 tab。
 *
 * 分两块，职责不重叠：
 * - 上：**连接渠道**（ImChannelsCard）—— 三个渠道怎么接进来，左栏选、右栏配。
 * - 下：**该渠道已连接的设备**—— 已经接过来了的是谁。
 *
 * 「已连接」列表**跟着左栏选中的渠道走**（2026-10-10 改）。此前它是一张跨渠道的
 * 全局表：选中企业微信时，底下照样列着 QQ 的设备和撤销按钮——既串台，撤销也容易
 * 点错人。设备表本身仍是一张跨渠道的表（IM 账号与配对手机共用 `paired_devices`），
 * 变的只是**呈现**：按 `channelOf()` 切分。
 */
export function DevicesTab() {
  const [active, setActive] = useState<ChannelId>("qq");
  const [devices, setDevices] = useState<PairedDevice[] | null>(null);
  const [listError, setListError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [revoking, setRevoking] = useState<string | null>(null);
  const [confirmRevoke, setConfirmRevoke] = useState<PairedDevice | null>(null);
  const abortRef = useRef<AbortController | null>(null);

  const refreshDevices = useCallback(async () => {
    abortRef.current?.abort();
    const ac = new AbortController();
    abortRef.current = ac;
    setBusy(true);
    setListError(null);
    try {
      const list = await fetchJSON<PairedDevice[]>("/api/pairing/devices", { signal: ac.signal });
      if (!ac.signal.aborted) setDevices(list);
    } catch (e) {
      if (!ac.signal.aborted) {
        setListError(e instanceof Error ? e.message : String(e));
        setDevices(null);
      }
    } finally {
      if (!ac.signal.aborted) setBusy(false);
    }
  }, []);

  useEffect(() => {
    void refreshDevices();
    return () => abortRef.current?.abort();
  }, [refreshDevices]);

  const onRevoke = useCallback(
    async (deviceId: string, deviceName: string) => {
      setRevoking(deviceId);
      try {
        const r = await fetchJSON<RevokeOutcome>(
          `/api/pairing/devices/${encodeURIComponent(deviceId)}`,
          { method: "DELETE" },
        );
        // 三种结果分别说清楚，不能一律「已撤销」：
        //  - 两边都断：真的撤销了
        //  - 只断了本地：这台手机发不出帧，但仍可能在收中转的广播帧
        //  - 没绑过中转：它只做过局域网直连，没有 relay 侧可撤销的东西
        if (r.relay_error) {
          toastError(
            `已撤销本地访问，但没能断开中转连接：${r.relay_error}。这台设备可能仍在接收消息。`,
          );
        } else if (r.relay_bound && !r.relay_revoked) {
          toastError("已撤销本地访问，但中转侧没有这条连接的记录。");
        } else if (r.relay_bound) {
          toastSuccess(`已撤销：${deviceName}（含中转连接）`);
        } else {
          toastSuccess(`已撤销：${deviceName}（该设备未连接中转）`);
        }
        await refreshDevices();
      } catch (e) {
        const msg = e instanceof Error ? e.message : String(e);
        toastError(failure("撤销失败", "请重试。", msg));
      } finally {
        setRevoking(null);
      }
    },
    [refreshDevices],
  );

  // 只呈现当前选中渠道的设备。见 channelOf() 的注释。
  const scoped = useMemo(
    () => (devices ?? []).filter((d) => channelOf(d.device_name) === active),
    [devices, active],
  );
  const activeDevices = scoped.filter((d) => !d.revoked_at);
  const revokedDevices = scoped.filter((d) => d.revoked_at);

  const ChannelIcon = CHANNEL_ICONS[active];

  return (
    <div className="space-y-6">
      <ImChannelsCard active={active} onActiveChange={setActive} />

      {/* 当前渠道已连接的设备 */}
      <div className="overflow-hidden rounded-[var(--radius-lg)] bg-bg-card shadow-[var(--shadow-paper)]">
        <div className="flex items-center justify-between px-4 py-3 bg-bg-secondary">
          <span className="text-[length:var(--text-ui)] font-medium text-text-primary">
            {CHANNEL_TITLES[active]}
          </span>
          <button
            onClick={() => void refreshDevices()}
            className="h-7 px-2.5 rounded-md flex items-center gap-1 text-[length:var(--text-meta)] text-text-secondary hover:text-text-primary hover:bg-bg-card-hover transition-colors"
            title="刷新设备列表"
          >
            <RefreshCw className={`icon-sm ${busy ? "animate-spin" : ""}`} />
            刷新
          </button>
        </div>

        {listError ? (
          <div className="px-4 py-3 flex items-start gap-2 text-[length:var(--text-meta)] text-text-secondary">
            <AlertTriangle className="icon-md mt-0.5 shrink-0" />
            读取设备列表失败：<span className="font-mono">{listError}</span>，请点「刷新」重试。
          </div>
        ) : devices === null ? (
          <div className="px-4 py-3 text-[length:var(--text-meta)] text-text-muted" aria-busy="true">{LOADING}</div>
        ) : activeDevices.length === 0 ? (
          <div className="px-4 py-3 text-[length:var(--text-meta)] text-text-muted">
            {CHANNEL_EMPTY[active]}
          </div>
        ) : (
          <ul className="p-2 space-y-1">
            {activeDevices.map((d) => (
              <li
                key={d.device_id}
                className="px-3 py-3 flex items-center gap-3 flex-wrap rounded-lg bg-bg-secondary hover:bg-bg-card-hover transition-colors"
              >
                <ChannelIcon className="icon-md text-text-muted shrink-0" />
                <div className="min-w-0 flex-1">
                  <div className="text-[length:var(--text-ui)] text-text-primary font-medium truncate">
                    {d.device_name}
                  </div>
                  <div className="text-[length:var(--text-meta)] text-text-muted">
                    <span className="font-mono">{d.device_id}</span>
                    {" · "}配对于 {formatUnixSec(d.issued_at)}
                    {" · "}最近活跃 {formatUnixSec(d.last_seen_at)}
                  </div>
                </div>
                <button
                  onClick={() => setConfirmRevoke(d)}
                  disabled={revoking === d.device_id}
                  className="h-7 px-2.5 rounded-md flex items-center gap-1 text-[length:var(--text-meta)] text-text-secondary hover:text-text-error hover:bg-error-soft transition-colors disabled:opacity-50 shrink-0"
                  title="撤销此设备的访问权限"
                >
                  <Trash2 className="icon-sm" />
                  {revoking === d.device_id ? "撤销中…" : "撤销"}
                </button>
              </li>
            ))}
          </ul>
        )}

        {revokedDevices.length > 0 && (
          <div className="px-4 py-3 bg-bg-secondary">
            <div className="text-[length:var(--text-meta)] text-text-muted mb-1">已撤销（{revokedDevices.length}）</div>
            <div className="text-[length:var(--text-meta)] text-text-muted space-y-0.5">
              {revokedDevices.map((d) => (
                <div key={d.device_id} className="truncate">
                  <span className="font-mono">{d.device_name}</span>
                  {" · "}撤销于 {formatUnixSec(d.revoked_at)}
                </div>
              ))}
            </div>
          </div>
        )}
      </div>

      <ConfirmDialog
        open={confirmRevoke !== null}
        title="撤销设备"
        body={`撤销设备「${confirmRevoke?.device_name ?? ""}」？撤销后其 token 立即失效，需要重新配对才能接入。`}
        confirmLabel="撤销"
        onCancel={() => setConfirmRevoke(null)}
        onConfirm={() => {
          const d = confirmRevoke;
          setConfirmRevoke(null);
          if (d) void onRevoke(d.device_id, d.device_name);
        }}
      />
    </div>
  );
}
