import { Sun, Moon, Monitor } from "lucide-react";
import { useTheme, type ThemeMode } from "../hooks/useTheme";

interface ThemeToggleProps {
  compact?: boolean;
}

const ORDER: ThemeMode[] = ["light", "dark", "system"];
const LABELS: Record<ThemeMode, string> = { light: "浅色", dark: "深色", system: "跟随系统" };

export function ThemeToggle({ compact = false }: ThemeToggleProps) {
  const { mode, cycle } = useTheme();
  const Icon = mode === "light" ? Sun : mode === "dark" ? Moon : Monitor;
  const title = `主题：${LABELS[mode]}（点击切换）`;

  if (compact) {
    return (
      <button
        onClick={cycle}
        title={title}
        className="h-10 w-10 flex items-center justify-center text-text-muted hover:bg-bg-hover hover:text-text-secondary transition-colors"
      >
        <Icon size={15} />
      </button>
    );
  }

  return (
    <div className="flex items-center gap-0.5 p-0.5 rounded-lg bg-bg-tertiary border border-border-primary">
      {ORDER.map((m) => {
        const MIcon = m === "light" ? Sun : m === "dark" ? Moon : Monitor;
        const active = mode === m;
        return (
          <button
            key={m}
            onClick={() => cycle()}
            title={LABELS[m]}
            className={`flex items-center justify-center w-7 h-7 rounded-md transition-colors ${
              active
                ? "bg-bg-elevated text-accent shadow-sm"
                : "text-text-muted hover:text-text-primary"
            }`}
          >
            <MIcon size={13} />
          </button>
        );
      })}
    </div>
  );
}
