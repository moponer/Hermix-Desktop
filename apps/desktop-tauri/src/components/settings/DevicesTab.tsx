import { useCallback, useEffect, useRef, useState } from "react";
import { RefreshCw, Smartphone, Trash2, AlertTriangle } from "lucide-react";
import { fetchJSON } from "../../api/rest";
import { toastError, toastSuccess } from "../../hooks/useToast";
import { ImChannelsCard } from "./ImChannelsCard";

/** `GET /api/pairing/devices` 的行（pairing.rs::PairedDevice）。 */
interface PairedDevice {
  device_id: string;
  device_name: string;
  scopes: string;
  issued_at: number;
  last_seen_at: number | null;
  revoked_at: number | null;
}

/**
 * `DELETE /api/pairing/devices/{id}` 的返回。
 *
 * **撤销是两件事**，必须分开报：本地设备令牌失效（= 发不出帧）永远成功；
 * relay 侧断开连接则可能失败（relay 不可达、没配地址、绑都没绑过）。
 * 把两者合成一句「已撤销」就是在骗用户——那台手机可能还在收广播帧。
 */
interface RevokeOutcome {
  device_id: string;
  local_revoked: boolean;
  /** 这台设备配对时是否报过 relay 凭据哈希。没报过就没有 relay 侧可撤。 */
  relay_bound: boolean;
  relay_revoked: boolean;
  relay_error: string | null;
}

function formatUnixSec(sec: number | null): string {
  if (!sec) return "—";
  return new Date(sec * 1000).toLocaleString();
}

/**
 * 设备管理 tab。
 *
 * 分两块，职责不重叠：
 * - 上：**连接渠道**（ImChannelsCard）—— 三个渠道怎么接进来，左栏选、右栏配。
 * - 下：**已配对设备**—— 已经接过来了的是谁。这份列表是**跨渠道**的
 *   （IM 账号形如 `qq:<openid>` 也在这张表里），所以它是全局表而不是某个
 *   渠道的附属，放在主从布局之外。
 *
 * 配对二维码原先在本文件里，和「手机端」这个渠道割裂着放；2026-10-09 随
 * 渠道改主从布局一起挪进了 ImChannelsCard —— 选到「手机端」才生成，不选
 * 不做这份无用功。
 */
export function DevicesTab() {
  const [devices, setDevices] = useState<PairedDevice[] | null>(null);
  const [listError, setListError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [revoking, setRevoking] = useState<string | null>(null);
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
      if (!window.confirm(`撤销设备「${deviceName}」？撤销后其 token 立即失效。`)) return;
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
        toastError(`撤销失败：${e instanceof Error ? e.message : String(e)}`);
      } finally {
        setRevoking(null);
      }
    },
    [refreshDevices],
  );

  const activeDevices = devices?.filter((d) => !d.revoked_at) ?? [];
  const revokedDevices = devices?.filter((d) => d.revoked_at) ?? [];

  /** IM 账号在设备表里的名字形如 `qq:<openid>` / `wecom:<userid>`（见 channels/session.rs）。 */
  const imLabel = (name: string): string | null => {
    const m = /^(qq|wecom):(.+)$/.exec(name);
    if (!m) return null;
    return m[1] === "qq" ? "QQ" : "企业微信";
  };

  return (
    <div className="space-y-6">
      <ImChannelsCard />

      {/* 已配对设备 —— 跨渠道的全局表 */}
      <div className="rounded-xl bg-bg-card overflow-hidden">
        <div className="flex items-center justify-between px-4 py-3 bg-bg-secondary">
          <span className="text-[length:var(--text-ui)] font-medium text-text-primary">已配对设备</span>
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
            读取设备列表失败：<span className="font-mono">{listError}</span>
          </div>
        ) : devices === null ? (
          <div className="px-4 py-3 text-[length:var(--text-meta)] text-text-muted">正在读取…</div>
        ) : activeDevices.length === 0 ? (
          <div className="px-4 py-3 text-[length:var(--text-meta)] text-text-muted">
            暂无已配对设备。在上面选一个渠道完成绑定或扫码即可。
          </div>
        ) : (
          <ul className="p-2 space-y-1">
            {activeDevices.map((d) => (
              <li
                key={d.device_id}
                className="px-3 py-3 flex items-center gap-3 flex-wrap rounded-lg bg-bg-secondary hover:bg-bg-card-hover transition-colors"
              >
                <Smartphone className="icon-md text-text-muted shrink-0" />
                <div className="min-w-0 flex-1">
                  <div className="text-[length:var(--text-ui)] text-text-primary font-medium truncate flex items-center gap-2">
                    {d.device_name}
                    {imLabel(d.device_name) && (
                      <span className="text-[length:var(--text-meta)] font-normal text-text-muted shrink-0">
                        IM · {imLabel(d.device_name)}
                      </span>
                    )}
                  </div>
                  <div className="text-[length:var(--text-meta)] text-text-muted">
                    <span className="font-mono">{d.device_id}</span>
                    {" · "}配对于 {formatUnixSec(d.issued_at)}
                    {" · "}最近活跃 {formatUnixSec(d.last_seen_at)}
                  </div>
                </div>
                <button
                  onClick={() => void onRevoke(d.device_id, d.device_name)}
                  disabled={revoking === d.device_id}
                  className="h-7 px-2.5 rounded-md flex items-center gap-1 text-[length:var(--text-meta)] text-text-secondary hover:text-error hover:bg-error-soft transition-colors disabled:opacity-50 shrink-0"
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
    </div>
  );
}
