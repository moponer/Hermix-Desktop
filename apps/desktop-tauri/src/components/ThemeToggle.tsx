import { useState, useRef, type ComponentType, type ReactNode } from "react";
import { Sun, Moon, Monitor, Check, ChevronDown, FileText } from "lucide-react";
import { useTheme, SYSTEM_THEME } from "../hooks/useTheme";
import { THEMES } from "../styles/themes/registry";
import { PopoverShell } from "./PopoverShell";

const ICONS: Record<string, ComponentType<{ className?: string }>> = {
  light: Sun,
  dark: Moon,
  notion: FileText,
  "notion-dark": FileText,
};
const DEFAULT_ICON = Monitor;
/** 跟随系统时触发器用 Monitor 图标（其字面义就是"跟着系统走"）。 */
const SYSTEM_ICON = Monitor;

export function ThemeToggle({ compact = false }: { compact?: boolean }) {
  const { mode, resolved, setMode } = useTheme();
  const followSystem = mode === SYSTEM_THEME;
  // 显式选主题时按 mode 取图标；跟随系统时用 Monitor，反映的是
  // "由系统决定"这个设置意图，而不是"此刻碰巧是深色"。
  const CurIcon = followSystem ? SYSTEM_ICON : (ICONS[mode] ?? DEFAULT_ICON);
  const [open, setOpen] = useState(false);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const containerRef = useRef<HTMLDivElement>(null);

  const label = followSystem
    ? `跟随系统（${THEMES.find((t) => t.id === resolved)?.label ?? resolved}）`
    : THEMES.find((t) => t.id === mode)?.label ?? mode;

  const pick = (id: string) => {
    setMode(id);
    setOpen(false);
  };

  // PopoverShell 走 portal + position:fixed 锚定触发器视口位置，对祖先
  // overflow 完全免疫。此前这里用的是 absolute 下拉，落在设置页右侧那栏
  // overflow-y:auto 里会被裁掉半截——"点了没反应"。
  // 顺带把原本自己写的 click-outside / ESC / 上下翻转逻辑一并交给它。
  const menu = open && (
    <PopoverShell
      containerRef={containerRef}
      triggerRef={triggerRef}
      align="right"
      side="bottom"
      width="sm"
    >
      <MenuItem
        active={followSystem}
        onClick={() => pick(SYSTEM_THEME)}
        icon={<Monitor className="icon-md" />}
        label="跟随系统"
      />
      <div className="my-1 h-px bg-[color:var(--color-border-subtle)]" />
      {THEMES.map((t) => (
        <MenuItem
          key={t.id}
          active={mode === t.id}
          onClick={() => pick(t.id)}
          icon={(() => {
            const Icon = ICONS[t.id] ?? DEFAULT_ICON;
            return <Icon className="icon-md" />;
          })()}
          label={t.label}
        />
      ))}
    </PopoverShell>
  );

  // compact：标题栏用的图标按钮（与 Menu / 窗口按钮同款 icon-btn 风格），点击展开菜单。
  if (compact) {
    return (
      <div className="relative" data-tauri-no-drag>
        <button
          ref={triggerRef}
          type="button"
          onClick={() => setOpen((v) => !v)}
          className="icon-btn"
          aria-label={`主题: ${label}`}
          aria-expanded={open}
          title={`主题: ${label} （点击展开）`}
        >
          <CurIcon className="icon-lg" />
        </button>
        {menu}
      </div>
    );
  }

  // 非 compact：设置页等处的「图标 + 文字 + 箭头」按钮。
  return (
    <div className="relative">
      <button
        ref={triggerRef}
        type="button"
        onClick={() => setOpen((v) => !v)}
        className="flex h-7 items-center gap-1.5 rounded-md border-[color:var(--color-border-primary)] bg-[color:var(--color-bg-elevated)] px-2 text-[var(--color-text-secondary)] transition-colors hover:bg-[color:var(--color-bg-hover)]"
        title={`主题: ${label}`}
        aria-expanded={open}
      >
        <CurIcon className="icon-md" />
        <span className="text-[length:var(--text-meta)]">{label}</span>
        <ChevronDown className="icon-sm opacity-60" />
      </button>
      {menu}
    </div>
  );
}

function MenuItem({
  active, onClick, icon, label,
}: {
  active: boolean;
  onClick: () => void;
  icon: ReactNode;
  label: string;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      role="menuitem"
      aria-checked={active}
      className={`flex w-full items-center gap-2 rounded-lg px-2.5 py-1.5 text-left text-[length:var(--text-ui)] transition-colors ${
        active
          ? "bg-[color:var(--color-accent-on-text)] text-[var(--color-accent)]"
          : "text-[var(--color-text-primary)] hover:bg-[color:var(--color-bg-hover)]"
      }`}
    >
      <span className="shrink-0 opacity-80">{icon}</span>
      <span className="grow truncate">{label}</span>
      {active && <Check className="icon-sm shrink-0" />}
    </button>
  );
}
