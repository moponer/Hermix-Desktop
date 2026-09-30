import { useCallback, useEffect, useRef, useState } from "react";
import { RefreshCw, Smartphone, QrCode, Trash2, AlertTriangle } from "lucide-react";
import QRCode from "qrcode";
import { fetchJSON } from "../../api/rest";
import { toastError, toastSuccess } from "../../hooks/useToast";
import { ImChannelsCard } from "./ImChannelsCard";

/** `/api/pairing/sessions` 返回的一次性配对负载（pairing.rs::QrPayload）。 */
interface QrPayload {
  server_id: string;
  host: string;
  port: number;
  cert_fingerprint: string;
  pairing_code: string;
  pairing_url: string;
  proto_version: number;
}

/** `GET /api/pairing/devices` 的行（pairing.rs::PairedDevice）。 */
interface PairedDevice {
  device_id: string;
  device_name: string;
  scopes: string;
  issued_at: number;
  last_seen_at: number | null;
  revoked_at: number | null;
}

function formatUnixSec(sec: number | null): string {
  if (!sec) return "—";
  return new Date(sec * 1000).toLocaleString();
}

/**
 * 设备管理 tab — 桌面端配对入口。生成一次性配对二维码（TLS 指纹 TOFU），
 * 展示已配对设备并支持撤销。走 stdio bridge 调 pairing.rs 的 4 个端点。
 */
export function DevicesTab() {
  const [qr, setQr] = useState<{ payload: QrPayload; svg: string } | null>(null);
  const [qrError, setQrError] = useState<string | null>(null);
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

  const generateQr = useCallback(async () => {
    setQrError(null);
    try {
      const payload = await fetchJSON<QrPayload>("/api/pairing/sessions", { method: "POST" });
      // 二维码内容 = 配对 URL：手机相机扫码 → 浏览器打开 /pair 落地页提交配对
      //（抄 cc-haha；6 位码同时展示在右侧，支持像 QQ 那样手动输入）。
      const svg = await QRCode.toString(payload.pairing_url, {
        type: "svg",
        margin: 1,
        errorCorrectionLevel: "M",
      });
      setQr({ payload, svg });
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      setQrError(msg);
      setQr(null);
    }
  }, []);

  useEffect(() => {
    void refreshDevices();
    void generateQr();
    return () => abortRef.current?.abort();
  }, [refreshDevices, generateQr]);

  const onRevoke = useCallback(
    async (deviceId: string, deviceName: string) => {
      if (!window.confirm(`撤销设备「${deviceName}」？撤销后其 token 立即失效。`)) return;
      setRevoking(deviceId);
      try {
        await fetchJSON(`/api/pairing/devices/${encodeURIComponent(deviceId)}`, { method: "DELETE" });
        toastSuccess(`已撤销：${deviceName}`);
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
      <div>
        <h2 className="text-base font-medium text-text-primary">设备与远程接入</h2>
        <p className="text-sm text-text-muted mt-1">
          两条互相独立的通道，按需选择：高危工具一律在桌面端强制确认。
        </p>
        <div className="mt-3 grid grid-cols-1 md:grid-cols-2 gap-3">
          <div className="rounded-lg border border-border-primary bg-bg-card p-3">
            <div className="flex items-center gap-2">
              <span className="inline-grid place-items-center h-5 w-5 rounded-full bg-accent-soft text-accent text-[length:var(--text-tiny)] font-semibold">1</span>
              <span className="text-[length:var(--text-ui)] font-medium text-text-primary">IM 机器人</span>
              <span className="text-[length:var(--text-meta)] text-text-muted">推荐</span>
            </div>
            <p className="mt-1 text-[length:var(--text-meta)] text-text-muted leading-relaxed">
              用 QQ 或 企业微信私聊机器人，在手机上继续本机会话、批权限。绑定机器人 → 发配对码 → 开聊。
            </p>
          </div>
          <div className="rounded-lg border border-border-primary bg-bg-card p-3">
            <div className="flex items-center gap-2">
              <span className="inline-grid place-items-center h-5 w-5 rounded-full bg-bg-tertiary text-text-secondary text-[length:var(--text-tiny)] font-semibold">2</span>
              <span className="text-[length:var(--text-ui)] font-medium text-text-primary">手机浏览器（H5）</span>
            </div>
            <p className="mt-1 text-[length:var(--text-meta)] text-text-muted leading-relaxed">
              手机扫码后用浏览器获得完整对话界面。同一局域网内可用，走 TLS 加密。
            </p>
          </div>
        </div>
      </div>

      {/* 通道一：IM 机器人 */}
      <div>
        <div className="flex items-center gap-2 mb-3">
          <span className="inline-grid place-items-center h-5 w-5 rounded-full bg-accent-soft text-accent text-[length:var(--text-tiny)] font-semibold">1</span>
          <span className="text-[length:var(--text-ui)] font-semibold text-text-primary">IM 机器人（QQ / 企业微信）</span>
        </div>
        <ImChannelsCard />
      </div>

      {/* 通道二：手机浏览器 H5 —— 与 IM 配对码无关，别混 */}
      <div className="rounded-lg border border-border-primary bg-bg-card p-4">
        <div className="flex items-center gap-2 mb-1">
          <span className="inline-grid place-items-center h-5 w-5 rounded-full bg-bg-tertiary text-text-secondary text-[length:var(--text-tiny)] font-semibold">2</span>
          <span className="text-[length:var(--text-ui)] font-semibold text-text-primary">手机浏览器（H5）</span>
        </div>
        <p className="text-[length:var(--text-meta)] text-text-muted mb-3">
          和上面的 IM 配对码不是一回事：这个二维码是给手机浏览器用的，扫开后在浏览器里获得完整对话界面。
        </p>
        <div className="flex items-center gap-2 mb-3">
          <QrCode className="icon-md text-text-muted" />
          <span className="text-[length:var(--text-ui)] font-medium text-text-primary">配对二维码</span>
          <button
            onClick={() => void generateQr()}
            className="ml-auto h-7 px-2.5 rounded-md flex items-center gap-1 text-[length:var(--text-meta)] text-text-secondary hover:text-text-primary hover:bg-bg-hover border border-border-secondary"
            title="重新生成配对码（旧码立即作废）"
          >
            <RefreshCw className="icon-sm" />
            重新生成
          </button>
        </div>

        {qrError ? (
          <div className="flex items-start gap-2 text-[length:var(--text-meta)] text-text-secondary">
            <AlertTriangle className="icon-md mt-0.5 shrink-0" />
            <div>
              生成配对码失败：<span className="font-mono">{qrError}</span>
              <br />
              <span className="text-text-muted">
                TLS 监听默认开启（端口 8443）；若设置了 <code className="font-mono">HERMIX_TLS_DISABLE=1</code>
                {" "}则无法配对，删除该变量并重启即可。
              </span>
            </div>
          </div>
        ) : qr ? (
          <div className="flex items-start gap-4 flex-wrap">
            <div
              className="shrink-0 rounded-lg border border-border-secondary bg-white p-2 [&>svg]:block"
              style={{ width: 168, height: 168 }}
              dangerouslySetInnerHTML={{ __html: qr.svg }}
            />
            <div className="text-[length:var(--text-meta)] text-text-muted space-y-1 min-w-0">
              <div>
                配对码（手机上输入或扫码）：
                <span
                  className="font-mono text-text-primary font-semibold tracking-[0.3em] text-[length:var(--text-title)] ml-1 select-all"
                >
                  {qr.payload.pairing_code}
                </span>
              </div>
              <div>
                扫码地址：<span className="font-mono text-text-secondary break-all">{qr.payload.pairing_url}</span>
              </div>
              <div>
                服务器：<span className="font-mono text-text-secondary">{qr.payload.server_id}</span>
                {" · "}指纹（前 16 位）：
                <span className="font-mono text-text-secondary">{qr.payload.cert_fingerprint.slice(0, 16)}…</span>
              </div>
              <div>配对码 60 分钟内有效、只能用一次；重新生成后旧码立即作废。连续输错 5 次将锁定，需重新生成。</div>
            </div>
          </div>
        ) : (
          <div className="text-[length:var(--text-meta)] text-text-muted">正在生成配对码…</div>
        )}
      </div>

      {/* 已配对设备 */}
      <div className="rounded-lg border border-border-primary bg-bg-card">
        <div className="flex items-center justify-between px-4 py-3 border-b border-border-primary">
          <span className="text-[length:var(--text-ui)] font-medium text-text-primary">已配对设备</span>
          <button
            onClick={() => void refreshDevices()}
            className="h-7 px-2.5 rounded-md flex items-center gap-1 text-[length:var(--text-meta)] text-text-secondary hover:text-text-primary hover:bg-bg-hover border border-border-secondary"
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
          <div className="px-4 py-3 text-[length:var(--text-meta)] text-text-muted">暂无已配对设备。</div>
        ) : (
          <ul className="divide-y divide-border-primary">
            {activeDevices.map((d) => (
              <li key={d.device_id} className="px-4 py-3 flex items-center gap-3 flex-wrap">
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
                  className="h-7 px-2.5 rounded-md flex items-center gap-1 text-[length:var(--text-meta)] text-text-secondary hover:text-text-primary hover:bg-bg-hover border border-border-secondary disabled:opacity-50"
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
          <div className="px-4 py-3 border-t border-border-primary">
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
