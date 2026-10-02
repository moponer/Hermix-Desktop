import { SettingsShell, resolvePage, type SettingsPage } from "./settings/SettingsShell";

/**
 * 兼容入口：真正实现在 `SettingsShell`（整页，非浮层）。
 *
 * 保留这个文件是因为 `App.tsx` / `ChatView` / `Composer` 都从这里 import
 * `ConfigCategory` 类型与 `SystemConfigPanel` 组件；直接改所有调用点收益
 * 为零、回归面却铺满整个应用。薄转发即可。
 */
export type ConfigCategory = SettingsPage;

interface SystemConfigPanelProps {
  /** 返回上一个页面。整页形态下没有"关闭"，只有"离开"。 */
  onBack: () => void;
  /** 打开时落在哪一页。 */
  initialCategory?: ConfigCategory;
}

export function SystemConfigPanel({ onBack, initialCategory }: SystemConfigPanelProps) {
  return (
    <SettingsShell
      onBack={onBack}
      initialPage={initialCategory ? resolvePage(initialCategory) : undefined}
    />
  );
}
