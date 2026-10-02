import { Check, RotateCcw, Sun, Moon, Monitor } from "lucide-react";

import { useTheme, SYSTEM_THEME } from "../../hooks/useTheme";
import { THEMES } from "../../styles/themes/registry";
import { ACCENT_PALETTE, accentLabel } from "../../styles/themes/accentPalette";
import { SettingsGroup, SettingsRow, Switch } from "../ui";
import { cx } from "../../lib/cx";
import { ThemeToggle } from "../ThemeToggle";

/**
 * 外观页：明暗三选 + 强调色调色板。
 *
 * 只放"用户能自己改回去"的东西。壁纸（L3.6）与对比度滑杆（L3.5 后半）
 * 刻意不在这里：前者要新 tauri command，后者要么改 @theme 契约、要么在
 * 根元素上挂 filter 破坏 fixed 定位，两者都属于要做就得做对的架构改动，
 * 不该塞进一个凑数的行里。
 */
export function AppearanceTab() {
  const { mode, resolved, setMode, accent, setAccent } = useTheme();

  return (
    <div className="space-y-7">
      <header>
        <h2 className="text-[length:var(--text-body)] font-medium text-[var(--color-text-primary)]">
          外观
        </h2>
        <p className="mt-1 text-[length:var(--text-meta)] text-[var(--color-text-muted)]">
          主题与强调色立即生效并持久保存。
        </p>
      </header>

      {/* 明暗模式 */}
      <section className="space-y-2">
        <h3 className="px-1 text-[length:var(--text-ui)] font-medium text-[var(--color-text-primary)]">
          明暗模式
        </h3>
        <div className="grid grid-cols-3 gap-3">
          <ModeCard
            label="浅色"
            icon={Sun}
            active={mode === "light"}
            preview="light"
            onClick={() => setMode("light")}
          />
          <ModeCard
            label="深色"
            icon={Moon}
            active={mode === "dark"}
            preview="dark"
            onClick={() => setMode("dark")}
          />
          <ModeCard
            label="跟随系统"
            icon={Monitor}
            active={mode === SYSTEM_THEME}
            preview={resolved === "dark" ? "dark" : "light"}
            hint={`当前：${THEMES.find((t) => t.id === resolved)?.label ?? resolved}`}
            onClick={() => setMode(SYSTEM_THEME)}
          />
        </div>
      </section>

      {/* 强调色 */}
      <SettingsGroup
        title="强调色"
        description="影响选中态、主按钮与链接。派生色（悬停/浅底）自动跟随。"
      >
        <SettingsRow
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
                  style={{ background: accent, border: "1px solid var(--color-border-secondary)" }}
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
                      "flex h-7 items-center gap-1.5 rounded-lg border px-2 text-[length:var(--text-meta)] transition-colors",
                      c.value === accent
                        ? "border-[var(--color-accent)] text-[var(--color-text-primary)]"
                        : "border-[var(--color-border-secondary)] text-[var(--color-text-muted)] hover:border-[var(--color-border-primary)] hover:text-[var(--color-text-primary)]",
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

/* ── 三选卡 ───────────────────────────────────────────────────────
   缩略图是"示意图"而非真实渲染：themes/*.css 按 :root[data-theme] 作用域
   提供 token，没法把另一套主题的内联样式摘出来当预览。这里用固定的
   明/暗配色画出结构示意，只表达"亮底还是暗底"，不谎称是像素级预览。 */
function ModeCard({
  label, icon: Icon, active, preview, hint, onClick,
}: {
  label: string;
  icon: typeof Sun;
  active: boolean;
  preview: "light" | "dark";
  hint?: string;
  onClick: () => void;
}) {
  const dark = preview === "dark";
  return (
    <button
      onClick={onClick}
      aria-pressed={active}
      className={cx(
        "group flex flex-col items-center gap-2 rounded-[var(--radius-lg)] border-2 p-3 transition-colors",
        active
          ? "border-[var(--color-accent)] bg-[var(--color-accent-on-text)]"
          : "border-[var(--color-border-secondary)] hover:border-[var(--color-border-primary)]",
      )}
    >
      <div
        aria-hidden
        className="flex h-14 w-full flex-col justify-between rounded-[var(--radius-md)] p-2"
        style={{ background: dark ? "#171717" : "#ffffff", border: "1px solid var(--color-border-primary)" }}
      >
        <div className="flex gap-1">
          <div className="h-1.5 w-1.5 rounded-full" style={{ background: dark ? "#ededed" : "#242424" }} />
          <div className="h-1.5 w-1.5 rounded-full" style={{ background: dark ? "#5f636b" : "#c8c8c8" }} />
        </div>
        <div className="space-y-1">
          <div className="h-1.5 w-4/5 rounded-full" style={{ background: dark ? "#9a9ea6" : "#575757" }} />
          <div className="h-1.5 w-3/5 rounded-full" style={{ background: dark ? "#5f636b" : "#8c8c8c" }} />
        </div>
      </div>
      <div className="flex items-center gap-1.5">
        <Icon className="icon-sm text-[var(--color-text-secondary)]" />
        <span className="text-[length:var(--text-ui)] text-[var(--color-text-primary)]">{label}</span>
        {active && <Check className="icon-sm text-[var(--color-accent)]" />}
      </div>
      {hint && (
        <span className="text-[length:var(--text-meta)] text-[var(--color-text-muted)]">{hint}</span>
      )}
    </button>
  );
}
