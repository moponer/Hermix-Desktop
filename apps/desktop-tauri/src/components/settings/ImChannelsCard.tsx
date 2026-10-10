import { useCallback, useEffect, useRef, useState } from "react";
import {
  AlertTriangle,
  Building2,
  Check,
  ChevronDown,
  Clock3,
  Copy,
  MessageCircle,
  QrCode,
  RefreshCw,
  Smartphone,
  Unlink,
  type LucideIcon,
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
  /** 最近一次网关故障原文（后端 channels/status 的 detail，连上后为 null）。 */
  detail?: string | null;
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

/** `/api/pairing/sessions` 的一次性配对负载（pairing.rs::QrPayload）。 */
interface QrPayload {
  server_id: string;
  pairing_code: string;
  pairing_url: string;
}

export type ChannelId = "qq" | "wecom" | "mobile";

interface ChannelDef {
  id: ChannelId;
  label: string;
  /** 一句话说明，出现在右栏标题下方与左栏副标题。 */
  desc: string;
  Icon: LucideIcon;
  /** 徽标底色：贴近各渠道品牌主色，一眼认出是谁。 */
  brand: string;
  /** 「网关未连接」错误面板里的排查建议（detail 原文之上的一句话）。 */
  failHint: string;
}

const CHANNELS: ChannelDef[] = [
  {
    id: "qq",
    label: "QQ 机器人",
    desc: "QQ 开放平台机器人 · 只处理私聊 · 流式消息",
    Icon: MessageCircle,
    brand: "var(--color-brand-qq)",
    failHint: "查 AppSecret 是否填错、机器人是否在开放平台被停用，以及本机能否出网到 QQ 网关。",
  },
  {
    id: "wecom",
    label: "企业微信机器人",
    desc: "企业微信智能机器人 · 只处理单聊 · 气泡原地刷新",
    Icon: Building2,
    brand: "var(--color-brand-wecom)",
    failHint: "查 botid / secret 是否填错、智能机器人是否被停用，以及本机出网是否正常。",
  },
  {
    id: "mobile",
    label: "手机端",
    desc: "扫码或输入配对码，把这台电脑连到手机；手机上的会话与审批同步到这里",
    Icon: Smartphone,
    brand: "var(--color-accent)",
    failHint: "",
  },
];

/** IM 平台自己的绑定说明（与渠道定义分开，因为只有 IM 有扫码授权这一步）。 */
const IM_BIND: Record<string, { hint: string; sandboxNote: string }> = {
  qq: {
    hint: "用手机 QQ 扫码，创建并授权机器人，AppID 与 AppSecret 自动写入。",
    /* 新建机器人默认在【沙箱环境】：只有被加进白名单的 QQ 号能给它发消息，
       其他人私聊它收不到 —— 表现为「绑定了但机器人不回消息」的头号原因。 */
    sandboxNote:
      "刚建好的机器人默认处于沙箱环境：只有你在 QQ 开放平台把它加进「沙箱白名单」的那个 QQ 号才能私聊它并收到回复。要给别人用需发布到正式环境。",
  },
  wecom: {
    hint: "用企业微信扫码，创建并授权智能机器人，botid 与 secret 自动写入。",
    sandboxNote: "",
  },
};

/** `GET /api/relay/config` 的返回。凭据不回传——只回 `has_token`。 */
interface RelayConfigView {
  url: string;
  node_id: string;
  has_token: boolean;
  enabled: boolean;
  /** `disabled` / `connecting` / `connected` */
  state: "disabled" | "connecting" | "connected";
  attempts: number;
  last_error: string | null;
  /** 配置读坏了 —— 与「没配过」是两回事，必须分开显示 */
  load_error: string | null;
}

/** 面板里的小节标题。 */
function SubHead({ children }: { children: React.ReactNode }) {
  return (
    <div className="text-[length:var(--text-tiny)] font-medium uppercase tracking-wide text-text-muted pt-1">
      {children}
    </div>
  );
}

function ActionBtn({
  children,
  onClick,
  icon,
  disabled,
  title,
  tone = "ghost",
  className = "",
}: {
  children: React.ReactNode;
  onClick: () => void;
  icon?: React.ReactNode;
  disabled?: boolean;
  title?: string;
  tone?: "ghost" | "primary" | "danger";
  className?: string;
}) {
  const toneCls =
    tone === "primary"
      ? "bg-accent text-[var(--color-on-accent)] hover:opacity-90"
      : tone === "danger"
        ? "bg-error-soft text-error hover:bg-bg-card-hover"
        : "bg-bg-tertiary text-text-secondary hover:text-text-primary hover:bg-bg-active";
  return (
    <button
      onClick={onClick}
      disabled={disabled}
      title={title}
      className={
        "h-7 px-2.5 rounded-md flex items-center gap-1 text-[length:var(--text-meta)] transition-colors disabled:opacity-50 shrink-0 " +
        toneCls +
        " " +
        className
      }
    >
      {icon}
      {children}
    </button>
  );
}

/**
 * 连接渠道 —— 主从布局。
 *
 * ## 为什么从「竖着排三块」改成「左边选、右边配」
 *
 * 原来三个渠道各占一整块卡片竖着往下堆：一屏放不下两条渠道，扫一遍要滚动，
 * 而**「我现在配的是哪个渠道」这件事在竖排里根本没有位置**——三块长得一样，
 * 只能靠读标题才知道自己在看哪条。加上「配对与授权」又是一块，内容跟渠道
 * 一一对应却又独立成块，于是既重复又散。
 *
 * 三个渠道是**同一件事的三个选项**，不是三个步骤，所以形态就该是「三选一」：
 * 左栏是选择器（图标 + 名称 + 状态一眼扫完），右栏是**当前选中渠道的全部配置**。
 * 配对码从全局块挪进各渠道自身——它本来就是按平台生成的（`makePairCode(platform)`），
 * 挂在全局反而丢掉了「这枚码发给哪个机器人」这个前提。
 *
 * ## 层次不靠描边
 *
 * 全站与手机端统一：卡片用 `bg-bg-card`，控件用 `bg-bg-tertiary`，选中态用
 * 底色差 + 左缘强调条，浮起用阴影。本组件内**没有任何 border**。
 */
/**
 * 当前选中渠道由父级（`DevicesTab`）持有：下方的「已配对设备」列表要按选中
 * 渠道过滤——在企业微信下面看到 QQ 的设备与撤销按钮是串台，撤销也容易点错人。
 */
export function ImChannelsCard({
  active,
  onActiveChange,
}: {
  active: ChannelId;
  onActiveChange: (id: ChannelId) => void;
}) {
  const setActive = onActiveChange;
  const [status, setStatus] = useState<BindStatusItem[] | null>(null);
  const [statusError, setStatusError] = useState<string | null>(null);
  /** 网关是否真连上了。凭据在 ≠ 通道活着 —— 「机器人不回消息」多半卡在这里。 */
  const [live, setLive] = useState<Record<string, boolean>>({});
  /** 各平台最近一次网关故障原文（有 = 后端最近报过错，面板逐字展示）。 */
  const [liveDetail, setLiveDetail] = useState<Record<string, string>>({});
  /** live 数据是否真的来自服务端（否则不亮「网关未连接」，免得误报）。 */
  const [liveLoaded, setLiveLoaded] = useState(false);
  /** 正在扫码的平台 → 二维码。同一时刻只有一个平台在绑定。 */
  const [binding, setBinding] = useState<{ platform: string; url: string; svg: string } | null>(null);
  const [bindingNote, setBindingNote] = useState<string | null>(null);
  const [pairCode, setPairCode] = useState<PairingCodeResult | null>(null);
  /** 配对码生成时刻 —— 倒计时进度条的锚点（同 ZCode 的 bind-code 面板）。 */
  const [pairCodeAt, setPairCodeAt] = useState(0);
  const [nowTs, setNowTs] = useState(() => Date.now());
  const [busy, setBusy] = useState(false);
  const pollTimer = useRef<number | null>(null);
  const sessionKey = useRef<string | null>(null);

  /* ── 手机端配对二维码（原先在 DevicesTab，与渠道割裂；挪进来后二维码跟着
       渠道走，选到「手机端」才生成，不选就不做这份无用功）────────── */
  const [qr, setQr] = useState<{ payload: QrPayload; svg: string } | null>(null);
  const [qrError, setQrError] = useState<string | null>(null);

  /* ── relay（跨网络）配置 ─────────────────────────────────────────── */
  const [relay, setRelay] = useState<RelayConfigView | null>(null);
  const [relayErr, setRelayErr] = useState<string | null>(null);
  const [relayUrl, setRelayUrl] = useState("");
  const [relayToken, setRelayToken] = useState("");
  const [relayBusy, setRelayBusy] = useState(false);
  /** 「高级」折叠区默认收起：中转地址是自建服务才要动的东西，不该占主视图。 */
  const [relayAdvOpen, setRelayAdvOpen] = useState(false);
  // 「加一台手机」与其手动签发口已于 2026-10-10 移除。理由不是「一机一机」，
  // 真实模型是 **N:1**：一台手机可以连多台桌面（家里/公司），但**每台桌面同时
  // 只能被一台手机连着**——所以在这台桌面上再签发一条手机凭据，等于给同一台
  // 电脑开第二个入口，与该模型直接冲突。手机凭据只由配对码当场签发。
  // 签发接口 `/api/relay/phone-credential` 仍在后端，供脚本/排障使用。

  const refreshRelay = useCallback(async () => {
    try {
      const r = await fetchJSON<RelayConfigView>("/api/relay/config");
      setRelay(r);
      setRelayUrl(r.url);
      setRelayErr(null);
    } catch (e) {
      setRelayErr(e instanceof Error ? e.message : String(e));
      setRelay(null);
    }
  }, []);

  const saveRelay = useCallback(async () => {
    setRelayBusy(true);
    try {
      // token 一栏留空 = 保持不变，而不是清空。原因：凭据是掩码输入框，
      // 用户没重新填就提交时的直觉是「我没改它」，清空反而意外。
      await fetchJSON("/api/relay/config", {
        method: "PUT",
        body: JSON.stringify({ url: relayUrl.trim(), token: relayToken.trim() }),
      });
      setRelayToken("");
      await refreshRelay();
      toastSuccess("relay 配置已保存，重连会在下一次退避后自动生效");
    } catch (e) {
      toastError(`保存失败：${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setRelayBusy(false);
    }
  }, [relayUrl, relayToken, refreshRelay]);

  const refreshStatus = useCallback(async () => {
    try {
      const res = await fetchJSON<{ platforms: BindStatusItem[] }>("/api/channels/bind/status");
      setStatus(res.platforms);
      setStatusError(null);
      fetchJSON<{ platforms: ChannelStatusItem[] }>("/api/channels/status")
        .then((st) => {
          setLive(Object.fromEntries(st.platforms.map((p) => [p.platform, p.connected])));
          setLiveDetail(
            Object.fromEntries(
              st.platforms.filter((p) => p.detail).map((p) => [p.platform, p.detail as string]),
            ),
          );
          setLiveLoaded(true);
        })
        .catch(() => {
          setLive({});
          setLiveLoaded(false);
        });
    } catch (e) {
      setStatusError(e instanceof Error ? e.message : String(e));
      setStatus(null);
    }
  }, []);

  const generateQr = useCallback(async () => {
    setQrError(null);
    try {
      const payload = await fetchJSON<QrPayload>("/api/pairing/sessions", { method: "POST" });
      const svg = await QRCode.toString(payload.pairing_url, {
        type: "svg",
        margin: 1,
        errorCorrectionLevel: "M",
      });
      setQr({ payload, svg });
    } catch (e) {
      setQrError(e instanceof Error ? e.message : String(e));
      setQr(null);
    }
  }, []);

  useEffect(() => {
    void refreshStatus();
    return () => {
      if (pollTimer.current !== null) window.clearTimeout(pollTimer.current);
    };
  }, [refreshStatus]);

  // 手机端二维码：第一次切到该渠道时生成一次，之后复用（除非点重新生成）。
  // relay 配置同理——它在同一个面板里，跟着「手机端」这个渠道一起加载。
  const visited = useRef(false);
  useEffect(() => {
    if (active !== "mobile" || visited.current) return;
    visited.current = true;
    void generateQr();
    void refreshRelay();
  }, [active, generateQr, refreshRelay]);

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
      setPairCodeAt(Date.now());
      setPairCode(res);
    } catch (e) {
      toastError(`生成配对码失败：${e instanceof Error ? e.message : String(e)}`);
    }
  }, []);

  // 配对码在屏上时每 500ms 走一次时钟 —— 倒计时条 + 「已过期」态都靠它。
  useEffect(() => {
    if (!pairCode) return;
    const t = window.setInterval(() => setNowTs(Date.now()), 500);
    return () => window.clearInterval(t);
  }, [pairCode]);

  const copyCode = useCallback(async () => {
    if (!pairCode) return;
    try {
      await navigator.clipboard.writeText(pairCode.pairing_code);
      toastSuccess("配对码已复制");
    } catch {
      toastError("复制失败，请手动选中复制");
    }
  }, [pairCode]);

  // 完整配对链接（relay 地址 + 当场签发的手机凭据 + 电脑公钥）——即二维码
  // 内容。手机端只有扫码一条路：「粘贴链接」通道随局域网直连一起退役了。
  const copyUrl = useCallback(async () => {
    if (!qr) return;
    try {
      await navigator.clipboard.writeText(qr.payload.pairing_url);
      toastSuccess("配对链接已复制，去手机配对页粘贴");
    } catch {
      toastError("复制失败，请手动选中复制");
    }
  }, [qr]);

  // 配对码倒计时（同 ZCode bind-code 面板：三段变色进度条 + 过期态）。
  const pairTotalMs = pairCode ? pairCode.ttl_secs * 1000 : 0;
  const pairRemainingMs = pairCode ? Math.max(0, pairCodeAt + pairTotalMs - nowTs) : 0;
  const pairExpired = pairCode !== null && pairTotalMs > 0 && pairRemainingMs <= 0;
  const pairProgress = pairTotalMs > 0 ? pairRemainingMs / pairTotalMs : 0;

  /** 左栏每个渠道的状态摘要 —— 扫一眼就知道哪个通了、哪个没配。 */
  const railStatus = (c: ChannelDef): { text: string; tone: string; dot: string } => {
    if (c.id === "mobile") {
      if (relayErr) return { text: "relay 配置读取失败", tone: "text-error", dot: "bg-error" };
      if (relay?.state === "connected")
        return { text: "跨网络已接通", tone: "text-accent", dot: "bg-accent" };
      if (relay?.state === "connecting")
        return { text: "正在连接中转", tone: "text-warning", dot: "bg-warning" };
      if (relay?.enabled)
        return { text: "中转未接通", tone: "text-warning", dot: "bg-warning" };
      if (qrError) return { text: "二维码生成失败", tone: "text-error", dot: "bg-error" };
      return qr
        ? { text: "可扫码配对", tone: "text-text-secondary", dot: "bg-bg-active" }
        : { text: "正在准备…", tone: "text-text-muted", dot: "bg-bg-active" };
    }
    const s = status?.find((x) => x.platform === c.id);
    if (status === null) return { text: "读取中…", tone: "text-text-muted", dot: "bg-bg-active" };
    if (!s?.bound) return { text: "未绑定", tone: "text-text-muted", dot: "bg-bg-active" };
    if (live[c.id]) return { text: "通道已连接", tone: "text-accent", dot: "bg-accent" };
    return { text: "网关未连接", tone: "text-warning", dot: "bg-warning" };
  };

  const current = CHANNELS.find((c) => c.id === active)!;

  return (
    <div className="space-y-4">
      {/* ── 主从布局 ──────────────────────────────────────────────
          左 232px 是选择器，右栏自适应。窄窗口下自动折成上下（先选后配），
          避免 1024px 以下两栏互相挤压。 */}
      <div className="grid gap-3 md:grid-cols-[232px_1fr] items-start">
        {/* 左：渠道选择 */}
        <div className="rounded-xl bg-bg-secondary p-1.5 space-y-1">
          {CHANNELS.map((c) => {
            const st = railStatus(c);
            const on = c.id === active;
            return (
              <button
                key={c.id}
                onClick={() => setActive(c.id)}
                aria-current={on}
                className={
                  "w-full flex items-center gap-2.5 rounded-lg px-2.5 py-2.5 text-left transition-colors " +
                  // 选中态：底色 + 左侧强调条，不加阴影。
                  // 此前那个 `shadow-[0_1px_2px_rgba(0,0,0,0.35)]` 是任意阴影——
                  // 暗色下压得还行，浅色下是一圈灰晕（0.35 透明度配白底），把
                  // 底色差盖掉了，正是「分层不明显」的一部分。元数据里也没有
                  // 对应的令牌，是当初随手写的。
                  (on ? "bg-bg-card channel-item--on" : "hover:bg-bg-card-hover")
                }
              >
                <span
                  className="inline-grid place-items-center h-8 w-8 rounded-lg shrink-0"
                  style={{ background: c.brand, color: "var(--color-on-accent)" }}
                  aria-hidden="true"
                >
                  <c.Icon className="icon-md" />
                </span>
                <span className="min-w-0 flex-1">
                  <span className="block text-[length:var(--text-ui)] font-medium text-text-primary truncate">
                    {c.label}
                  </span>
                  <span className={"block text-[length:var(--text-tiny)] truncate " + st.tone}>
                    {st.text}
                  </span>
                </span>
                <span className={"h-1.5 w-1.5 rounded-full shrink-0 " + st.dot} aria-hidden="true" />
              </button>
            );
          })}
        </div>

        {/* 右：当前渠道的全部配置 */}
        <div className="rounded-xl bg-bg-card p-5 min-w-0">
          <div className="flex items-start gap-3">
            <span
              className="inline-grid place-items-center h-10 w-10 rounded-xl shrink-0"
              style={{ background: current.brand, color: "var(--color-on-accent)" }}
              aria-hidden="true"
            >
              <current.Icon className="icon-lg" />
            </span>
            <div className="min-w-0 flex-1">
              <div className="text-[length:var(--text-title)] font-semibold text-text-primary">
                {current.label}
              </div>
              <div className="mt-0.5 text-[length:var(--text-meta)] text-text-secondary">
                {current.desc}
              </div>
            </div>
            <ActionBtn onClick={() => void refreshStatus()} icon={<RefreshCw className="icon-sm" />} title="刷新渠道状态">
              刷新
            </ActionBtn>
          </div>

          {/* ── 手机端：扫码配对（经 relay）+ relay 中转连接 ──────── */}
          {active === "mobile" && (
            <div className="mt-5 space-y-6">
              <section className="space-y-4">
                <SubHead>扫码配对</SubHead>
                <div className="text-[length:var(--text-ui)] text-text-secondary leading-relaxed">
                  用手机相机扫下面的码，经中转（relay）把这台电脑连进去——
                  两台设备不必在同一网络。屏幕上的 6 位码用于扫码后核对身份：
                  <strong className="text-text-primary">只能用一次</strong>，重新生成后旧码立即作废。
                </div>

              {qrError ? (
                  <div className="rounded-lg bg-warning-soft px-4 py-3 space-y-2">
                    <div className="flex items-start gap-2 text-[length:var(--text-ui)] text-text-primary">
                      <AlertTriangle className="icon-md mt-0.5 shrink-0 text-warning" />
                      <span className="min-w-0 flex-1 break-all">{qrError}</span>
                    </div>
                    {/* 不再指向「去下方填 relay 地址」——地址已有内置默认，那个输入框
                        也不在主视图里。出码失败的真实原因就写在那句话里（未接中转 /
                        缺电脑侧凭据 / 这台电脑已经连着一台手机），照着做即可。 */}
                    <ActionBtn
                      onClick={() => void generateQr()}
                      icon={<RefreshCw className="icon-sm" />}
                      title="重新尝试"
                    >
                      重试
                    </ActionBtn>
                  </div>
                ) : qr ? (
                <div className="flex items-start gap-5 flex-wrap rounded-lg bg-bg-secondary p-4">
                  <div
                    className="shrink-0 rounded-lg bg-white p-2 [&>svg]:block"
                    style={{ width: 168, height: 168 }}
                    dangerouslySetInnerHTML={{ __html: qr.svg }}
                  />
                  <div className="min-w-0 flex-1 space-y-2">
                    <div className="flex items-center gap-2 flex-wrap">
                      <span className="text-[length:var(--text-meta)] text-text-muted">配对码</span>
                      <span className="font-mono font-bold tracking-[0.3em] text-[length:var(--text-display)] text-accent select-all">
                        {qr.payload.pairing_code}
                      </span>
                      <ActionBtn onClick={() => void copyCode()} icon={<Copy className="icon-sm" />} title="复制配对码">
                        复制
                      </ActionBtn>
                      <ActionBtn
                        onClick={() => void generateQr()}
                        icon={<RefreshCw className="icon-sm" />}
                        title="重新生成配对码（旧码立即作废）"
                      >
                        重新生成
                      </ActionBtn>
                    </div>
                    <div className="text-[length:var(--text-meta)] text-text-muted break-all flex items-center gap-1.5 flex-wrap">
                      <span>
                        扫码地址：<span className="font-mono text-text-secondary">{qr.payload.pairing_url}</span>
                      </span>
                      <ActionBtn onClick={() => void copyUrl()} icon={<Copy className="icon-sm" />} title="复制完整配对链接（备用；手机端以扫码为准）">
                        复制链接
                      </ActionBtn>
                    </div>
                    <div className="text-[length:var(--text-meta)] text-text-muted">
                      服务器：<span className="font-mono text-text-secondary">{qr.payload.server_id}</span>
                      {" · "}手机扫码后经中转绑定，两台设备不必在同一网络。
                    </div>
                    <div className="flex items-center gap-1.5 text-[length:var(--text-meta)] text-text-muted">
                      <Clock3 className="icon-sm shrink-0" />
                      60 分钟内有效；连续输错 5 次将锁定，需重新生成。
                    </div>
                  </div>
                </div>
              ) : (
                <div className="text-[length:var(--text-meta)] text-text-muted">正在生成配对码…</div>
              )}
              </section>

              {/* ── 中转连接（relay）──────────────────────────────────
                  扫码配对**依赖**这条通道：出配对码要当场向 relay 签发手机凭据，
                  配好之后聊天连接也常驻经 relay。放在同一个渠道面板里，因为它们
                  服务的都是「手机端」这一个渠道。

                  **地址不再是一个输入框**（2026-10-10）。地址是我们部署的，不是
                  用户该填的参数——此前摆一个必填框，用户拿到手机端却连不上，报错
                  还写「还没配置 relay 地址」。现在后端内置默认（relay_settings::
                  DEFAULT_RELAY_URL），界面不显示也不要求填写；自建服务才展开
                  地址框，见下面「高级」。 */}
              <section className="space-y-4">
                <SubHead>中转连接</SubHead>

                {relayErr ? (
                  <div className="flex items-start gap-2 text-[length:var(--text-ui)] text-text-secondary">
                    <AlertTriangle className="icon-md mt-0.5 shrink-0 text-error" />
                    <span className="min-w-0 flex-1">
                      读取中转配置失败：<span className="font-mono">{relayErr}</span>
                    </span>
                    <ActionBtn onClick={() => void refreshRelay()}>
                      重试
                    </ActionBtn>
                  </div>
                ) : !relay ? (
                  <div className="text-[length:var(--text-ui)] text-text-muted">正在读取…</div>
                ) : (
                  <>
                    {/* 状态一行说清，其余内容都从这个前提展开 */}
                    <div
                      className={`rounded-lg px-4 py-3 flex items-center gap-2 flex-wrap ${
                        relay.state === "connected" ? "bg-bg-secondary" : "bg-warning-soft"
                      }`}
                    >
                      {relay.state === "connected" ? (
                        <>
                          <span className="inline-block h-2 w-2 rounded-full bg-accent shrink-0" />
                          <span className="text-[length:var(--text-ui)] text-text-primary">
                            已连上中转
                          </span>
                        </>
                      ) : relay.state === "connecting" ? (
                        <>
                          <RefreshCw className="icon-sm animate-spin text-warning shrink-0" />
                          <span className="text-[length:var(--text-ui)] text-text-secondary">
                            正在连接（第 {relay.attempts} 次）
                          </span>
                        </>
                      ) : (
                        <>
                          <AlertTriangle className="icon-sm text-warning shrink-0" />
                          <span className="text-[length:var(--text-ui)] text-text-secondary">
                            尚未接通中转
                          </span>
                        </>
                      )}
                      <div className="ml-auto">
                        <ActionBtn
                          onClick={() => void refreshRelay()}
                          icon={<RefreshCw className="icon-sm" />}
                          title="刷新连接状态"
                        >
                          刷新
                        </ActionBtn>
                      </div>
                    </div>

                    {/* 配置读坏了 ≠ 没配过。两者排查方向相反，必须分开说。 */}
                    {relay.load_error && (
                      <div className="rounded-lg bg-error-soft px-4 py-3 text-[length:var(--text-meta)] text-text-secondary break-all">
                        {relay.load_error}
                      </div>
                    )}

                    {relay.last_error && (
                      <div className="text-[length:var(--text-meta)] text-text-muted break-all">
                        上次连接失败：{relay.last_error}
                      </div>
                    )}

                    {/* 首次设置：没有电脑侧凭据就只能停在这里，所以这是当前唯一的
                        阻塞项，放在最显眼处而不是塞进高级折叠区。 */}
                    {!relay.has_token && relay.node_id && (
                      <div className="rounded-lg bg-bg-secondary p-4 space-y-3">
                        <div className="text-[length:var(--text-ui)] text-text-primary">
                          还差一步：把这台电脑登记到中转
                        </div>
                        <div className="text-[length:var(--text-meta)] text-text-secondary leading-relaxed">
                          在中转服务器上执行下面这条命令，把打印出来的凭据填进去。
                          这一步刻意保持手动——如果电脑能自助登记，任何人都能凭空注册一台
                          「已登记的电脑」。做完这一次，之后生成配对码就不用再登服务器。
                        </div>
                        <code className="block break-all rounded-md bg-bg-tertiary px-3 py-2 font-mono text-[length:var(--text-meta)] text-accent select-all">
                          node src/relayctl.js add-node {relay.node_id}
                        </code>
                        <label className="block">
                          <span className="block text-[length:var(--text-meta)] text-text-muted mb-1.5">
                            电脑侧凭据（hrel_*）
                          </span>
                          <input
                            value={relayToken}
                            onChange={(e) => setRelayToken(e.target.value)}
                            placeholder="relayctl add-node 的输出"
                            type="password"
                            spellCheck={false}
                            className="w-full h-9 px-3 rounded-lg bg-bg-tertiary text-[length:var(--text-ui)] text-text-primary placeholder:text-text-faint focus:outline-none focus:ring-1 focus:ring-accent"
                          />
                        </label>
                        <ActionBtn
                          onClick={() => void saveRelay()}
                          disabled={relayBusy || !relayToken.trim()}
                          tone="primary"
                        >
                          {relayBusy ? "保存中…" : "保存并连接"}
                        </ActionBtn>
                      </div>
                    )}

                    {/* 高级：自建服务才需要动地址 */}
                    <div className="rounded-lg bg-bg-secondary overflow-hidden">
                      <button
                        type="button"
                        onClick={() => setRelayAdvOpen((v) => !v)}
                        className="w-full px-4 py-3 flex items-center gap-2 text-left hover:bg-bg-card-hover transition-colors"
                      >
                        <ChevronDown
                          className={`icon-sm shrink-0 text-text-muted transition-transform ${
                            relayAdvOpen ? "rotate-180" : ""
                          }`}
                        />
                        <span className="text-[length:var(--text-ui)] text-text-secondary">
                          高级
                        </span>
                        <span className="text-[length:var(--text-meta)] text-text-muted truncate-1">
                          自建中转服务
                        </span>
                      </button>

                      {relayAdvOpen && (
                        <div className="px-4 pb-4 space-y-3">
                          <div className="text-[length:var(--text-meta)] text-text-muted leading-relaxed">
                            默认走官方中转，不用管这一节。只有你自己部署了 relay 服务才需要改：
                            填上你的地址（留空 = 改回官方）。
                          </div>
                          <label className="block">
                            <span className="block text-[length:var(--text-meta)] text-text-muted mb-1.5">
                              自建中转地址（可选）
                            </span>
                            <input
                              value={relayUrl}
                              onChange={(e) => setRelayUrl(e.target.value)}
                              placeholder={relay.url || "wss://relay.example.com"}
                              spellCheck={false}
                              className="w-full h-9 px-3 rounded-lg bg-bg-tertiary text-[length:var(--text-ui)] text-text-primary placeholder:text-text-faint focus:outline-none focus:ring-1 focus:ring-accent"
                            />
                          </label>
                          <div className="flex items-center gap-3 flex-wrap">
                            <ActionBtn
                              onClick={() => void saveRelay()}
                              disabled={relayBusy}
                              tone="primary"
                            >
                              {relayBusy ? "保存中…" : "保存"}
                            </ActionBtn>
                            <span className="text-[length:var(--text-meta)] text-text-muted">
                              本机 id：<span className="font-mono">{relay.node_id || "（未生成）"}</span>
                            </span>
                          </div>
                        </div>
                      )}
                    </div>
                  </>
                )}
              </section>

            </div>
          )}

          {/* ── IM 渠道：绑定 + 配对码 ─────────────────────────── */}
          {active !== "mobile" && (
            <ImPane
              platform={active}
              status={status}
              statusError={statusError}
              live={live}
              liveDetail={liveDetail}
              liveLoaded={liveLoaded}
              binding={binding}
              bindingNote={bindingNote}
              pairCode={pairCode}
              pairCodeAt={pairCodeAt}
              pairRemainingMs={pairRemainingMs}
              pairExpired={pairExpired}
              pairProgress={pairProgress}
              busy={busy}
              onStartBind={startBind}
              onCancelBind={cancelBind}
              onUnbind={unbind}
              onMakePairCode={makePairCode}
              onCopyCode={copyCode}
              onRefresh={refreshStatus}
            />
          )}
        </div>
      </div>
    </div>
  );
}

/**
 * 单个 IM 渠道的配置面板。抽成独立组件是为了让主从外壳只管「选中谁」，
 * 不被每个渠道几十行细节淹没。
 */
function ImPane(props: {
  platform: string;
  status: BindStatusItem[] | null;
  statusError: string | null;
  live: Record<string, boolean>;
  liveDetail: Record<string, string>;
  liveLoaded: boolean;
  binding: { platform: string; url: string; svg: string } | null;
  bindingNote: string | null;
  pairCode: PairingCodeResult | null;
  pairCodeAt: number;
  pairRemainingMs: number;
  pairExpired: boolean;
  pairProgress: number;
  busy: boolean;
  onStartBind: (platform: string) => Promise<void>;
  onCancelBind: () => Promise<void>;
  onUnbind: (platform: string, label: string) => Promise<void>;
  onMakePairCode: (platform: string) => Promise<void>;
  onCopyCode: () => Promise<void>;
  onRefresh: () => Promise<void>;
}) {
  const {
    platform,
    status,
    statusError,
    live,
    liveDetail,
    liveLoaded,
    binding,
    bindingNote,
    pairCode,
    pairRemainingMs,
    pairExpired,
    pairProgress,
    busy,
    onStartBind,
    onCancelBind,
    onUnbind,
    onMakePairCode,
    onCopyCode,
    onRefresh,
  } = props;

  const def = CHANNELS.find((c) => c.id === platform)!;
  const label = def.label.replace(" 机器人", "");
  const s = status?.find((x) => x.platform === platform);
  const bound = s?.bound ?? false;
  const isBinding = binding?.platform === platform;
  const myCode = pairCode?.platform === platform ? pairCode : null;
  const meta = IM_BIND[platform];

  if (status === null) {
    return statusError ? (
      <div className="mt-5 flex items-start gap-2 text-[length:var(--text-meta)] text-text-secondary">
        <AlertTriangle className="icon-md mt-0.5 shrink-0" />
        <div>
          读取绑定状态失败：<span className="font-mono">{statusError}</span>
          <div className="mt-1 text-text-muted">
            通常是桌面端版本不一致 —— 重启开发脚本让后端重新编译后再试。
          </div>
        </div>
        <ActionBtn onClick={() => void onRefresh()} className="ml-auto">
          重试
        </ActionBtn>
      </div>
    ) : (
      <div className="mt-5 text-[length:var(--text-meta)] text-text-muted">正在读取绑定状态…</div>
    );
  }

  return (
    <div className="mt-5 space-y-4">
      {/* 状态条：凭据在 ≠ 通道活着，两件事分开说 */}
      <div className="flex items-center gap-3 flex-wrap rounded-lg bg-bg-secondary px-4 py-3">
        {bound ? (
          <>
            <span className="inline-flex items-center gap-1.5 text-[length:var(--text-ui)] text-text-primary">
              <Check className="icon-sm text-accent" />
              已绑定 <span className="font-mono">{s?.id_preview}</span>
              {s?.source === "env" && <span className="text-text-muted">（环境变量）</span>}
            </span>
            {live[platform] ? (
              <span className="inline-flex items-center gap-1.5 text-[length:var(--text-meta)] text-accent">
                <span className="inline-block h-1.5 w-1.5 rounded-full bg-accent" />
                通道已连接
              </span>
            ) : liveLoaded ? (
              <span
                className="inline-flex items-center gap-1.5 text-[length:var(--text-meta)] text-warning"
                title="凭据在，但网关没连上（网络不通 / 凭据过期 / 机器人被停用）。机器人此时不会回消息。"
              >
                <AlertTriangle className="icon-sm" />
                网关未连接
              </span>
            ) : null}
          </>
        ) : (
          <span className="text-[length:var(--text-ui)] text-text-muted">未绑定 —— 先扫码把机器人授权进来</span>
        )}

        <div className="ml-auto flex items-center gap-2">
          {bound && (
            <ActionBtn
              onClick={() => void onMakePairCode(platform)}
              icon={<QrCode className="icon-sm" />}
              title="生成一枚 6 位码，在 IM 私聊里发给机器人"
            >
              配对码
            </ActionBtn>
          )}
          {bound && s?.source === "file" && (
            <ActionBtn
              onClick={() => void onUnbind(platform, label)}
              icon={<Unlink className="icon-sm" />}
              title="删除本地保存的机器人凭据"
            >
              解绑
            </ActionBtn>
          )}
          {isBinding ? (
            <ActionBtn onClick={() => void onCancelBind()}>取消</ActionBtn>
          ) : (
            <ActionBtn
              onClick={() => void onStartBind(platform)}
              disabled={busy}
              icon={<QrCode className="icon-sm" />}
              tone="primary"
            >
              {bound ? "重新扫码" : "扫码绑定"}
            </ActionBtn>
          )}
        </div>
      </div>

      <p className="text-[length:var(--text-meta)] text-text-muted leading-relaxed">
        {isBinding ? meta?.hint : bound ? `从${label}私聊机器人打开本机工作区。${def.desc}` : def.desc}
      </p>

      {/* 网关连接失败：detail 原文 + 一句人话排查方向 */}
      {bound && liveLoaded && !live[platform] && (
        <div className="flex items-start gap-2 rounded-lg bg-bg-secondary px-4 py-3">
          <AlertTriangle className="icon-sm mt-0.5 shrink-0 text-warning" />
          <div className="min-w-0">
            <div className="text-[length:var(--text-ui)] font-medium text-text-primary">网关连接失败</div>
            {liveDetail[platform] ? (
              <div className="mt-0.5 break-all font-mono text-[length:var(--text-meta)] text-text-secondary">
                {liveDetail[platform]}
              </div>
            ) : null}
            <div className="mt-1 text-[length:var(--text-meta)] text-text-muted">{def.failHint}</div>
          </div>
        </div>
      )}

      {/* 扫码授权中的二维码 */}
      {isBinding && (
        <div className="flex items-start gap-4 flex-wrap rounded-lg bg-bg-secondary p-4">
          <div
            className="shrink-0 rounded-lg bg-white p-2 [&>svg]:block"
            style={{ width: 152, height: 152 }}
            dangerouslySetInnerHTML={{ __html: binding!.svg }}
          />
          <div className="min-w-0 flex-1 space-y-2 self-center">
            <p className="text-[length:var(--text-ui)] text-text-secondary">{bindingNote ?? "请用手机扫码授权"}</p>
            <a
              href={binding!.url}
              target="_blank"
              rel="noreferrer"
              className="block truncate text-[length:var(--text-meta)] text-accent hover:underline"
            >
              {binding!.url}
            </a>
          </div>
        </div>
      )}

      {bindingNote && !isBinding && (
        <p className="text-[length:var(--text-meta)] text-text-secondary">{bindingNote}</p>
      )}

      {bound && meta?.sandboxNote && (
        <p className="flex items-start gap-2 rounded-lg bg-warning-soft px-4 py-3 text-[length:var(--text-meta)] text-text-secondary leading-relaxed">
          <AlertTriangle className="icon-sm mt-0.5 shrink-0 text-warning" />
          <span>{meta.sandboxNote}</span>
        </p>
      )}

      {/* 配对码：跟着渠道走，不再是全局一块 */}
      {myCode && (
        <div className="rounded-lg bg-bg-secondary p-4">
          <SubHead>配对码</SubHead>
          <div className="mt-2 text-[length:var(--text-ui)] text-text-secondary">
            在 {label} 私聊机器人窗口里把这枚码发过去：
          </div>
          <div className="mt-2 flex items-center gap-2 flex-wrap">
            <span className="font-mono font-bold tracking-[0.3em] text-[length:var(--text-display)] text-accent select-all">
              {myCode.pairing_code}
            </span>
            <ActionBtn onClick={() => void onCopyCode()} disabled={pairExpired} icon={<Copy className="icon-sm" />}>
              复制
            </ActionBtn>
            {pairExpired && (
              <ActionBtn onClick={() => void onMakePairCode(platform)} tone="primary">
                重新生成
              </ActionBtn>
            )}
            <ActionBtn onClick={() => onRefresh()}>收起</ActionBtn>
          </div>
          <div className="mt-2 flex items-center gap-1.5 text-[length:var(--text-meta)] text-text-muted">
            <Clock3 className="icon-sm shrink-0" />
            {pairExpired
              ? "码已过期，点「重新生成」再发一次"
              : `${Math.ceil(pairRemainingMs / 1000)} 秒内有效、只能用一次；重新生成后旧码立即作废。连续输错 5 次只锁定该账号。`}
          </div>
          <div className="mt-2 h-1 overflow-hidden rounded-full bg-bg-tertiary">
            <div
              className={
                "h-full " +
                (pairExpired
                  ? "bg-text-faint"
                  : pairProgress <= 0.1
                    ? "bg-error"
                    : pairRemainingMs <= 10_000
                      ? "bg-warning"
                      : "bg-accent")
              }
              style={{
                width: "100%",
                transformOrigin: "left",
                transform: `scaleX(${pairProgress})`,
                transition: "transform 300ms linear",
              }}
            />
          </div>
        </div>
      )}

      {bound && !myCode && (
        <p className="text-[length:var(--text-meta)] text-text-muted leading-relaxed">
          点上方「配对码」生成一枚 6 位码，在 {label} 私聊里发给机器人完成授权。配对成功后，你从{" "}
          {label} 发的每条消息都会在桌面端左侧栏出现（带渠道徽标），桌面端也能直接回复 —— 同一个会话，双向。
        </p>
      )}
    </div>
  );
}
