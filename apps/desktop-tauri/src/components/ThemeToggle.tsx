import { Sun, Moon, Monitor, Check, ChevronDown } from "lucide-react";
import { useState, useRef, useEffect } from "react";
import { useTheme, SYSTEM_THEME } from "../hooks/useTheme";
import { THEMES } from "../styles/themes/registry";

const ICONS: Record<string, React.ElementType> = {
  light: Sun,
  dark: Moon,
};
const DEFAULT_ICON = Monitor;
/** 跟随系统时触发器用 Monitor 图标（其字面义就是"跟着系统走"）。 */
const SYSTEM_ICON = Monitor;

export function ThemeToggle({ compact = false }: { compact?: boolean }) {
  const { mode, resolved, setMode } = useTheme();
  const followSystem = mode === SYSTEM_THEME;
  // 显式选主题时按 mode 取图标；跟随系统时按系统实际解析出的深浅取，
  // 这样图标反映的是屏幕上真实的样子，而不是"设置里存的是什么"。
  const CurIcon = followSystem ? SYSTEM_ICON : (ICONS[mode] ?? DEFAULT_ICON);
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const onClickAway = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    };
    const onEsc = (e: KeyboardEvent) => { if (e.key === "Escape") setOpen(false); };
    window.addEventListener("mousedown", onClickAway);
    window.addEventListener("keydown", onEsc);
    return () => {
      window.removeEventListener("mousedown", onClickAway);
      window.removeEventListener("keydown", onEsc);
    };
  }, [open]);

  const label = followSystem
    ? `跟随系统（${THEMES.find((t) => t.id === resolved)?.label ?? resolved}）`
    : THEMES.find((t) => t.id === mode)?.label ?? mode;

  // 下拉菜单：compact 与非 compact 共用同一份内容，只是触发器外观不同。
  const menu = open && (
    <div
      className="absolute right-0 top-full mt-1 min-w-[160px] rounded-md border border-border-primary bg-bg-elevated shadow-pop py-1 z-[1000]"
      role="menu"
    >
      {THEMES.map((t) => {
        const Icon = ICONS[t.id] ?? DEFAULT_ICON;
        const active = mode === t.id;
        return (
          <button
            key={t.id}
            type="button"
            onClick={() => { setMode(t.id); setOpen(false); }}
            className={`w-full flex items-center gap-2 px-2.5 py-1.5 text-left text-[length:var(--text-ui)] hover:bg-bg-hover transition-colors ${active ? "text-accent" : "text-text-primary"}`}
            role="menuitem"
          >
            <Icon className="icon-md shrink-0 opacity-80" />
            <span className="grow truncate">{t.label}</span>
            {active && <Check className="icon-sm shrink-0" />}
          </button>
        );
      })}
      <div className="my-1 border-t border-border-primary" />
      <button
        type="button"
        onClick={() => { setMode(SYSTEM_THEME); setOpen(false); }}
        className={`w-full flex items-center gap-2 px-2.5 py-1.5 text-left text-[length:var(--text-ui)] hover:bg-bg-hover transition-colors ${followSystem ? "text-accent" : "text-text-primary"}`}
        role="menuitem"
      >
        <Monitor className="icon-md shrink-0 opacity-80" />
        <span className="grow truncate">跟随系统</span>
        {followSystem && <Check className="icon-sm shrink-0" />}
      </button>
    </div>
  );

  // compact：标题栏用的图标按钮（与 Menu / 窗口按钮同款 icon-btn 风格），点击展开菜单。
  if (compact) {
    return (
      <div className="relative" ref={ref} data-tauri-no-drag>
        <button
          type="button"
          onClick={() => setOpen((v) => !v)}
          className="icon-btn"
          aria-label={`主题: ${label}`}
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
    <div className="relative" ref={ref}>
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        className="flex items-center gap-1.5 h-7 px-2 rounded-md border border-border-primary bg-bg-elevated text-text-secondary hover:bg-bg-hover transition-colors"
        title={`主题: ${label}`}
      >
        <CurIcon className="icon-md" />
        <span className="text-[length:var(--text-meta)]">{label}</span>
        <ChevronDown className="icon-sm opacity-60" />
      </button>
      {menu}
    </div>
  );
}
