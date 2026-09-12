import { useState, useRef, useEffect, useCallback } from "react";
import { createPortal } from "react-dom";
import { clsx } from "clsx";
import {
  Globe, Palette, ZoomIn, BarChart3, Unplug, ChevronRight,
  Sun, Moon, Trees, Sunrise, Monitor, Check,
} from "lucide-react";
import { useTheme } from "../hooks/useTheme";
import { THEMES } from "../styles/themes/registry";
import { storage } from "../utils/storageCompat";

const THEME_ICONS: Record<string, React.ElementType> = {
  light: Sun, dark: Moon, rainforest: Trees, sunset: Sunrise,
};

const SCALE_OPTIONS = [
  { label: "小", value: 0.85 },
  { label: "标准", value: 1 },
  { label: "大", value: 1.15 },
  { label: "特大", value: 1.3 },
] as const;

const LANG_OPTIONS = [
  { label: "简体中文", value: "zh-CN" },
  { label: "English", value: "en" },
] as const;

const SCALE_KEY = "hermix.desktop.ui-scale";
const LANG_KEY = "hermix.desktop.lang";

function readScale(): number {
  try { return Number(storage.getItem(SCALE_KEY)) || 1; } catch { return 1; }
}
function writeScale(v: number) {
  try { storage.setItem(SCALE_KEY, String(v)); } catch {}
  document.documentElement.style.zoom = String(v);
}

function readLang(): string {
  try { return storage.getItem(LANG_KEY) || "zh-CN"; } catch { return "zh-CN"; }
}
function writeLang(v: string) {
  try { storage.setItem(LANG_KEY, v); } catch {}
}

interface UserMenuPopoverProps {
  open: boolean;
  onClose: () => void;
  anchorRef: React.RefObject<HTMLDivElement | null>;
  onNavigateAnalytics: () => void;
  onDisconnect: () => void;
}

export function UserMenuPopover({ open, onClose, anchorRef, onNavigateAnalytics, onDisconnect }: UserMenuPopoverProps) {
  const [subMenu, setSubMenu] = useState<string | null>(null);
  const popoverRef = useRef<HTMLDivElement>(null);
  const { mode, setMode } = useTheme();
  const [scale, setScale] = useState(readScale);
  const [lang, setLang] = useState(readLang);

  // Close on click-away / ESC
  useEffect(() => {
    if (!open) { setSubMenu(null); return; }
    const onDown = (e: MouseEvent) => {
      const t = e.target as Node;
      if (anchorRef.current?.contains(t) || popoverRef.current?.contains(t)) return;
      onClose();
    };
    const onEsc = (e: KeyboardEvent) => { if (e.key === "Escape") onClose(); };
    window.addEventListener("mousedown", onDown);
    window.addEventListener("keydown", onEsc);
    return () => { window.removeEventListener("mousedown", onDown); window.removeEventListener("keydown", onEsc); };
  }, [open, onClose, anchorRef]);

  // Position: above the anchor (bottom-left corner of menu ≈ bottom-right of anchor)
  const [pos, setPos] = useState<{ left: number; bottom: number }>({ left: 0, bottom: 0 });
  useEffect(() => {
    if (!open || !anchorRef.current) return;
    const r = anchorRef.current.getBoundingClientRect();
    setPos({ left: r.left, bottom: window.innerHeight - r.top + 4 });
  }, [open, anchorRef]);

  const handleSetScale = useCallback((v: number) => {
    setScale(v);
    writeScale(v);
  }, []);

  const handleSetLang = useCallback((v: string) => {
    setLang(v);
    writeLang(v);
  }, []);

  if (!open) return null;

  // ── Sub-menu renderers ──
  const themeSub = subMenu === "theme" && (
    <div className="user-menu-sub" role="menu">
      {THEMES.map((t) => {
        const Icon = THEME_ICONS[t.id] ?? Monitor;
        const active = mode === t.id;
        return (
          <button key={t.id} type="button" role="menuitem"
            onClick={() => { setMode(t.id); setSubMenu(null); }}
            className={clsx("user-menu-item", active && "is-active")}
          >
            <Icon className="icon-md shrink-0 opacity-70" />
            <span className="flex-1 truncate">{t.label}</span>
            {active && <Check className="icon-sm shrink-0 text-accent" />}
          </button>
        );
      })}
    </div>
  );

  const scaleSub = subMenu === "scale" && (
    <div className="user-menu-sub" role="menu">
      {SCALE_OPTIONS.map((o) => {
        const active = Math.abs(scale - o.value) < 0.02;
        return (
          <button key={o.value} type="button" role="menuitem"
            onClick={() => handleSetScale(o.value)}
            className={clsx("user-menu-item", active && "is-active")}
          >
            <ZoomIn className={`icon-md ${clsx("shrink-0 opacity-70", active && "text-accent")}`} />
            <span className="flex-1">{o.label}</span>
            <span className="text-[11px] text-text-faint">{Math.round(o.value * 100)}%</span>
            {active && <Check className="icon-sm shrink-0 text-accent" />}
          </button>
        );
      })}
    </div>
  );

  const langSub = subMenu === "lang" && (
    <div className="user-menu-sub" role="menu">
      {LANG_OPTIONS.map((o) => {
        const active = lang === o.value;
        return (
          <button key={o.value} type="button" role="menuitem"
            onClick={() => handleSetLang(o.value)}
            className={clsx("user-menu-item", active && "is-active")}
          >
            <Globe className={`icon-md ${clsx("shrink-0 opacity-70", active && "text-accent")}`} />
            <span className="flex-1">{o.label}</span>
            {active && <Check className="icon-sm shrink-0 text-accent" />}
          </button>
        );
      })}
    </div>
  );

  const currentLabel = (key: string) => {
    switch (key) {
      case "theme": return THEMES.find((t) => t.id === mode)?.label ?? mode;
      case "scale": return SCALE_OPTIONS.find((o) => Math.abs(scale - o.value) < 0.02)?.label ?? `${Math.round(scale * 100)}%`;
      case "lang": return LANG_OPTIONS.find((o) => o.value === lang)?.label ?? lang;
      default: return "";
    }
  };

  /* Main items */
  const items: { key: string; icon: React.ElementType; label: string; hasSub?: boolean; danger?: boolean; onClick?: () => void }[] = [
    { key: "lang", icon: Globe, label: "界面语言", hasSub: true },
    { key: "theme", icon: Palette, label: "界面主题", hasSub: true },
    { key: "scale", icon: ZoomIn, label: "界面缩放", hasSub: true },
    { key: "analytics", icon: BarChart3, label: "使用统计", onClick: () => { onNavigateAnalytics(); onClose(); } },
    { key: "disconnect", icon: Unplug, label: "断开连接", danger: true, onClick: () => { onDisconnect(); onClose(); } },
  ];

  return createPortal(
    <div
      ref={popoverRef}
      className="user-menu-popover"
      role="menu"
      style={{ position: "fixed", left: pos.left, bottom: pos.bottom, zIndex: 9999 }}
    >
      {/* Main list */}
      <div className="user-menu-main">
        {items.map((item) => {
          const Icon = item.icon;
          const showSub = item.hasSub && subMenu === item.key;
          return (
            <div key={item.key}>
              <button
                type="button"
                role="menuitem"
                onClick={() => item.hasSub ? setSubMenu(subMenu === item.key ? null : item.key) : item.onClick?.()}
                className={clsx("user-menu-row", item.danger && "is-danger", showSub && "is-open")}
              >
                <Icon className={`icon-md ${clsx("shrink-0", item.danger ? "text-red-400" : "text-text-secondary")}`} />
                <span className="flex-1 text-left truncate">{item.label}</span>
                {item.hasSub ? (
                  <>
                    <span className="text-[11px] text-text-faint mr-1">{!showSub ? currentLabel(item.key) : ""}</span>
                    <ChevronRight className={`icon-md ${clsx("shrink-0 text-text-faint transition-transform duration-base", showSub && "rotate-90")}`} />
                  </>
                ) : null}
              </button>
              {showSub && (item.key === "theme" ? themeSub : item.key === "scale" ? scaleSub : langSub)}
            </div>
          );
        })}
      </div>
    </div>,
    document.body,
  );
}
