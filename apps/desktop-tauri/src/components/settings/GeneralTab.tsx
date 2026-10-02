import { useCallback, useEffect, useState } from "react";
import { Code2, Briefcase, FolderOpen, Copy, Check, BellRing } from "lucide-react";

import { useDomainMode } from "../../hooks/useExpert";
import { useSendKey } from "../../hooks/useSendKey";
import { useDesktopNotify } from "../../hooks/useDesktopNotify";
import {
  getDataDir, openDataDir, copyText,
  getAutostart, setAutostart, getTrayEnabled, syncTray,
} from "../../desktopBridge";
import { SettingsGroup, SettingsRow, SegmentedControl, Switch } from "../ui";
import { ChoiceCard } from "./ChoiceCard";
import { toastError } from "../../hooks/useToast";

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
      <header>
        <h2 className="text-[length:var(--text-body)] font-medium text-[var(--color-text-primary)]">
          通用
        </h2>
        <p className="mt-1 text-[length:var(--text-meta)] text-[var(--color-text-muted)]">
          工作模式、输入快捷键、系统能力与数据位置。
        </p>
      </header>

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
      <h3 className="px-1 text-[length:var(--text-ui)] font-medium text-[var(--color-text-primary)]">
        工作模式
      </h3>
      <div className="grid grid-cols-2 gap-3" role="radiogroup" aria-label="工作模式">
        <ChoiceCard
          label="编程开发"
          description="注入代码纪律，grep / 搜索类工具加权。"
          icon={<Code2 className="icon-lg" />}
          tone="info"
          selected={mode === "code"}
          onSelect={() => setMode("code")}
        />
        <ChoiceCard
          label="日常办公"
          description="由后端按每轮对话内容自动判断，不固定加权。"
          icon={<Briefcase className="icon-lg" />}
          tone="success"
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
  const [tray, setTrayOn] = useState<boolean | null>(null);
  const [busy, setBusy] = useState<"autostart" | "tray" | null>(null);
  const { enabled: notifyOn, setEnabled: setNotifyOn, notify } = useDesktopNotify();

  const refresh = useCallback(() => {
    getAutostart().then(setAutostartOn).catch(() => setAutostartOn(null));
    getTrayEnabled().then(setTrayOn).catch(() => setTrayOn(null));
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

  const toggleTray = async (next: boolean) => {
    setBusy("tray");
    setTrayOn(next);
    try {
      await syncTray(next);
      refresh();
    } catch (e) {
      setTrayOn(!next);
      toastError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
  };

  return (
    <SettingsGroup
      title="系统"
      description="这三项会改动本机状态，改动立即生效，无需重启。"
    >
      <SettingsRow
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
        title="显示在菜单栏"
        description="在系统托盘常驻图标，左键单击唤起主窗口，右键菜单可退出。"
        control={
          <Switch
            checked={tray === true}
            onChange={(v) => void toggleTray(v)}
            disabled={tray === null || busy === "tray"}
            label="显示在菜单栏（系统托盘）"
          />
        }
      />
      <SettingsRow
        title="回复完成时通知"
        description="一轮回复结束、且窗口当前没有焦点时弹系统通知。"
        control={
          <div className="flex items-center gap-2">
            <button
              onClick={() => notify("Hermix Desktop", "这是一条测试通知。", true)}
              className="flex h-7 items-center gap-1.5 rounded-lg border border-[var(--color-border-secondary)] px-2 text-[length:var(--text-meta)] text-[var(--color-text-secondary)] transition-colors hover:border-[var(--color-border-primary)] hover:text-[var(--color-text-primary)]"
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
      toastError("复制失败");
    }
  };

  return (
    <SettingsGroup
      title="数据"
      description="配置、会话、日志与备份都落在这里。"
    >
      <SettingsRow
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
              className="flex h-7 items-center gap-1.5 rounded-lg border border-[var(--color-border-secondary)] px-2 text-[length:var(--text-meta)] text-[var(--color-text-secondary)] transition-colors hover:border-[var(--color-border-primary)] hover:text-[var(--color-text-primary)] disabled:opacity-40"
            >
              {copied ? <Check className="icon-sm" /> : <Copy className="icon-sm" />}
              {copied ? "已复制" : "复制"}
            </button>
            <button
              onClick={() => {
                openDataDir().catch(() => toastError("打开目录失败"));
              }}
              disabled={!dir}
              className="flex h-7 items-center gap-1.5 rounded-lg border border-[var(--color-border-secondary)] px-2 text-[length:var(--text-meta)] text-[var(--color-text-secondary)] transition-colors hover:border-[var(--color-border-primary)] hover:text-[var(--color-text-primary)] disabled:opacity-40"
            >
              <FolderOpen className="icon-sm" /> 打开
            </button>
          </div>
        }
      />
    </SettingsGroup>
  );
}
