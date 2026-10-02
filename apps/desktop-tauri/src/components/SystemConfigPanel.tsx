import { SettingsShell, resolvePage, type SettingsPage } from "./settings/SettingsShell";

/**
 * 兼容入口：L2 之后真正的实现在 `SettingsShell`（左导航 + 分组 + 搜索）。
 *
 * 保留这个文件是因为 `App.tsx` / `ChatView` / `Composer` 都从这里 import
 * `ConfigCategory` 类型与 `SystemConfigPanel` 组件；直接改所有调用点收益
 * 为零、回归面却铺满整个应用。薄转发即可。
 */
export type ConfigCategory = SettingsPage;

interface SystemConfigPanelProps {
  onClose: () => void;
  /** 打开时落在哪一页。`App.tsx` 的 openSettings 缺省传 "model"——
   *  保持原行为：本应用的配置入口绝大多数是"没配模型/想换模型"。 */
  initialCategory?: ConfigCategory;
}

export function SystemConfigPanel({ onClose, initialCategory }: SystemConfigPanelProps) {
  return (
    <SettingsShell
      onClose={onClose}
      initialPage={initialCategory ? resolvePage(initialCategory) : undefined}
    />
  );
}
