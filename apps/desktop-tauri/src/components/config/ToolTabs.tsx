// ToolTabs.tsx —— 配置域四 Tab 的 barrel（R2-4 拆分完成后仅负责再导出）。
// 实现分布：PluginsTab.tsx / SkillsTab.tsx / ConnectorsTab.tsx / ToolsetsTab.tsx，
// 共享件在 toolTabsShared.tsx，常量与解析器在 toolTabsData.ts。
export { PluginsTab } from "./PluginsTab";
export { SkillsTab } from "./SkillsTab";
export { ConnectorsTab } from "./ConnectorsTab";
export { ToolsetsTab } from "./ToolsetsTab";
