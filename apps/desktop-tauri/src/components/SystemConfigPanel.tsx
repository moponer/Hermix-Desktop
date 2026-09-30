import { useCallback, useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import {
  Cpu,
  Boxes,
  Server,
  Smartphone,
  SlidersHorizontal,
  Plug,
  Sparkles,
  Network,
  Image as ImageIcon,
  X,
  Activity,
  Shield,
} from "lucide-react";
import { ModelListTab } from "./settings/ModelListTab";
import { SecurityTab } from "./settings/SecurityTab";
import { BackendTab } from "./settings/BackendSettingsTab";
import { AdvancedTab } from "./settings/AdvancedSettingsTab";
import { DiagnosticsTab } from "./settings/DiagnosticsTab";
import { RuntimesTab } from "./settings/RuntimesTab";
import { DevicesTab } from "./settings/DevicesTab";
import { PluginsTab, SkillsTab, ConnectorsTab, ToolsetsTab } from "./config/ToolTabs";
import { ToggleSwitch } from "./ToggleSwitch";
import { storage, STORAGE_KEYS } from "../utils/storageCompat";

interface SystemConfigPanelProps {
  onClose: () => void;
  /** 打开时落在哪个分类。默认「模型」——本应用的配置入口绝大多数是
   *  "没配模型/想换模型"，落到「工具」会让用户以为点错了地方。 */
  initialCategory?: ConfigCategory;
}

export type ConfigCategory = "model" | "tools" | "backend" | "advanced" | "diagnostics" | "devices" | "security";
type CategoryId = ConfigCategory;
type ToolTabId = "plugins" | "skills" | "connectors" | "toolsets";
type ModelTabId = "model" | "image";
type BackendTabId = "service" | "runtimes";

const CATEGORIES: { id: CategoryId; label: string; hint: string; icon: React.ElementType }[] = [
  { id: "model", label: "模型", hint: "配置大模型与图片生成", icon: Cpu },
  { id: "tools", label: "工具", hint: "插件、技能、连接器与工具集", icon: Boxes },
  { id: "backend", label: "后端", hint: "本地后端服务与健康", icon: Server },
  { id: "advanced", label: "高级", hint: "高级选项与实验功能", icon: SlidersHorizontal },
  { id: "devices", label: "设备管理", hint: "配对新设备与撤销权限", icon: Smartphone },
  { id: "diagnostics", label: "诊断", hint: "查看后端日志与排错", icon: Activity },
  { id: "security", label: "安全", hint: "自动备份与数据安全", icon: Shield },
];

const TOOL_TABS: { id: ToolTabId; label: string; icon: React.ElementType }[] = [
  { id: "plugins", label: "插件", icon: Plug },
  { id: "skills", label: "技能", icon: Sparkles },
  { id: "connectors", label: "连接器", icon: Network },
  { id: "toolsets", label: "工具集", icon: Boxes },
];

// 模型中心制：模型列表为默认视图，图像为二级 tab。
const MODEL_TABS: { id: ModelTabId; label: string; icon: React.ElementType }[] = [
  { id: "model", label: "模型列表", icon: Cpu },
  { id: "image", label: "图像", icon: ImageIcon },
];

const BACKEND_TABS: { id: BackendTabId; label: string; icon: React.ElementType }[] = [
  { id: "service", label: "服务", icon: Server },
  { id: "runtimes", label: "内置运行时", icon: Boxes },
];

export function SystemConfigPanel({ onClose, initialCategory }: SystemConfigPanelProps) {
  const [cat, setCat] = useState<CategoryId>(initialCategory ?? "model");
  const [toolTab, setToolTab] = useState<ToolTabId>("connectors");
  const [modelTab, setModelTab] = useState<ModelTabId>("model");
  const [backendTab, setBackendTab] = useState<BackendTabId>("service");
  const [query, setQuery] = useState("");
  const panelRef = useRef<HTMLDivElement>(null);

  // ESC closes
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [onClose]);

  // Lock body scroll
  useEffect(() => {
    document.body.style.overflow = "hidden";
    return () => { document.body.style.overflow = ""; };
  }, []);

  const handleBackdropClick = useCallback((e: React.MouseEvent<HTMLDivElement>) => {
    if (e.target === e.currentTarget) onClose();
  }, [onClose]);

  return createPortal(
    <div
      className="fixed inset-0 z-[100] flex items-center justify-center"
      style={{ background: "var(--color-overlay-scrim)" }}
      onMouseDown={handleBackdropClick}
    >
      <div
        ref={panelRef}
        role="dialog"
        aria-modal="true"
        aria-label="系统配置"
        className="flex flex-col rounded-xl border border-border-primary shadow-modal overflow-hidden"
        style={{ width: "min(96vw, 1100px)", height: "min(90vh, 780px)", background: "var(--color-bg-app)" }}
      >
        {/* Header */}
        <div className="flex items-center gap-3 px-5 py-3 border-b border-border-primary bg-bg-secondary shrink-0">
          <span className="text-sm font-semibold text-text-primary">系统配置</span>
          <span className="text-[length:var(--text-meta)] text-text-muted">模型 · 工具 · 后端 · 高级选项</span>
          <button
            onClick={onClose}
            className="ml-auto h-7 w-7 rounded-md flex items-center justify-center text-text-muted hover:text-text-primary hover:bg-bg-hover transition-colors"
            title="关闭 (Esc)"
            aria-label="关闭"
          >
            <X className="icon-md" />
          </button>
        </div>

        {/* Body */}
        <div className="flex flex-1 min-h-0 overflow-hidden">
          {/* Left nav */}
          <nav className="w-[200px] shrink-0 border-r border-border-primary bg-bg-secondary px-2 py-3 flex flex-col gap-0.5">
            {CATEGORIES.map(({ id, label, hint, icon: Icon }) => (
              <button
                key={id}
                onClick={() => { setCat(id); if (id === "model") setModelTab("model"); if (id === "tools") setToolTab("connectors"); }}
                className={clsxNav(cat === id)}
              >
                <Icon className="icon-md shrink-0 opacity-70" />
                <span className="flex-1 min-w-0">
                  <span className="block text-[length:var(--text-ui)] leading-tight truncate font-medium">{label}</span>
                  <span className="block text-[length:var(--text-meta)] text-text-muted leading-tight truncate">{hint}</span>
                </span>
              </button>
            ))}
          </nav>

          {/* Right detail */}
          <div className="flex-1 min-w-0 flex flex-col bg-bg-app">
            {(cat === "model" || cat === "tools" || cat === "backend") && (
              <div className="flex items-center gap-1 px-4 py-2 border-b border-border-primary bg-bg-secondary shrink-0 overflow-x-auto">
                {cat === "backend" && BACKEND_TABS.map(({ id, label, icon: Icon }) => (
                  <button
                    key={id}
                    onClick={() => setBackendTab(id)}
                    className={`flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-[length:var(--text-ui)] transition-colors whitespace-nowrap ${
                      backendTab === id
                        ? "bg-bg-tertiary text-text-primary font-medium"
                        : "text-text-muted hover:text-text-secondary hover:bg-bg-hover"
                    }`}
                  >
                    <Icon className="icon-md" />
                    {label}
                  </button>
                ))}
                {cat === "model" && MODEL_TABS.map(({ id, label, icon: Icon }) => (
                  <button
                    key={id}
                    onClick={() => setModelTab(id)}
                    className={`flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-[length:var(--text-ui)] transition-colors whitespace-nowrap ${
                      modelTab === id
                        ? "bg-bg-tertiary text-text-primary font-medium"
                        : "text-text-muted hover:text-text-secondary hover:bg-bg-hover"
                    }`}
>
                    <Icon className="icon-md" />
                    {label}
                  </button>
                ))}
                {cat === "tools" && TOOL_TABS.map(({ id, label, icon: Icon }) => (
                  <button
                    key={id}
                    onClick={() => setToolTab(id)}
                    className={`flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-[length:var(--text-ui)] transition-colors whitespace-nowrap ${
                      toolTab === id
                        ? "bg-bg-tertiary text-text-primary font-medium"
                        : "text-text-muted hover:text-text-secondary hover:bg-bg-hover"
                    }`}
                  >
                    <Icon className="icon-md" />
                    {label}
                  </button>
                ))}
              </div>
            )}

            <div className="flex-1 overflow-y-auto min-h-0">
              {cat === "model" && modelTab === "model" && (
                <div className="h-full min-h-0">
                  <ModelListTab />
                </div>
              )}
              {cat === "model" && modelTab === "image" && <Padded><ImageSettingsCard /></Padded>}
              {cat === "tools" && (
                <div className="px-6 py-5 min-h-0">
                  <div className="max-w-4xl mx-auto">
                    {toolTab === "plugins" && <PluginsTab query={query} onQuery={setQuery} />}
                    {toolTab === "skills" && <SkillsTab query={query} onQuery={setQuery} />}
                    {toolTab === "connectors" && <ConnectorsTab query={query} onQuery={setQuery} />}
                    {toolTab === "toolsets" && <ToolsetsTab query={query} onQuery={setQuery} />}
                  </div>
                </div>
              )}
              {cat === "backend" && backendTab === "service" && <Padded><BackendTab /></Padded>}
              {cat === "backend" && backendTab === "runtimes" && <Padded wide><RuntimesTab /></Padded>}
              {cat === "advanced" && <Padded><AdvancedTab /></Padded>}
              {cat === "devices" && <Padded wide><DevicesTab /></Padded>}
              {cat === "diagnostics" && <Padded wide><DiagnosticsTab /></Padded>}
              {cat === "security" && <Padded wide><SecurityTab /></Padded>}
            </div>
          </div>
        </div>
      </div>
    </div>,
    document.body,
  );
}

function Padded({ children, wide }: { children: React.ReactNode; wide?: boolean }) {
  return (
    <div className="px-6 py-5 min-h-0">
      <div className={wide ? "max-w-5xl mx-auto" : "max-w-2xl mx-auto"}>{children}</div>
    </div>
  );
}

function clsxNav(active: boolean): string {
  return [
    "flex items-center gap-2.5 w-full px-2.5 py-2 rounded-lg text-left transition-colors",
    active
      ? "bg-bg-active text-accent"
      : "text-text-secondary hover:bg-bg-hover hover:text-text-primary",
  ].join(" ");
}

// ── 图像：本地图片生成偏好（前端持久化，作为图片能力接入前的集中配置入口） ──

interface ImageConfig {
  enabled: boolean;
  model: string;
}

function loadImageConfig(): ImageConfig {
  try {
    const raw = storage.getItem(STORAGE_KEYS.imageConfig);
    if (raw) return { enabled: false, model: "", ...(JSON.parse(raw) as Partial<ImageConfig>) };
  } catch { /* ignore */ }
  return { enabled: false, model: "" };
}

function ImageSettingsCard() {
  const [cfg, setCfg] = useState<ImageConfig>(loadImageConfig);

  const update = useCallback((next: Partial<ImageConfig>) => {
    setCfg((prev) => {
      const merged = { ...prev, ...next };
      storage.setItem(STORAGE_KEYS.imageConfig, JSON.stringify(merged));
      return merged;
    });
  }, []);

  return (
    <div className="space-y-4">
      <div>
        <h2 className="text-base font-medium text-text-primary">图像</h2>
        <p className="text-sm text-text-muted mt-1">配置图片生成能力。当前为本地偏好设置，待图片生成功能接入后自动生效。</p>
      </div>

      <div className="rounded-lg border border-border-primary bg-bg-card p-4 space-y-4">
        <div className="flex items-center justify-between">
          <div>
            <div className="text-sm text-text-primary">启用图片生成</div>
            <div className="text-[length:var(--text-meta)] text-text-muted mt-0.5">开启后，对话中可调用图片生成技能。</div>
          </div>
          <ToggleSwitch enabled={cfg.enabled} onToggle={() => update({ enabled: !cfg.enabled })} />
        </div>

        <div>
          <label className="text-[length:var(--text-meta)] text-text-secondary">图片模型</label>
          <input
            value={cfg.model}
            onChange={(e) => update({ model: e.target.value })}
            placeholder="例如 agnes-image-2.0"
            className="mt-1 w-full px-3 py-2 rounded-lg bg-bg-tertiary text-sm text-text-primary border border-border-secondary outline-none focus:border-accent transition-colors"
          />
          <p className="mt-1 text-[length:var(--text-meta)] text-text-muted">默认留空时由后端决定使用的图片模型。</p>
        </div>
      </div>
    </div>
  );
}
