import { useCallback, useEffect, useState } from "react";
import {
  Code2, Briefcase, FolderOpen, Copy, Check, BellRing,
  Keyboard, Power, PanelTop, LogOut,
} from "lucide-react";

import { useDomainMode } from "../../hooks/useExpert";
import { useSendKey } from "../../hooks/useSendKey";
import { useDesktopNotify } from "../../hooks/useDesktopNotify";
import {
  getDataDir, openDataDir, copyText,
  getAutostart, setAutostart, getDesktopSettings, syncTray, setCloseAction,
  type DesktopSettings,
} from "../../desktopBridge";
import { SettingsGroup, SettingsRow, SegmentedControl, Switch, IconTile } from "../ui";
import { ChoiceCard } from "./ChoiceCard";
import { toastError } from "../../hooks/useToast";
import { COPIED, COPY_FAILED, failure } from "../strings";

/**
 * 通用页：工作模式 + 快捷键 + 系统能力 + 数据位置。
 *
 * 「系统」组的三个开关是本机副作用（写注册表 / 建托盘图标 / 发系统通知），
 * 不是数据操作，所以**回读一律走后端**，不落 localStorage：localStorage 里
 * 存一份"我认为开了"只会和真实系统状态漂移（用户去任务管理器禁用开机
 * 启动后，设置页还显示"已开启"）。每次进设置页重新读，切换成功再重读确认。
 */
export function GeneralTab() {
  return (
    <div className="space-y-7">
      <ModeCards />
      <SendKeyRow />
      <SystemGroup />
      <DataDirGroup />
    </div>
  );
}

/* ── L3.3 模式选择卡 ──────────────────────────────────────────────
   两张卡对应用户的两档心智模型（写代码 / 办公）。存的是 auto | code，
   与后端 `AgentOverrides.domain_override` 的取值域一致——设置页写进去的
   值就是每轮发消息时带上的那个值，中间不经过第二份映射。 */
function ModeCards() {
  const [mode, setMode] = useDomainMode();

  return (
    <section className="space-y-2">
      <h3 className="px-1 text-[length:var(--text-ui)] font-medium text-[var(--color-text-secondary)]">
        工作模式
      </h3>
      <div className="grid grid-cols-2 gap-3" role="radiogroup" aria-label="工作模式">
        <ChoiceCard
          label="编程开发"
          description="注入代码纪律，grep / 搜索类工具加权。"
          icon={<Code2 className="icon-lg" />}
          tone="blue"
          selected={mode === "code"}
          onSelect={() => setMode("code")}
        />
        <ChoiceCard
          label="日常办公"
          description="由后端按每轮对话内容自动判断，不固定加权。"
          icon={<Briefcase className="icon-lg" />}
          tone="green"
          selected={mode === "auto"}
          onSelect={() => setMode("auto")}
        />
      </div>
    </section>
  );
}

/* ── L3.8 发送快捷键 ──────────────────────────────────────────────
   Composer 的 keydown 走 ChatView.handleKeyDown（原本写死 Enter 发送）。
   两边共用 useSendKey：设置页写、输入框读，经 localStorage + 自定义事件
   同步，不会出现"设置页显示已改、输入框还是旧行为"的分裂。 */
function SendKeyRow() {
  const [value, setValue] = useSendKey();

  return (
    <SettingsGroup title="输入">
      <SettingsRow
        icon={<IconTile icon={<Keyboard className="icon-md" />} tone="amber" />}
        title="发送快捷键"
        description={
          value === "enter"
            ? "Enter 发送，Shift + Enter 换行。"
            : "Ctrl + Enter 发送，Enter 换行。"
        }
        control={
          <SegmentedControl
            label="发送快捷键"
            value={value}
            onChange={setValue}
            options={[
              { value: "enter", label: "Enter" },
              { value: "ctrl_enter", label: "Ctrl + Enter" },
            ]}
          />
        }
      />
    </SettingsGroup>
  );
}

/* ── L4 系统能力 ──────────────────────────────────────────────────
   三个开关都读真实系统状态。切换时先乐观更新再回读确认：回读失败要把
   状态**弹回去**，不能停在乐观值——那正是「显示已开启但实际没生效」这类
   幽灵 bug 的成因。 */
function SystemGroup() {
  const [autostart, setAutostartOn] = useState<boolean | null>(null);
  // 托盘开关 + 关窗去向合在一份 DesktopSettings（后端单文件持久化），
  // 回读一次同时刷新两项，不会出现「托盘关了、关窗行为还显示旧值」。
  const [settings, setSettings] = useState<DesktopSettings | null>(null);
  const [busy, setBusy] = useState<"autostart" | "tray" | "close" | null>(null);
  const { enabled: notifyOn, setEnabled: setNotifyOn, notify } = useDesktopNotify();

  const refresh = useCallback(() => {
    getAutostart().then(setAutostartOn).catch(() => setAutostartOn(null));
    getDesktopSettings().then(setSettings).catch(() => setSettings(null));
  }, []);

  useEffect(() => { refresh(); }, [refresh]);

  const toggleAutostart = async (next: boolean) => {
    setBusy("autostart");
    setAutostartOn(next);
    try {
      await setAutostart(next);
      refresh();
    } catch (e) {
      setAutostartOn(!next);
      toastError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
  };

  const patchSettings = (patch: Partial<DesktopSettings>) =>
    setSettings((s) => (s ? { ...s, ...patch } : s));

  const toggleTray = async (next: boolean) => {
    setBusy("tray");
    patchSettings({ tray: next });
    try {
      await syncTray(next);
      refresh();
    } catch (e) {
      patchSettings({ tray: !next });
      toastError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
  };

  const changeCloseBehavior = async (closeToTray: boolean) => {
    setBusy("close");
    patchSettings({ close_to_tray: closeToTray });
    try {
      await setCloseAction(closeToTray);
      refresh();
    } catch (e) {
      patchSettings({ close_to_tray: !closeToTray });
      toastError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
  };

  return (
    <SettingsGroup
      title="系统"
      description="这些项会改动本机状态，改动立即生效，无需重启。"
    >
      <SettingsRow
        icon={<IconTile icon={<Power className="icon-md" />} tone="slate" />}
        title="开机自启动"
        description={
          autostart === null
            ? "读取系统状态中…"
            : "登录 Windows 后自动启动 Hermix。"
        }
        control={
          <Switch
            checked={autostart === true}
            onChange={(v) => void toggleAutostart(v)}
            disabled={autostart === null || busy === "autostart"}
            label="开机自启动"
          />
        }
      />
      <SettingsRow
        icon={<IconTile icon={<PanelTop className="icon-md" />} tone="slate" />}
        title="显示在菜单栏"
        description="在系统托盘常驻图标：左键单击唤起主窗口，右键菜单有任务、更新与退出。"
        control={
          <Switch
            checked={settings?.tray === true}
            onChange={(v) => void toggleTray(v)}
            disabled={settings === null || busy === "tray"}
            label="显示在菜单栏（系统托盘）"
          />
        }
      />
      <SettingsRow
        icon={<IconTile icon={<LogOut className="icon-md" />} tone="slate" />}
        title="关闭按钮行为"
        description={
          settings?.tray
            ? "最小化到托盘时回复与定时任务在后台继续；退出程序则完全结束。"
            : "关闭托盘后点关闭按钮将直接退出程序。"
        }
        control={
          <SegmentedControl
            label="关闭按钮行为"
            value={settings?.close_to_tray ? "tray" : "quit"}
            onChange={(v) => void changeCloseBehavior(v === "tray")}
            options={[
              { value: "tray", label: "最小化到托盘" },
              { value: "quit", label: "退出程序" },
            ]}
          />
        }
      />
      <SettingsRow
        icon={<IconTile icon={<BellRing className="icon-md" />} tone="amber" />}
        title="回复完成时通知"
        description="一轮回复结束、且窗口当前没有焦点时弹系统通知。"
        control={
          <div className="flex items-center gap-2">
            <button
              onClick={() => notify("Hermix Desktop", "这是一条测试通知。", true)}
              className="flex h-7 items-center gap-1.5 rounded-[var(--radius-md)] bg-[var(--color-bg-tertiary)] px-2 text-[length:var(--text-meta)] text-[var(--color-text-secondary)] transition-colors hover:bg-[var(--color-bg-quaternary)] hover:text-[var(--color-text-primary)]"
            >
              <BellRing className="icon-sm" /> 测试
            </button>
            <Switch
              checked={notifyOn}
              onChange={setNotifyOn}
              label="回复完成时发送桌面通知"
            />
          </div>
        }
      />
    </SettingsGroup>
  );
}

/* ── L3.7 数据目录 ────────────────────────────────────────────────
   路径来自 tauri `get_data_dir`，与「打开」按钮走同一命令，所以不会出现
   "显示一处、打开另一处"。只读展示：改数据目录要动便携模式的环境变量，
   做成一个能点的按钮反而会让人以为改了就能搬走。 */
function DataDirGroup() {
  const [dir, setDir] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    let cancelled = false;
    getDataDir()
      .then((d) => { if (!cancelled) setDir(d); })
      .catch(() => { if (!cancelled) setDir(null); });
    return () => { cancelled = true; };
  }, []);

  const copy = async () => {
    if (!dir) return;
    const ok = await copyText(dir);
    if (ok) {
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } else {
      toastError(failure(COPY_FAILED, "请手动选中内容后复制。"));
    }
  };

  return (
    <SettingsGroup
      title="数据"
      description="配置、会话、日志与备份都落在这里。"
    >
      <SettingsRow
        icon={<IconTile icon={<FolderOpen className="icon-md" />} tone="green" />}
        title="数据目录"
        description={
          dir ? (
            <span className="break-all font-mono">{dir}</span>
          ) : (
            <span>读取中…</span>
          )
        }
        control={
          <div className="flex items-center gap-1.5">
            <button
              onClick={copy}
              disabled={!dir}
              className="flex h-7 items-center gap-1.5 rounded-[var(--radius-md)] bg-[var(--color-bg-tertiary)] px-2 text-[length:var(--text-meta)] text-[var(--color-text-secondary)] transition-colors hover:bg-[var(--color-bg-quaternary)] hover:text-[var(--color-text-primary)] disabled:opacity-40"
            >
              {copied ? <Check className="icon-sm" /> : <Copy className="icon-sm" />}
              {copied ? COPIED : "复制"}
            </button>
            <button
              onClick={() => {
                openDataDir().catch(() => toastError("打开目录失败"));
              }}
              disabled={!dir}
              className="flex h-7 items-center gap-1.5 rounded-[var(--radius-md)] bg-[var(--color-bg-tertiary)] px-2 text-[length:var(--text-meta)] text-[var(--color-text-secondary)] transition-colors hover:bg-[var(--color-bg-quaternary)] hover:text-[var(--color-text-primary)] disabled:opacity-40"
            >
              <FolderOpen className="icon-sm" /> 打开
            </button>
          </div>
        }
      />
    </SettingsGroup>
  );
}
