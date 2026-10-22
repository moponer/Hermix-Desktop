// PluginsTab.tsx —— 内置能力开关 Tab + PluginCard（R2-4 拆分第六步）。
import { useCallback, useEffect, useState } from "react";
import { ArrowUpRight } from "lucide-react";
import * as api from "../../api";
import type { PluginSettings } from "../../api";
import { BUILTIN_PLUGINS, DEFAULT_PLUGIN_SETTINGS } from "./toolTabsData";
import { ToolCardIcon } from "./ToolCardIcon";
import { MgmtTag } from "./MgmtTag";
import { ToggleSwitch } from "../ToggleSwitch";
import { SearchBar, Section, Grid, SummaryBar } from "./toolTabsShared";

// ── Plugins tab: built-in toggles + discovered manifests ────────────

export function PluginsTab({ query, onQuery }: { query: string; onQuery: (q: string) => void }) {
  const [settings, setSettings] = useState<PluginSettings>(DEFAULT_PLUGIN_SETTINGS);
  const [loading, setLoading] = useState(true);
  const [savingId, setSavingId] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    setLoading(true);
    try {
      setSettings(await api.getPluginSettings());
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const togglePlugin = useCallback(
    async (plugin: (typeof BUILTIN_PLUGINS)[number], enabled: boolean) => {
      setSavingId(plugin.id);
      try {
        const next = { ...settings, [plugin.key]: enabled };
        setSettings(next);
        const saved = await api.updatePluginSettings(next);
        setSettings(saved);
      } finally {
        setSavingId(null);
      }
    },
    [settings],
  );

  const q = query.trim().toLowerCase();
  const filteredBuiltins = q
    ? BUILTIN_PLUGINS.filter((p) => `${p.name} ${p.description} ${p.category}`.toLowerCase().includes(q))
    : BUILTIN_PLUGINS;

  return (
    <>
      <SearchBar query={query} onQuery={onQuery} />

      {!loading && BUILTIN_PLUGINS.length > 0 && (
        <SummaryBar
          total={BUILTIN_PLUGINS.length}
          enabled={BUILTIN_PLUGINS.filter((p) => settings[p.key]).length}
          enabledLabel="已启用"
          totalLabel="个插件"
        />
      )}

      <Section title="内置工具" hint="由后端持久化保存于 dashboard_plugins.json">
        <Grid empty={!loading && filteredBuiltins.length === 0} emptyText="没有匹配的内置工具。">
          {filteredBuiltins.map((plugin) => {
            const enabled = Boolean(settings[plugin.key]);
            return (
              <PluginCard
                key={plugin.id}
                icon={<plugin.icon className="icon-lg" />}
                name={plugin.name}
                description={plugin.description}
                badge={plugin.category}
                category={plugin.category}
                enabled={enabled}
                saving={savingId === plugin.id}
                disabled={loading}
                rightIcon={<ArrowUpRight className="icon-sm" />}
                onToggle={() => togglePlugin(plugin, !enabled)}
              />
            );
          })}
        </Grid>
      </Section>

    </>
  );
}


// ── Shared building blocks ──────────────────────────────────────────

export function PluginCard({
  icon,
  name,
  description,
  badge,
  category,
  enabled,
  saving,
  disabled,
  highlight = false,
  /** 右侧装饰图标（开关按钮之前）。默认无。 */
  rightIcon,
  onToggle,
}: {
  icon: React.ReactNode;
  name: string;
  description: string;
  badge: string;
  /** 类目名，用于推导图标色调（见 lib/categoryTone）。 */
  category?: string | null;
  enabled: boolean;
  saving: boolean;
  disabled: boolean;
  highlight?: boolean;
  rightIcon?: React.ReactNode;
  onToggle: () => void;
}) {
  // 无描边：卡片靠 bg-bg-card 浮出（浅色主题下 bg-app 才是白的，卡片比底暗一档，
  // 夜墨则是 #1d1f22 压在 #171717 上）。描边在这种底色上只会画出一排灰线。
  // 状态不再叠「已启用/已停用」文字——开关本身已是状态，重复只会更吵；
  // 完整说明留在 ToggleSwitch 的 title/aria-label 里。
  //
  // 此前这里有一个 `noBorder` 分支走 `bg-bg-card/40`，全项目只有插件页一处调用。
  // 那是把**背景调淡**而不是去描边：`#f6f6f6` 按 40% 压在白底上只剩 `#fbfbfb`，
  // 卡片等于不存在——令牌怎么调都救不回来，也就是为什么唯独插件页调色阶毫无
  // 反应。已删除该 prop，插件卡片与其余所有卡片走同一条实色分支。
  return (
    <div
      className={`h-enter group flex items-center gap-3 px-3 py-2.5 rounded-xl m-lift ${
        highlight ? "bg-bg-card-hover" : "bg-bg-card hover:bg-bg-card-hover"
      }`}
    >
      <ToolCardIcon category={category}>
        {icon}
      </ToolCardIcon>
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-2 flex-wrap">
          <span className="text-[length:var(--text-ui)] font-medium text-text-primary truncate">{name}</span>
          <MgmtTag tone="neutral" className="shrink-0">{badge}</MgmtTag>
        </div>
        <p className="text-[length:var(--text-meta)] text-text-muted mt-0.5 line-clamp-2 group-hover:line-clamp-none">{description}</p>
      </div>
      {rightIcon && <span className="shrink-0 text-text-faint">{rightIcon}</span>}
      <ToggleSwitch enabled={enabled} onToggle={onToggle} disabled={saving || disabled} offLabel={`停用 ${name}`} onLabel={`启用 ${name}`} className="shrink-0" />
    </div>
  );
}
