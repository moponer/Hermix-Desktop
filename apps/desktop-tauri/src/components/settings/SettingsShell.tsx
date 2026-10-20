import {
  useCallback, useEffect, useMemo, useRef, useState,
  type CSSProperties, type ReactNode,
} from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  ArrowLeft, Search, Cpu, Boxes, Server, Smartphone, SlidersHorizontal,
  Plug, Sparkles, Network, Image as ImageIcon, Activity, Shield,
  Archive, Palette, Brain, AppWindow, Mic, X,
} from "lucide-react";

import { cx } from "../../lib/cx";
import { FileTypeIcon } from "@hermix/preview/FileTypeIcon";
import { storage, STORAGE_KEYS } from "../../utils/storageCompat";
import { Switch, SettingsGroup, SettingsRow, SegmentedControl } from "../ui";
import { toastWarning } from "../../hooks/useToast";
import {
  PluginsTab, SkillsTab, ConnectorsTab, ToolsetsTab,
} from "../config/ToolTabs";
import { ModelListTab } from "./ModelListTab";
import { MemoryTab } from "./MemoryTab";
import { SecurityTab } from "./SecurityTab";
import { BackendTab } from "./BackendSettingsTab";
import { AdvancedTab } from "./AdvancedSettingsTab";
import { DiagnosticsTab } from "./DiagnosticsTab";
import { RuntimesTab } from "./RuntimesTab";
import { DevicesTab } from "./DevicesTab";
import { VoiceTab } from "./VoiceTab";
import { GeneralTab } from "./GeneralTab";
import { AppearanceTab } from "./AppearanceTab";
import { ArchiveTab } from "./ArchiveTab";

/**
 * 设置：左导航（分组 + 搜索）+ 右内容区。
 *
 * **2026-10-19 起是浮层弹窗（dialog）**。此前一度改成整页
 * （`NavPage="systemconfig"`），理由写在这里存档：整页意味着设置与
 * 「应用 / 工作台」同级、切走时顺带关右栏。用户重新拍板要弹窗形态——
 * 调完设置就关、不打断当前会话，这是弹窗的价值；因此外壳交回
 * `ui/Modal`（焦点陷阱 / Esc / 点击外部关闭由它统一负责），本组件只
 * 保留 `variant="dialog"` 的布局能力，不再承担整页路由。
 *
 * 为什么从平铺分类改成分组：7 个平铺项在 780px 高的面板里要靠图标 +
 * 两行文字才能区分"工具"和"插件/技能/连接器/工具集"这四件同源的事。
 * 分组后"插件组四项"和"偏好组两项"的关系一眼可见，导航项也从 7 变 15
 * 但认知负担反而降——因为组名承担了归类信息。
 *
 * 四个工具子 tab 提升为一级导航项（原先藏在"工具"下面的横条里），
 * 与 ZCode 的"插件组直接列插件/技能/MCP/Agents"一致：它们本来就是四件
 * 独立的配置，不该挤在一条 200px 宽的横条里互相挤掉标签。
 */
export type SettingsPage =
  | "general" | "appearance" | "model" | "image"
  | "plugins" | "skills" | "connectors" | "toolsets"
  | "backend" | "runtimes" | "devices" | "voice" | "security" | "fileassoc" | "archive"
  | "memory" | "diagnostics" | "advanced";

interface PageDef {
  id: SettingsPage;
  label: string;
  /** 侧栏搜索命中用；同时作为内容区页头的说明文案。 */
  hint: string;
  icon: React.ElementType;
  /** 搜索命中用；不写就只能靠标题字面匹配。 */
  keywords?: string;
}

interface GroupDef {
  label: string;
  items: PageDef[];
}

const GROUPS: GroupDef[] = [
  {
    label: "偏好",
    items: [
      { id: "general", label: "通用", hint: "工作模式、输入快捷键、系统能力与数据位置", icon: SlidersHorizontal, keywords: "mode domain 编程 办公 通知 自启 索引" },
      { id: "appearance", label: "外观", hint: "主题、强调色与壁纸，立即生效并持久保存", icon: Palette, keywords: "theme 主题 深色 浅色 跟随系统 强调色 accent 对比度" },
      { id: "model", label: "模型", hint: "统一管理内置与自定义模型；内置供应商只需填 API Key", icon: Cpu, keywords: "model llm provider api key 供应商" },
      { id: "image", label: "截图", hint: "系统截图热键与开关", icon: ImageIcon, keywords: "image 截图 screenshot 热键 hotkey 快捷键" },
    ],
  },
  {
    label: "插件",
    items: [
      { id: "plugins", label: "插件", hint: "内置插件开关", icon: Plug, keywords: "plugin 插件" },
      { id: "skills", label: "技能", hint: "技能库与启停", icon: Sparkles, keywords: "skill 技能" },
      { id: "connectors", label: "连接器", hint: "MCP server 接入与社区目录", icon: Network, keywords: "mcp connector 连接器 服务器" },
      { id: "toolsets", label: "工具集", hint: "工具白名单", icon: Boxes, keywords: "toolset 工具 工具集" },
    ],
  },
  {
    label: "管理",
    items: [
      { id: "backend", label: "后端服务", hint: "本地服务与健康状态", icon: Server, keywords: "backend server 服务 端口 健康" },
      { id: "runtimes", label: "内置运行时", hint: "Node / Python / Git 便携内置，优先于系统 PATH", icon: Boxes, keywords: "runtime node python git 运行时" },
      { id: "devices", label: "设备管理", hint: "IM 机器人与桌面配对两条通道；高危工具一律在桌面端强制确认", icon: Smartphone, keywords: "device 设备 配对 权限" },
      { id: "voice", label: "手机语音识别", hint: "配置云端语音识别 provider，手机端长按说话直发该 provider；未开启则手机仅支持文字", icon: Mic, keywords: "voice 语音 识别 asr stt minimax 转文字 手机" },
      { id: "security", label: "安全", hint: "覆盖写入前先备份旧版本，误改可回滚", icon: Shield, keywords: "security 安全 备份 backup" },
      { id: "fileassoc", label: "文件关联", hint: "勾选后由 Hermix 文件查看器接管对应扩展名；取消即恢复原程序", icon: AppWindow, keywords: "file association 关联 打开方式 viewer 查看器 md pdf png" },
      { id: "archive", label: "已归档", hint: "归档只是从侧栏收起，数据仍在本机，取消归档即可恢复", icon: Archive, keywords: "archive 归档 已归档" },
      { id: "memory", label: "记忆管理", hint: "记忆图谱、角色提示词、全局记忆文本与每日记忆的查看与编辑", icon: Brain, keywords: "memory 记忆 图谱 记忆库 persona 角色 soul 长期记忆 每日记忆 遗忘" },
    ],
  },
  {
    label: "诊断",
    items: [
      { id: "diagnostics", label: "诊断", hint: "日志、审计与网络出口；后端日志写入 hermix_home/logs/", icon: Activity, keywords: "diagnostics 日志 log 审计 出网" },
      { id: "advanced", label: "高级", hint: "压缩、子 Agent 等内部参数；修改后需重启生效", icon: SlidersHorizontal, keywords: "advanced 高级 实验 config yaml" },
    ],
  },
];

const ALL_PAGES = GROUPS.flatMap((g) => g.items);

/** 旧 `ConfigCategory` 的 id → 新页。保留是为了不打断既有调用点。 */
const LEGACY_ALIAS: Record<string, SettingsPage> = {
  tools: "connectors",
  diagnostics: "diagnostics",
};

export function resolvePage(id?: string): SettingsPage {
  if (!id) return "general";
  const hit = ALL_PAGES.find((p) => p.id === id);
  if (hit) return hit.id;
  return LEGACY_ALIAS[id] ?? "general";
}

export interface SettingsShellProps {
  /** 关闭设置（dialog）或返回上一页（page）。 */
  onBack: () => void;
  initialPage?: string;
  /** `page`：带"返回工作区"条的整页；`dialog`：浮层内使用，无返回条，
   *  由外层 Modal 的标题栏和关闭按钮承担。默认 `page`。 */
  variant?: "page" | "dialog";
}

export function SettingsShell({ onBack, initialPage, variant = "page" }: SettingsShellProps) {
  const [page, setPage] = useState<SettingsPage>(() => resolvePage(initialPage));
  const [query, setQuery] = useState("");
  const [toolQuery, setToolQuery] = useState("");
  const searchRef = useRef<HTMLInputElement>(null);

  // cmd+K 聚焦搜索；ESC 先清空搜索、再返回上一页。搜索框抢走焦点后 ESC
  // 若直接返回，用户"只想退出搜索"却会被踢出设置页 —— 所以分两级。
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        searchRef.current?.focus();
        searchRef.current?.select();
        return;
      }
      if (e.key === "Escape") {
        if (query) { setQuery(""); return; }
        onBack();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onBack, query]);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return GROUPS;
    const terms = q.split(/\s+/);
    return GROUPS.map((g) => ({
      ...g,
      items: g.items.filter((p) => {
        const hay = `${p.label} ${p.hint} ${p.keywords ?? ""}`.toLowerCase();
        return terms.every((t) => hay.includes(t));
      }),
    })).filter((g) => g.items.length > 0);
  }, [query]);

  const current = useMemo(
    () => ALL_PAGES.find((p) => p.id === page) ?? ALL_PAGES[0],
    [page],
  );

  const go = useCallback((id: SettingsPage) => {
    setPage(id);
    setQuery("");
  }, []);

  const dialog = variant === "dialog";

  const navNode = (
    /* 无右边线：侧栏与内容区同底色，靠内容区自己的内边距与卡片分界。
       加一条竖线等于在已经平整的面上再画一刀。 */
    <nav className="flex w-[228px] shrink-0 flex-col bg-[var(--color-bg-secondary)]">
      <div className="px-3 pb-1 pt-3">
        <div className="relative">
          <Search className="icon-md pointer-events-none absolute left-2.5 top-1/2 -translate-y-1/2 text-[var(--color-text-muted)]" />
          <input
            ref={searchRef}
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="搜索设置…"
            aria-label="搜索设置"
            className="w-full rounded-[var(--radius-md)] bg-[var(--color-bg-tertiary)] py-2 pl-8 pr-2 text-[length:var(--text-ui)] text-[var(--color-text-primary)] outline-none transition-colors placeholder:text-[var(--color-text-muted)] focus-visible:ring-2 focus-visible:ring-[var(--color-accent)]"
          />
        </div>
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto px-2 pb-3">
        {filtered.length === 0 ? (
          <div className="px-3 py-6 text-center text-[length:var(--text-meta)] text-[var(--color-text-muted)]">
            没有匹配「{query}」的设置项
          </div>
        ) : (
          filtered.map((g) => (
            <div key={g.label} className="mb-2">
              <div className="px-3 pb-1 pt-4 text-[length:var(--text-meta)] font-medium text-[var(--color-text-faint)]">
                {g.label}
              </div>
              {g.items.map(({ id, label, icon: Icon }) => (
                <button
                  key={id}
                  onClick={() => go(id)}
                  aria-current={page === id ? "page" : undefined}
                  className={cx(
                    "flex w-full items-center gap-2.5 rounded-[var(--radius-md)] px-3 py-2 text-left transition-colors",
                    /* 选中态用「深一档的底 + 满墨文字」，强调色只留给开关和
                       主按钮——导航是位置，不是行动。把强调色铺在导航上，
                       整块侧栏会抢走内容的注意力。 */
                    page === id
                      ? "bg-[var(--color-bg-active)] text-[var(--color-text-primary)]"
                      : "text-[var(--color-text-secondary)] hover:bg-[var(--color-bg-tertiary)] hover:text-[var(--color-text-primary)]",
                  )}
                >
                  <Icon
                    className={cx(
                      "icon-lg shrink-0",
                      page === id ? "opacity-100" : "opacity-55",
                    )}
                  />
                  <span className="min-w-0 flex-1 truncate text-[length:var(--text-body)]">{label}</span>
                </button>
              ))}
            </div>
          ))
        )}
      </div>
    </nav>
  );

  /* 右内容：与侧栏同为案面（#f5f5f5），卡片才是抬起的白面。
     2026-10-19 反转了「页面最亮、卡片后退」的方向——那套让每张卡都得靠
     描边撑存在感；反过来（灰案 + 白卡 + 极淡投影）一眼就是「信息块」，
     也不需要边框。`--color-bg-card` 在这里被重写成 elevated，
     所有子页面里的 bg-card 自动变成白卡，无需逐页改类名。 */
  const contentNode = (
    <div
      className="min-w-0 flex-1 overflow-y-auto bg-[var(--color-bg-secondary)]"
      style={{ "--color-bg-card": "var(--color-bg-elevated)" } as CSSProperties}
    >
      <PageBody def={current} toolQuery={toolQuery} onToolQuery={setToolQuery} />
    </div>
  );

  if (dialog) {
    return (
      <div className="flex h-full min-h-0">
        {navNode}
        <div className="flex min-w-0 flex-1 flex-col">
          {/* 36px 拖拽条只压在内容列上：左导航保持通天，避免出现一条横贯
              面板的标题带（用户实测 56px 头部"又高又不舒服"）。整条都是
              移动把手，关闭钮除外。 */}
          <div
            data-modal-drag-handle
            className="flex h-9 shrink-0 cursor-grab select-none items-center justify-between bg-[var(--color-bg-secondary)] pl-6 pr-2 active:cursor-grabbing [touch-action:none]"
          >
            <span className="text-[length:var(--text-meta)] font-medium text-[var(--color-text-faint)]">
              设置
            </span>
            <button
              onClick={onBack}
              aria-label="关闭设置"
              className="grid size-7 cursor-pointer place-items-center rounded-full text-[var(--color-text-secondary)] transition-colors hover:bg-[var(--color-bg-hover)] hover:text-[var(--color-text-primary)] focus-visible:ring-2 focus-visible:ring-[var(--color-accent)]"
            >
              <X className="size-4" />
            </button>
          </div>
          <div className="flex min-h-0 flex-1 overflow-hidden">{contentNode}</div>
        </div>
      </div>
    );
  }

  return (
    <div className="flex h-full min-h-0 flex-col bg-[var(--color-bg-app)]">
      <div className="flex shrink-0 items-center gap-3 bg-[var(--color-bg-secondary)] px-4 py-2.5">
        <button
          onClick={onBack}
          className="flex items-center gap-1.5 rounded-md px-2 py-1 text-[length:var(--text-ui)] text-[var(--color-text-secondary)] transition-colors hover:bg-[var(--color-bg-hover)] hover:text-[var(--color-text-primary)]"
        >
          <ArrowLeft className="icon-sm" />
          返回工作区
        </button>
        <span className="text-[length:var(--text-ui)] font-medium text-[var(--color-text-primary)]">
          设置
        </span>
      </div>

      <div className="flex min-h-0 flex-1 overflow-hidden">
        {navNode}
        {contentNode}
      </div>
    </div>
  );
}

/** 内容区页头：页名 + 一句说明。每个 tab 的自带页级标题已并入这里
 *  （原描述文案迁入 PageDef.hint，侧栏搜索与页头共用一份）。 */
function PageHeading({ def }: { def: PageDef }) {
  return (
    <header className="mb-6">
      <h2 className="text-[length:var(--text-title)] font-semibold text-[var(--color-text-primary)]">{def.label}</h2>
      <p className="mt-1 text-[length:var(--text-ui)] text-[var(--color-text-muted)]">{def.hint}</p>
    </header>
  );
}

function PageBody({
  def, toolQuery, onToolQuery,
}: {
  def: PageDef;
  toolQuery: string;
  onToolQuery: (q: string) => void;
}) {
  switch (def.id) {
    case "general": return <Section><PageHeading def={def} /><GeneralTab /></Section>;
    case "appearance": return <Section><PageHeading def={def} /><AppearanceTab /></Section>;
    case "model": return <ModelListTab />;
    case "image": return <Section><PageHeading def={def} /><ScreenshotSettingsCard /></Section>;
    case "plugins": return <ToolSection><PageHeading def={def} /><PluginsTab query={toolQuery} onQuery={onToolQuery} /></ToolSection>;
    case "skills": return <ToolSection><PageHeading def={def} /><SkillsTab query={toolQuery} onQuery={onToolQuery} /></ToolSection>;
    case "connectors": return <ToolSection><PageHeading def={def} /><ConnectorsTab query={toolQuery} onQuery={onToolQuery} /></ToolSection>;
    case "toolsets": return <ToolSection><PageHeading def={def} /><ToolsetsTab query={toolQuery} onQuery={onToolQuery} /></ToolSection>;
    case "backend": return <Section><PageHeading def={def} /><BackendTab /></Section>;
    case "runtimes": return <Section wide><PageHeading def={def} /><RuntimesTab /></Section>;
    case "devices": return <Section wide><PageHeading def={def} /><DevicesTab /></Section>;
    case "voice": return <Section><PageHeading def={def} /><VoiceTab /></Section>;
    case "security": return <Section wide><PageHeading def={def} /><SecurityTab /></Section>;
    case "fileassoc": return <Section><PageHeading def={def} /><FileAssocTab /></Section>;
    case "archive": return <Section><PageHeading def={def} /><ArchiveTab /></Section>;
    case "memory": return <Section wide><PageHeading def={def} /><MemoryTab /></Section>;
    case "diagnostics": return <Section wide><PageHeading def={def} /><DiagnosticsTab /></Section>;
    case "advanced": return <Section><PageHeading def={def} /><AdvancedTab /></Section>;
  }
}

/* 内容左对齐（不再 mx-auto）：侧栏选中项与内容块共用同一条左起始线，
   扫视路径是直的；居中对齐会让每页的左边界随宽度漂移。 */
function Section({ children, wide }: { children: ReactNode; wide?: boolean }) {
  return (
    <div className="px-7 py-6">
      <div className={wide ? undefined : "max-w-[760px]"}>{children}</div>
    </div>
  );
}

function ToolSection({ children }: { children: ReactNode }) {
  return <div className="px-7 py-6">{children}</div>;
}

// ── 图像页：只剩系统截图偏好。图片生成设置已于 2026-10-18 移除——
// 无后端能力支撑，纯 localStorage 摆设（决策见前后端对接审查）。

// ── 图像：系统截图偏好（前端持久化 + Rust 即时应用） ──────────────
// 持久化走 localStorage（screenshotConfig），与 imageConfig 同模式；生效
// 走 screenshot_apply_config 命令即时重注册全局热键，启动时 main.tsx 回放。

interface ScreenshotConfig {
  enabled: boolean;
  hotkey: string;
}

// 显式宽化成 string：否则 const 字面量把 SegmentedControl 的泛型推成
// 字面量联合，与 cfg.hotkey（string）不兼容。
const SCREENSHOT_HOTKEYS: { value: string; label: string }[] = [
  { value: "alt+Q", label: "Alt+Q" },
  { value: "ctrl+Alt+A", label: "Ctrl+Alt+A" },
  { value: "printscreen", label: "PrtSc" },
  { value: "F4", label: "F4" },
];

function loadScreenshotConfig(): ScreenshotConfig {
  try {
    const raw = storage.getItem(STORAGE_KEYS.screenshotConfig);
    if (raw) return { enabled: true, hotkey: "alt+Q", ...(JSON.parse(raw) as Partial<ScreenshotConfig>) };
  } catch { /* ignore */ }
  // 无存储 = 从未配置过：保持 Rust setup 已注册的默认 Alt+Q，不触发 apply。
  return { enabled: true, hotkey: "alt+Q" };
}

function ScreenshotSettingsCard() {
  const [cfg, setCfg] = useState<ScreenshotConfig>(loadScreenshotConfig);
  const prevRef = useRef<ScreenshotConfig>(cfg);

  // 变更 → 持久化 + 即时应用。注册失败（热键被占用）时回滚 UI 并提示，
  // Rust 侧顺序是"先注册新、再解旧"，失败时旧热键仍在。
  const apply = (next: ScreenshotConfig) => {
    const prev = prevRef.current;
    setCfg(next);
    prevRef.current = next;
    storage.setItem(STORAGE_KEYS.screenshotConfig, JSON.stringify(next));
    void invoke("screenshot_apply_config", { enabled: next.enabled, hotkey: next.hotkey })
      .catch((err) => {
        setCfg(prev);
        prevRef.current = prev;
        storage.setItem(STORAGE_KEYS.screenshotConfig, JSON.stringify(prev));
        toastWarning(`截图热键未生效：${String(err)}`);
      });
  };

  return (
    <div className="mt-6 space-y-6">
      <SettingsGroup>
        <SettingsRow
          title="启用系统截图"
          description="全局热键或标题栏相机按钮截取屏幕，选区直接进剪贴板。"
          control={
            <Switch
              checked={cfg.enabled}
              onChange={(v) => apply({ ...cfg, enabled: v })}
              label="启用系统截图"
            />
          }
        />
        <SettingsRow
          title="截图热键"
          description="系统级全局热键，任何应用内可用；被占用时会提示并回滚。"
        >
          <SegmentedControl
            label="截图热键"
            size="sm"
            options={SCREENSHOT_HOTKEYS}
            value={cfg.hotkey}
            onChange={(v) => apply({ ...cfg, hotkey: v })}
            className="mt-2"
          />
        </SettingsRow>
      </SettingsGroup>
    </div>
  );
}

// ── 文件关联：viewer 接管扩展名（HKCU 登记，显式授权） ─────────────

interface FileAssocEntry {
  ext: string;
  owned: boolean;
}

function FileAssocTab() {
  const [entries, setEntries] = useState<FileAssocEntry[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);

  useEffect(() => {
    void invoke<FileAssocEntry[]>("file_assoc_list")
      .then(setEntries)
      .catch((e) => setError(String(e instanceof Error ? e.message : e)));
  }, []);

  const toggle = (ext: string, enable: boolean) => {
    setBusy(ext);
    setError(null);
    void invoke("file_assoc_set", { ext, enable })
      .then(() => invoke<FileAssocEntry[]>("file_assoc_list").then(setEntries))
      .catch((e) => setError(String(e instanceof Error ? e.message : e)))
      .finally(() => setBusy(null));
  };

  return (
    <div>
      {error && (
        <p className="mb-3 text-[length:var(--text-meta)] text-error">{error}</p>
      )}
      <SettingsGroup
        title="由 Hermix 文件查看器打开"
        description="勾选即明确授权查看器接管该扩展名的默认打开方式；取消勾选自动恢复勾选前的原程序关联。登记只写当前用户的注册表（HKCU），不需要管理员权限。"
      >
        {entries.map(({ ext, owned }) => (
          <SettingsRow
            key={ext}
            title={
              <span className="flex items-center gap-2">
                <FileTypeIcon name={`sample.${ext}`} size={16} />
                {`.${ext}`}
              </span>
            }
            description={owned ? "已由查看器接管" : "当前由其他程序处理"}
            control={
              <Switch
                checked={owned}
                disabled={busy === ext}
                onChange={(v) => toggle(ext, v)}
              />
            }
          />
        ))}
      </SettingsGroup>
    </div>
  );
}
