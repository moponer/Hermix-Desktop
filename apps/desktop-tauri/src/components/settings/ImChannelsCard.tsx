import { useCallback, useEffect, useRef, useState } from "react";
import {
  MessageSquare,
  QrCode,
  RefreshCw,
  Link as LinkIcon,
  Unlink,
  AlertTriangle,
  Check,
  Copy,
} from "lucide-react";
import QRCode from "qrcode";
import { fetchJSON } from "../../api/rest";
import { toastError, toastSuccess } from "../../hooks/useToast";

/** 后端 `channels::provision::BindStatusItem`。 */
interface BindStatusItem {
  platform: string;
  bound: boolean;
  id_preview: string;
  source: "env" | "file" | "";
}

/** `GET /api/channels/status` 的行：凭据 + 网关连接状态。 */
interface ChannelStatusItem {
  platform: string;
  bound: boolean;
  connected: boolean;
}

interface BindStartResult {
  session_key: string;
  verification_url: string;
  poll_interval_secs: number;
}

/** 后端 `channels::provision::BindPoll`，用 `status` 字段做 tag 分发。 */
type BindPoll =
  | { status: "waiting"; message: string }
  | { status: "connected" }
  | { status: "expired"; message: string }
  | { status: "failed"; message: string }
  | { status: "not_started"; message: string };

interface PairingCodeResult {
  platform: string;
  pairing_code: string;
  ttl_secs: number;
  single_use: boolean;
}

const PLATFORMS: {
  id: string;
  label: string;
  /** 圆形字母徽标：贴近各品牌主色，一眼认出渠道，不画来路不明的 logo。 */
  mark: string;
  markBg: string;
  markFg: string;
  hint: string;
  bindHint: string;
  sandboxNote: string;
}[] = [
  {
    id: "qq",
    label: "QQ",
    mark: "Q",
    markBg: "var(--color-brand-qq)",
    markFg: "var(--color-on-accent)",
    hint: "QQ 开放平台机器人 · 只处理私聊 · 流式消息",
    bindHint: "用手机 QQ 扫码，创建并授权机器人，AppID 与 AppSecret 自动写入。",
    /* 新建机器人默认在【沙箱环境】：只有被加进白名单的 QQ 号能给它发消息，
       其他人私聊它收不到 —— 表现为「绑定了但机器人不回消息」的头号原因。 */
    sandboxNote:
      "刚建好的机器人默认处于沙箱环境：只有你在 QQ 开放平台把它加进「沙箱白名单」的那个 QQ 号才能私聊它并收到回复。要给别人用需发布到正式环境。",
  },
  {
    id: "wecom",
    label: "企业微信",
    mark: "微",
    markBg: "var(--color-brand-wecom)",
    markFg: "var(--color-on-accent)",
    hint: "企业微信智能机器人 · 只处理单聊 · 气泡原地刷新",
    bindHint: "用企业微信扫码，创建并授权智能机器人，botid 与 secret 自动写入。",
    sandboxNote: "",
  },
];

/** 卡片外框。抄 cc-haha 的 `rounded-[var(--radius-xl)] border … overflow-hidden` 卡片壳。 */
function Card({ title, icon, children, action }: { title: string; icon: React.ReactNode; children: React.ReactNode; action?: React.ReactNode }) {
  return (
    <div className="rounded-xl border border-border-primary bg-bg-card overflow-hidden">
      <div className="flex items-center gap-2 px-4 py-3 bg-bg-hover border-b border-border-primary">
        {icon}
        <span className="text-[length:var(--text-ui)] font-semibold text-text-primary">{title}</span>
        {action ? <div className="ml-auto flex items-center gap-2">{action}</div> : null}
      </div>
      <div className="p-4 space-y-4">{children}</div>
    </div>
  );
}

/**
 * IM 接入卡片。
 *
 * 布局抄 cc-haha `AdapterSettings.tsx`：
 * **配对管理置顶**（在平台列表之上），因为它是"进来之后干什么"；平台绑定
 * 是"怎么进来"，属于准备步骤。每个平台一行，标题+说明+已绑定 id 在左，
 * 按钮在右，二维码展开在下一行——就是它 `QrBindPanel` 的结构。
 *
 * 绑定走厂商自己的接口（后端 `channels/provision.rs`）：这里只负责把授权
 * URL 渲染成二维码并轮询结果。凭据由后端落盘，前端永远拿不到 secret。
 */
export function ImChannelsCard() {
  const [status, setStatus] = useState<BindStatusItem[] | null>(null);
  const [statusError, setStatusError] = useState<string | null>(null);
  /** 网关是否真连上了。凭据在 ≠ 通道活着 —— 「机器人不回消息」多半卡在这里。 */
  const [live, setLive] = useState<Record<string, boolean>>({});
  /** 正在扫码的平台 → 二维码。同一时刻只有一个平台在绑定。 */
  const [binding, setBinding] = useState<{ platform: string; url: string; svg: string } | null>(null);
  const [bindingNote, setBindingNote] = useState<string | null>(null);
  const [pairCode, setPairCode] = useState<PairingCodeResult | null>(null);
  const [busy, setBusy] = useState(false);
  const pollTimer = useRef<number | null>(null);
  const sessionKey = useRef<string | null>(null);

  const refreshStatus = useCallback(async () => {
    try {
      const res = await fetchJSON<{ platforms: BindStatusItem[] }>("/api/channels/bind/status");
      setStatus(res.platforms);
      setStatusError(null);
      fetchJSON<{ platforms: ChannelStatusItem[] }>("/api/channels/status")
        .then((st) =>
          setLive(
            Object.fromEntries(st.platforms.map((p) => [p.platform, p.connected])),
          ),
        )
        .catch(() => setLive({}));
    } catch (e) {
      setStatusError(e instanceof Error ? e.message : String(e));
      setStatus(null);
    }
  }, []);

  useEffect(() => {
    void refreshStatus();
    return () => {
      if (pollTimer.current !== null) window.clearTimeout(pollTimer.current);
    };
  }, [refreshStatus]);

  const stopPolling = useCallback(() => {
    if (pollTimer.current !== null) {
      window.clearTimeout(pollTimer.current);
      pollTimer.current = null;
    }
    sessionKey.current = null;
  }, []);

  const pollOnce = useCallback(
    async (platform: string, key: string) => {
      try {
        const res = await fetchJSON<BindPoll>(
          `/api/channels/bind/poll?session_key=${encodeURIComponent(key)}`,
        );
        switch (res.status) {
          case "waiting":
            setBindingNote(res.message);
            pollTimer.current = window.setTimeout(() => void pollOnce(platform, key), 3000);
            return;
          case "connected":
            stopPolling();
            setBinding(null);
            setBindingNote(null);
            toastSuccess(`${platform === "qq" ? "QQ" : "企业微信"} 绑定成功，通道已自动启动`);
            await refreshStatus();
            return;
          default:
            stopPolling();
            setBinding(null);
            setBindingNote(res.message);
            return;
        }
      } catch (e) {
        stopPolling();
        setBinding(null);
        setBindingNote(`查询绑定状态失败：${e instanceof Error ? e.message : String(e)}`);
      }
    },
    [refreshStatus, stopPolling],
  );

  const startBind = useCallback(
    async (platform: string) => {
      setBusy(true);
      setBindingNote(null);
      try {
        const res = await fetchJSON<BindStartResult>("/api/channels/bind/start", {
          method: "POST",
          body: JSON.stringify({ platform }),
        });
        const svg = await QRCode.toString(res.verification_url, {
          type: "svg",
          margin: 1,
          errorCorrectionLevel: "M",
        });
        setBinding({ platform, url: res.verification_url, svg });
        setBindingNote("请用手机扫码授权");
        sessionKey.current = res.session_key;
        pollTimer.current = window.setTimeout(
          () => void pollOnce(platform, res.session_key),
          res.poll_interval_secs * 1000,
        );
      } catch (e) {
        setBindingNote(`发起绑定失败：${e instanceof Error ? e.message : String(e)}`);
      } finally {
        setBusy(false);
      }
    },
    [pollOnce],
  );

  const cancelBind = useCallback(async () => {
    const key = sessionKey.current;
    stopPolling();
    setBinding(null);
    setBindingNote(null);
    if (key) {
      await fetchJSON("/api/channels/bind/cancel", {
        method: "POST",
        body: JSON.stringify({ session_key: key }),
      }).catch(() => undefined);
    }
  }, [stopPolling]);

  const unbind = useCallback(
    async (platform: string, label: string) => {
      if (!window.confirm(`解除 ${label} 机器人绑定？已配对的 IM 账号不会被删除。`)) return;
      try {
        await fetchJSON("/api/channels/bind/unbind", {
          method: "POST",
          body: JSON.stringify({ platform }),
        });
        toastSuccess(`已解除 ${label} 绑定`);
        await refreshStatus();
      } catch (e) {
        toastError(`解绑失败：${e instanceof Error ? e.message : String(e)}`);
      }
    },
    [refreshStatus],
  );

  const makePairCode = useCallback(async (platform: string) => {
    try {
      const res = await fetchJSON<PairingCodeResult>("/api/channels/pairing-code", {
        method: "POST",
        body: JSON.stringify({ platform }),
      });
      setPairCode(res);
    } catch (e) {
      toastError(`生成配对码失败：${e instanceof Error ? e.message : String(e)}`);
    }
  }, []);

  const copyCode = useCallback(async () => {
    if (!pairCode) return;
    try {
      await navigator.clipboard.writeText(pairCode.pairing_code);
      toastSuccess("配对码已复制");
    } catch {
      toastError("复制失败，请手动选中复制");
    }
  }, [pairCode]);

  const anyBound = status?.some((s) => s.bound) ?? false;

  return (
    <div className="space-y-6">
      {/* ── 第 1 步 · 绑定机器人（QQ / 企业微信 二选一）──────────────
          这是整条链路的入口：凭据不落盘，配对码就没地方发。 */}
      {/* ── 渠道区（2026-09-30 按参考设计重排）──────────────────────
          每个渠道一行、带自己的图标 / 状态 / 动作；不再用「第 1 步 / 第 2 步」
          的步骤框架 —— 那是同一条链路的两个动作，不是两个平级渠道。 */}
      <Card
        title="移动端远程控制"
        icon={<MessageSquare className="icon-md text-text-secondary" />}
        action={
          <button
            onClick={() => void refreshStatus()}
            className="h-7 px-2.5 rounded-md flex items-center gap-1 text-[length:var(--text-meta)] text-text-secondary hover:text-text-primary hover:bg-bg-card-hover border border-border-secondary"
            title="刷新渠道状态"
          >
            <RefreshCw className="icon-sm" />
            刷新
          </button>
        }
      >
        <p className="text-[length:var(--text-ui)] text-text-secondary">
          在手机上继续本机会话、远程批权限。绑定一个机器人渠道后，桌面端会收到它的消息并在这里显示。
        </p>
        {status === null ? (
          statusError ? (
            <div className="flex items-start gap-2 text-[length:var(--text-meta)] text-text-secondary">
              <AlertTriangle className="icon-md mt-0.5 shrink-0" />
              <div>
                读取绑定状态失败：<span className="font-mono">{statusError}</span>
                <div className="mt-1 text-text-muted">
                  通常是桌面端版本不一致 —— 重启开发脚本让后端重新编译后再试。
                </div>
              </div>
              <button
                onClick={() => void refreshStatus()}
                className="ml-auto h-7 px-2.5 shrink-0 rounded-md text-[length:var(--text-meta)] text-text-secondary hover:text-text-primary hover:bg-bg-card-hover border border-border-secondary"
              >
                重试
              </button>
            </div>
          ) : (
            <div className="text-[length:var(--text-meta)] text-text-muted">正在读取…</div>
          )
        ) : (
          PLATFORMS.map((p) => {
            const s = status.find((x) => x.platform === p.id);
            const bound = s?.bound ?? false;
            const isBinding = binding?.platform === p.id;
            return (
              <div key={p.id} className="rounded-lg border border-border-secondary bg-bg-card p-4 space-y-3">
                <div className="flex items-start justify-between gap-4">
                  <div className="min-w-0">
                    <div className="flex items-center gap-2">
                      <span
                        className="inline-grid place-items-center h-6 w-6 rounded-full text-[length:var(--text-tiny)] font-semibold shrink-0"
                        style={{ background: p.markBg, color: p.markFg }}
                        aria-hidden="true"
                      >
                        {p.mark}
                      </span>
                      <span className="text-[length:var(--text-ui)] font-semibold text-text-primary">{p.label}</span>
                      {bound ? (
                        <span className="inline-flex items-center gap-2 text-[length:var(--text-meta)] text-text-muted">
                          <span className="inline-flex items-center gap-1">
                            <Check className="icon-sm" />
                            已绑定 {s?.id_preview}
                            {s?.source === "env" && "（环境变量）"}
                          </span>
                          {live[p.id] ? (
                            <span className="inline-flex items-center gap-1 text-accent">
                              <span className="inline-block h-1.5 w-1.5 rounded-full bg-accent" />
                              通道已连接
                            </span>
                          ) : (
                            <span
                              className="inline-flex items-center gap-1 text-warning"
                              title="凭据在，但网关没连上（网络不通 / 凭据过期 / 机器人被停用）。机器人此时不会回消息。"
                            >
                              <AlertTriangle className="icon-sm" />
                              网关未连接
                            </span>
                          )}
                        </span>
                      ) : (
                        <span className="text-[length:var(--text-meta)] text-text-muted">未绑定</span>
                      )}
                    </div>
                    <p className="mt-1 text-[length:var(--text-meta)] text-text-muted">
                      {isBinding ? p.bindHint : bound ? `从${p.label}私聊机器人打开本机工作区。${p.hint}` : p.hint}
                    </p>
                  </div>

                  <div className="flex items-center gap-2 shrink-0">
                    {bound && (
                      <button
                        onClick={() => void makePairCode(p.id)}
                        className="h-7 px-2.5 rounded-md flex items-center gap-1 text-[length:var(--text-meta)] text-text-secondary hover:text-text-primary hover:bg-bg-card-hover border border-border-secondary"
                        title="生成一枚 6 位码，在 IM 私聊里发给机器人"
                      >
                        <QrCode className="icon-sm" />
                        配对码
                      </button>
                    )}
                    {bound && s?.source === "file" && (
                      <button
                        onClick={() => void unbind(p.id, p.label)}
                        className="h-7 px-2.5 rounded-md flex items-center gap-1 text-[length:var(--text-meta)] text-text-secondary hover:text-text-primary hover:bg-bg-card-hover border border-border-secondary"
                        title="删除本地保存的机器人凭据"
                      >
                        <Unlink className="icon-sm" />
                        解绑
                      </button>
                    )}
                    {isBinding ? (
                      <button
                        onClick={() => void cancelBind()}
                        className="h-7 px-2.5 rounded-md text-[length:var(--text-meta)] text-text-secondary hover:text-text-primary hover:bg-bg-card-hover border border-border-secondary"
                      >
                        取消
                      </button>
                    ) : (
                      <button
                        onClick={() => void startBind(p.id)}
                        disabled={busy}
                        className="h-7 px-2.5 rounded-md text-[length:var(--text-meta)] text-text-secondary hover:text-text-primary hover:bg-bg-card-hover border border-border-secondary disabled:opacity-50"
                      >
                        {bound ? "重新扫码" : "扫码绑定"}
                      </button>
                    )}
                  </div>
                </div>

                {isBinding && (
                  <div className="flex flex-wrap items-start gap-4 pt-3 border-t border-border-secondary">
                    <div
                      className="shrink-0 rounded-lg border border-border-secondary bg-white p-2 [&>svg]:block"
                      style={{ width: 152, height: 152 }}
                      dangerouslySetInnerHTML={{ __html: binding.svg }}
                    />
                    <div className="min-w-0 flex-1 space-y-2 pt-2">
                      <p className="text-[length:var(--text-ui)] text-text-secondary">{bindingNote ?? "请用手机扫码授权"}</p>
                      <a
                        href={binding.url}
                        target="_blank"
                        rel="noreferrer"
                        className="block truncate text-[length:var(--text-meta)] text-accent hover:underline"
                      >
                        {binding.url}
                      </a>
                    </div>
                  </div>
                )}

                {bindingNote && !isBinding && (
                  <p className="text-[length:var(--text-meta)] text-text-secondary">{bindingNote}</p>
                )}

                {bound && p.sandboxNote && (
                  <p className="flex items-start gap-2 text-[length:var(--text-meta)] text-text-muted leading-relaxed">
                    <AlertTriangle className="icon-sm mt-0.5 shrink-0 text-warning" />
                    <span>{p.sandboxNote}</span>
                  </p>
                )}
              </div>
            );
          })
        )}
      </Card>
      {/* ── 配对管理（置顶）——抄 cc-haha 的「配对管理」块位置 ── */}
      <Card
        title="配对与授权"
        icon={<LinkIcon className="icon-md text-text-secondary" />}
        action={
          <button
            onClick={() => void refreshStatus()}
            className="h-7 px-2.5 rounded-md flex items-center gap-1 text-[length:var(--text-meta)] text-text-secondary hover:text-text-primary hover:bg-bg-card-hover border border-border-secondary"
            title="刷新绑定状态"
          >
            <RefreshCw className="icon-sm" />
            刷新
          </button>
        }
      >
        <p className="text-[length:var(--text-ui)] text-text-secondary">
          配对码发到 IM 私聊里完成身份绑定。配对成功的账号会出现在下方「已配对设备」中，可随时撤销。
        </p>

        {statusError && (
          <div className="flex items-start gap-2 text-[length:var(--text-meta)] text-text-secondary">
            <AlertTriangle className="icon-md mt-0.5 shrink-0" />
            读取绑定状态失败：<span className="font-mono">{statusError}</span>
          </div>
        )}

        {pairCode ? (
          <div className="flex items-start gap-4 flex-wrap rounded-lg border border-border-secondary bg-bg-card-hover px-3 py-3">
            <div className="min-w-0 flex-1">
              <div className="text-[length:var(--text-meta)] text-text-secondary">
                在 {pairCode.platform === "qq" ? "QQ" : "企业微信"} 私聊机器人窗口里把这枚码发过去：
              </div>
              <div className="mt-1 flex items-center gap-2">
                <span className="font-mono font-bold tracking-[0.3em] text-[length:var(--text-display)] text-accent select-all">
                  {pairCode.pairing_code}
                </span>
                <button
                  onClick={() => void copyCode()}
                  className="h-7 w-7 rounded-md grid place-items-center text-text-secondary hover:text-text-primary hover:bg-bg-card border border-border-secondary"
                  title="复制配对码"
                >
                  <Copy className="icon-sm" />
                </button>
                <button
                  onClick={() => setPairCode(null)}
                  className="h-7 px-2.5 rounded-md text-[length:var(--text-meta)] text-text-secondary hover:text-text-primary hover:bg-bg-card border border-border-secondary"
                >
                  收起
                </button>
              </div>
              <div className="mt-1 text-[length:var(--text-meta)] text-text-muted">
                {pairCode.ttl_secs / 60} 分钟内有效、只能用一次。重新生成后旧码立即作废。连续输错 5 次只锁定该账号，不影响其它人。
              </div>
            </div>
          </div>
        ) : anyBound ? (
          <p className="text-[length:var(--text-meta)] text-text-muted leading-relaxed">
            点渠道行的「配对码」生成一枚 6 位码，在该渠道的私聊里发给机器人完成授权。
            <br />
            配对成功后，你从 QQ / 企业微信发的每条消息都会在桌面端左侧栏出现（带渠道徽标），
            桌面端也能直接在这条会话里回复 —— 同一个会话，双向。
          </p>
        ) : (
          <p className="text-[length:var(--text-meta)] text-text-muted">先在上面绑定一个机器人渠道，这里才能生成配对码。</p>
        )}
      </Card>

    </div>
  );
}
