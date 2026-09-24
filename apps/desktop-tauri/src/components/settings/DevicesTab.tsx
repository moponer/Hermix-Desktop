import { useCallback, useEffect, useRef, useState } from "react";
import { RefreshCw, Smartphone, QrCode, Trash2, AlertTriangle } from "lucide-react";
import QRCode from "qrcode";
import { fetchJSON } from "../../api/rest";
import { toastError, toastSuccess } from "../../hooks/useToast";

/** `/api/pairing/sessions` 返回的一次性配对负载（pairing.rs::QrPayload）。 */
interface QrPayload {
  server_id: string;
  host: string;
  port: number;
  cert_fingerprint: string;
  pairing_token: string;
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
      // 二维码内容 = QR 负载原样 JSON，手机端按 proto_version 解析。
      const svg = await QRCode.toString(JSON.stringify(payload), {
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

  return (
    <div className="space-y-6">
      <div>
        <h2 className="text-base font-medium text-text-primary">设备管理</h2>
        <p className="text-sm text-text-muted mt-1">
          手机等远程设备通过扫码配对后可访问本机 Hermix。连接走 TLS 1.3，
          证书指纹固定（TOFU）；高危工具在桌面端强制确认。
        </p>
      </div>

      {/* 配对二维码 */}
      <div className="rounded-lg border border-border-primary bg-bg-card p-4">
        <div className="flex items-center gap-2 mb-3">
          <QrCode className="icon-md text-text-muted" />
          <span className="text-[13px] font-medium text-text-primary">发起配对</span>
          <button
            onClick={() => void generateQr()}
            className="ml-auto h-7 px-2.5 rounded-md flex items-center gap-1 text-[12px] text-text-secondary hover:text-text-primary hover:bg-bg-hover border border-border-secondary"
            title="重新生成配对码（旧码立即作废）"
          >
            <RefreshCw className="icon-sm" />
            重新生成
          </button>
        </div>

        {qrError ? (
          <div className="flex items-start gap-2 text-[12px] text-text-secondary">
            <AlertTriangle className="icon-md mt-0.5 shrink-0" />
            <div>
              生成配对码失败：<span className="font-mono">{qrError}</span>
              <br />
              <span className="text-text-muted">
                配对依赖 TLS 监听（环境变量 <code className="font-mono">HERMIX_TLS_ADDR</code>），
                未开启时无法发起。
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
            <div className="text-[12px] text-text-muted space-y-1 min-w-0">
              <div>
                服务器：<span className="font-mono text-text-secondary">{qr.payload.server_id}</span>
              </div>
              <div>
                地址：<span className="font-mono text-text-secondary">{qr.payload.host}:{qr.payload.port}</span>
              </div>
              <div>
                指纹（前 16 位）：
                <span className="font-mono text-text-secondary">{qr.payload.cert_fingerprint.slice(0, 16)}…</span>
              </div>
              <div>配对码一次性有效，2 分钟内未扫码即作废。</div>
            </div>
          </div>
        ) : (
          <div className="text-[12px] text-text-muted">正在生成配对码…</div>
        )}
      </div>

      {/* 已配对设备 */}
      <div className="rounded-lg border border-border-primary bg-bg-card">
        <div className="flex items-center justify-between px-4 py-3 border-b border-border-primary">
          <span className="text-[13px] font-medium text-text-primary">已配对设备</span>
          <button
            onClick={() => void refreshDevices()}
            className="h-7 px-2.5 rounded-md flex items-center gap-1 text-[12px] text-text-secondary hover:text-text-primary hover:bg-bg-hover border border-border-secondary"
            title="刷新设备列表"
          >
            <RefreshCw className={`icon-sm ${busy ? "animate-spin" : ""}`} />
            刷新
          </button>
        </div>

        {listError ? (
          <div className="px-4 py-3 flex items-start gap-2 text-[12px] text-text-secondary">
            <AlertTriangle className="icon-md mt-0.5 shrink-0" />
            读取设备列表失败：<span className="font-mono">{listError}</span>
          </div>
        ) : devices === null ? (
          <div className="px-4 py-3 text-[12px] text-text-muted">正在读取…</div>
        ) : activeDevices.length === 0 ? (
          <div className="px-4 py-3 text-[12px] text-text-muted">暂无已配对设备。</div>
        ) : (
          <ul className="divide-y divide-border-primary">
            {activeDevices.map((d) => (
              <li key={d.device_id} className="px-4 py-3 flex items-center gap-3 flex-wrap">
                <Smartphone className="icon-md text-text-muted shrink-0" />
                <div className="min-w-0 flex-1">
                  <div className="text-[13px] text-text-primary font-medium truncate">{d.device_name}</div>
                  <div className="text-[11px] text-text-muted">
                    <span className="font-mono">{d.device_id}</span>
                    {" · "}配对于 {formatUnixSec(d.issued_at)}
                    {" · "}最近活跃 {formatUnixSec(d.last_seen_at)}
                  </div>
                </div>
                <button
                  onClick={() => void onRevoke(d.device_id, d.device_name)}
                  disabled={revoking === d.device_id}
                  className="h-7 px-2.5 rounded-md flex items-center gap-1 text-[12px] text-text-secondary hover:text-text-primary hover:bg-bg-hover border border-border-secondary disabled:opacity-50"
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
            <div className="text-[11px] text-text-muted mb-1">已撤销（{revokedDevices.length}）</div>
            <div className="text-[11px] text-text-muted space-y-0.5">
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
