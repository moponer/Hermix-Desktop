import { RotateCcw, Sun, Moon, Monitor, Check, Palette } from "lucide-react";

import { useTheme, SYSTEM_THEME } from "../../hooks/useTheme";
import { THEMES } from "../../styles/themes/registry";
import { ACCENT_PALETTE, accentLabel } from "../../styles/themes/accentPalette";
import { SettingsGroup, SettingsRow, Switch, IconTile } from "../ui";
import { ChoiceCard } from "./ChoiceCard";
import { cx } from "../../lib/cx";
import { ThemeToggle } from "../ThemeToggle";

/**
 * 外观页：明暗三选 + 强调色调色板。
 *
 * 对比度滑杆（L3.5 后半）刻意不在这里：`--color-text-*` 在 tokens.css 的
 * `@theme` 里，改名会断掉 Tailwind 工具类生成；不改名就只有"在根元素挂
 * filter"一条路，而 filter 会创建包含块、打断所有 fixed 定位的弹窗与
 * toast。两条路都会伤到别处，等有人能一次改对再补。
 */
export function AppearanceTab() {
  const { mode, resolved, setMode, accent, setAccent } = useTheme();

  return (
    <div className="space-y-7">
      {/* 明暗模式 */}
      <section className="space-y-2">
        <h3 className="px-1 text-[length:var(--text-ui)] font-medium text-[var(--color-text-secondary)]">
          明暗模式
        </h3>
        <div className="grid grid-cols-3 gap-3" role="radiogroup" aria-label="明暗模式">
          <ChoiceCard
            label="浅色"
            description="白底深字。白天与明亮环境下对比更舒适。"
            icon={<Sun className="icon-lg" />}
            tone="amber"
            selected={mode === "light"}
            onSelect={() => setMode("light")}
          />
          <ChoiceCard
            label="深色"
            description="深底亮字。夜间与长时间阅读时更省眼。"
            icon={<Moon className="icon-lg" />}
            tone="violet"
            selected={mode === "dark"}
            onSelect={() => setMode("dark")}
          />
          <ChoiceCard
            label="跟随系统"
            description="自动跟随操作系统的深浅色设置，切换时实时跟随。"
            icon={<Monitor className="icon-lg" />}
            tone="slate"
            selected={mode === SYSTEM_THEME}
            onSelect={() => setMode(SYSTEM_THEME)}
            meta={`当前：${THEMES.find((t) => t.id === resolved)?.label ?? resolved}`}
          />
        </div>
      </section>

      {/* 强调色 */}
      <SettingsGroup
        title="强调色"
        description="影响选中态、主按钮与链接。派生色（悬停/浅底）自动跟随。"
      >
        <SettingsRow
          icon={<IconTile icon={<Palette className="icon-md" />} tone="violet" />}
          title="主题默认色"
          description="回到当前主题自带的强调色（不注入任何覆盖）。"
          control={
            <Switch
              checked={accent === null}
              onChange={(v) => setAccent(v ? null : ACCENT_PALETTE[1]?.value ?? null)}
              label="使用主题默认强调色"
            />
          }
        />
        {accent !== null && (
          <>
            <div className="px-4 py-3">
              <div className="mb-2 flex items-center gap-2 text-[length:var(--text-meta)] text-[var(--color-text-muted)]">
                <span
                  aria-hidden
                  className="inline-block h-4 w-4 rounded-full"
                  /* design-guard: allow — `accent` 是用户当场选中的颜色值（数据），
                   不是主题令牌，无法类化。 */
                style={{ background: accent }}
                />
                <span className="font-mono">{accentLabel(accent)}</span>
                <button
                  onClick={() => setAccent(null)}
                  className="ml-auto flex items-center gap-1 rounded-md px-1.5 py-0.5 text-[length:var(--text-meta)] text-[var(--color-text-secondary)] transition-colors hover:bg-[var(--color-bg-hover)] hover:text-[var(--color-text-primary)]"
                >
                  <RotateCcw className="icon-sm" /> 重置
                </button>
              </div>
              <div className="flex flex-wrap gap-2">
                {ACCENT_PALETTE.filter((c) => c.value !== null).map((c) => (
                  <button
                    key={c.label}
                    onClick={() => setAccent(c.value)}
                    title={c.label}
                    aria-label={c.label}
                    aria-pressed={c.value === accent}
                    className={cx(
                      "flex h-7 items-center gap-1.5 rounded-[var(--radius-md)] bg-[var(--color-bg-tertiary)] px-2 text-[length:var(--text-meta)] transition-colors",
                      c.value === accent
                        ? "bg-[var(--color-accent-soft-strong)] text-[var(--color-text-primary)]"
                        : "text-[var(--color-text-muted)] hover:bg-[var(--color-bg-quaternary)] hover:text-[var(--color-text-primary)]",
                    )}
                  >
                    <span
                      aria-hidden
                      className="h-3.5 w-3.5 rounded-full"
                      style={{ background: c.swatch }}
                    />
                    {c.label}
                    {c.value === accent && <Check className="icon-sm" />}
                  </button>
                ))}
              </div>
            </div>
          </>
        )}
      </SettingsGroup>

      {/* 自定义主题 */}
      <SettingsGroup
        title="主题"
        description="内置浅色 / 深色两套；把 <id>.css + <id>.json 放进 themes/ 即自动出现在下拉里。"
      >
        <SettingsRow
          title="当前主题"
          control={<ThemeToggle />}
        />
      </SettingsGroup>
    </div>
  );
}
