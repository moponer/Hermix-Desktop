// src/components/ModeSelector.tsx
//
// P2-3: Task 模式选择器 — Craft / Plan / Ask
//
// 三个按钮，用户在创建任务时选择工作模式：
//   · Craft  — 直接执行，不需确认（默认）
//   · Plan   — 先出方案，等用户确认后再执行
//   · Ask    — 只读问答，不能执行写操作
//
// 样式遵守铁律 V：无蓝紫，用 gold/amber + gray/zinc。
// Dark 双轨：通过 CSS 变量自动适配。

import { memo } from "react";
import { clsx } from "clsx";
import { Hammer, ClipboardList, HelpCircle } from "lucide-react";

export type TaskMode = "craft" | "plan" | "ask";

interface ModeSelectorProps {
  value: TaskMode;
  onChange: (mode: TaskMode) => void;
  disabled?: boolean;
  size?: "sm" | "md";
}

const MODE_CONFIG: Record<
  TaskMode,
  {
    label: string;
    icon: typeof Hammer;
    description: string;
    accent: string;
    accentBg: string;
    accentBorder: string;
  }
> = {
  craft: {
    label: "直接执行",
    icon: Hammer,
    description: "Agent 直接执行任务，不需确认",
    accent: "hsl(43, 96%, 48%)",
    accentBg: "hsla(43, 96%, 56%, 0.15)",
    accentBorder: "hsla(43, 96%, 56%, 0.40)",
  },
  plan: {
    label: "先规划",
    icon: ClipboardList,
    description: "Agent 先出方案，确认后再执行",
    accent: "hsl(150, 60%, 45%)",
    accentBg: "hsla(150, 60%, 50%, 0.12)",
    accentBorder: "hsla(150, 60%, 50%, 0.35)",
  },
  ask: {
    label: "仅问答",
    icon: HelpCircle,
    description: "只读模式，不执行写操作",
    accent: "hsl(220, 10%, 50%)",
    accentBg: "hsla(220, 10%, 50%, 0.12)",
    accentBorder: "hsla(220, 10%, 50%, 0.30)",
  },
};

export const ModeSelector = memo(function ModeSelector({
  value,
  onChange,
  disabled = false,
  size = "md",
}: ModeSelectorProps) {
  const isSm = size === "sm";

  return (
    <div className="flex items-center gap-1.5">
      {(Object.keys(MODE_CONFIG) as TaskMode[]).map((mode) => {
        const config = MODE_CONFIG[mode];
        const Icon = config.icon;
        const isActive = value === mode;

        return (
          <button
            key={mode}
            type="button"
            disabled={disabled}
            onClick={() => onChange(mode)}
            title={config.description}
            className={clsx(
              "flex items-center gap-1.5 rounded-lg font-semibold transition-all",
              isSm ? "px-2 py-1 text-[10.5px]" : "px-2.5 py-1.5 text-[11.5px]",
              "border",
              isActive
                ? "cursor-default"
                : "border-border-subtle text-text-muted hover:text-text-secondary hover:bg-bg-tertiary cursor-pointer",
              disabled && "opacity-50 cursor-not-allowed",
            )}
            style={
              isActive
                ? {
                    background: config.accentBg,
                    borderColor: config.accentBorder,
                    color: config.accent,
                  }
                : undefined
            }
          >
            <Icon size={isSm ? 11 : 13} />
            <span>{config.label}</span>
          </button>
        );
      })}
    </div>
  );
});
