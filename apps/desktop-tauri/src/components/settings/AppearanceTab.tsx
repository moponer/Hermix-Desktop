import { useState } from "react";
import { RotateCcw, Sun, Moon, Monitor, ImagePlus, Trash2, Loader2, Check } from "lucide-react";

import { useTheme, SYSTEM_THEME } from "../../hooks/useTheme";
import { useWallpaper } from "../../hooks/useWallpaper";
import { THEMES } from "../../styles/themes/registry";
import { ACCENT_PALETTE, accentLabel } from "../../styles/themes/accentPalette";
import { SettingsGroup, SettingsRow, Switch } from "../ui";
import { ChoiceCard } from "./ChoiceCard";
import { cx } from "../../lib/cx";
import { ThemeToggle } from "../ThemeToggle";
import { open } from "@tauri-apps/plugin-dialog";
import { toastError, toastSuccess } from "../../hooks/useToast";

/**
 * 外观页：明暗三选 + 强调色调色板 + 壁纸。
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
        <h3 className="px-1 text-[length:var(--text-ui)] font-medium text-[var(--color-text-primary)]">
          明暗模式
        </h3>
        <div className="grid grid-cols-3 gap-3" role="radiogroup" aria-label="明暗模式">
          <ChoiceCard
            label="浅色"
            description="白底深字。白天与明亮环境下对比更舒适。"
            icon={<Sun className="icon-lg" />}
            tone="warning"
            selected={mode === "light"}
            onSelect={() => setMode("light")}
          />
          <ChoiceCard
            label="深色"
            description="深底亮字。夜间与长时间阅读时更省眼。"
            icon={<Moon className="icon-lg" />}
            tone="info"
            selected={mode === "dark"}
            onSelect={() => setMode("dark")}
          />
          <ChoiceCard
            label="跟随系统"
            description="自动跟随操作系统的深浅色设置，切换时实时跟随。"
            icon={<Monitor className="icon-lg" />}
            tone="success"
            selected={mode === SYSTEM_THEME}
            onSelect={() => setMode(SYSTEM_THEME)}
            meta={`当前：${THEMES.find((t) => t.id === resolved)?.label ?? resolved}`}
          />
        </div>
      </section>

      <WallpaperGroup />

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

/* ── L3.6 壁纸 ────────────────────────────────────────────────────
   缩略图直接用 `useWallpaper().url`（就是铺在聊天区那张图的 asset URL），
   不另存一份预览：两处各存一份就一定会漂移——用户换图后设置页还显示旧图，
   是这类"选图"功能最常见的投诉。 */
function WallpaperGroup() {
  const { info, url, loading, setFrom, clear } = useWallpaper();
  const [busy, setBusy] = useState(false);

  const pick = async () => {
    setBusy(true);
    try {
      const picked = await open({
        multiple: false,
        directory: false,
        title: "选择壁纸",
        filters: [{ name: "图片", extensions: ["png", "jpg", "jpeg", "webp", "gif", "bmp", "avif"] }],
      });
      const path = typeof picked === "string" ? picked : null;
      if (!path) return;
      await setFrom(path);
      toastSuccess("壁纸已更新");
    } catch (e) {
      toastError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const doClear = async () => {
    setBusy(true);
    try {
      await clear();
      toastSuccess("壁纸已清除");
    } catch (e) {
      toastError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <SettingsGroup
      title="壁纸"
      description="铺在聊天区背景。原图会复制进数据目录，删掉原图不影响显示。"
    >
      <SettingsRow
        title="聊天区壁纸"
        description={
          loading
            ? "读取中…"
            : info
              ? <span className="break-all">{info.source || "（来源未知）"}</span>
              : "未设置壁纸。"
        }
      >
        {url && (
          <div
            className="mt-2 h-24 w-full overflow-hidden rounded-[var(--radius-md)] border border-[var(--color-border-secondary)]"
            style={{ backgroundImage: `url("${url}")`, backgroundSize: "cover", backgroundPosition: "center" }}
          />
        )}
        <div className="mt-2 flex items-center gap-2">
          <button
            onClick={() => void pick()}
            disabled={busy || loading}
            className="flex h-7 items-center gap-1.5 rounded-lg border border-[var(--color-border-secondary)] px-2 text-[length:var(--text-meta)] text-[var(--color-text-secondary)] transition-colors hover:border-[var(--color-border-primary)] hover:text-[var(--color-text-primary)] disabled:opacity-40"
          >
            {busy ? <Loader2 className="icon-sm animate-spin" /> : <ImagePlus className="icon-sm" />}
            {info ? "更换壁纸" : "添加壁纸"}
          </button>
          {info && (
            <button
              onClick={() => void doClear()}
              disabled={busy}
              className="flex h-7 items-center gap-1.5 rounded-lg border border-[var(--color-border-secondary)] px-2 text-[length:var(--text-meta)] text-[var(--color-text-secondary)] transition-colors hover:border-[var(--color-border-primary)] hover:text-[var(--color-text-primary)] disabled:opacity-40"
            >
              <Trash2 className="icon-sm" /> 移除
            </button>
          )}
        </div>
      </SettingsRow>
    </SettingsGroup>
  );
}
