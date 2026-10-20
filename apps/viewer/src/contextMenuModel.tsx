// 右键菜单的**内容模型**：按右键落点（目标元素）决定摆什么菜。
//
// 分工：
//   - ContextMenu.tsx 负责"怎么摆、怎么关、怎么用键盘走"；
//   - 本文件负责"给这个目标摆什么"——分组、禁用态、快捷键提示。
//
// 原则：菜单里不放占位项。每个项要么有真实实现，要么按**真实状态**
// 禁用（没装 VS Code 就不给"用 VS Code 打开"留一个能点的假按钮）。
// 快捷键提示只写**真的能按**的键——菜单里写着 Ctrl+Shift+C，就一定
// 有全局监听在处理它。

import {
  ArrowUp,
  ClipboardCopy,
  ClipboardPaste,
  Code,
  Copy,
  ExternalLink,
  FileText,
  FolderOpen,
  FolderSearch,
  Info,
  PenLine,
  RefreshCw,
  Sparkles,
  Star,
  Sun,
  Moon,
  Monitor,
  ArrowUpDown,
  TextSelect,
} from "lucide-react";
import type { MenuItem } from "./ContextMenu";
import type { ThemeMode } from "./theme";

/** 列表排序方式（右键菜单可切换，勾选当前项）。 */
export type SortKind = "name" | "size" | "mtime";

/** 视图模式（与 App.tsx 的 ViewerMode 同形）。 */
export type MenuMode = "pure" | "heavy";

/**
 * 右键落点。用 `data-ctx` 系列属性标在 DOM 元素上，App 的全局拦截
 * 靠 closest("[data-ctx]") 找到落点——不需要给每个组件挂 onContextMenu。
 */
export type MenuTarget =
  | {
      kind: "entry";
      path: string;
      name: string;
      isDir: boolean;
      from: "list" | "tree" | "bookmark";
    }
  | { kind: "explorer" }
  | { kind: "tab"; id: number; path: string; name: string }
  | { kind: "preview"; path: string; name: string }
  | { kind: "pathbar" }
  | { kind: "field" }
  | { kind: "app" };

/** 菜单要用到的状态与动作。 */
export interface MenuContext {
  currentPath: string | null;
  activeFile: { path: string; name: string } | null;
  sortBy: SortKind;
  showHidden: boolean;
  mode: MenuMode;
  themeMode: ThemeMode;
  /** 可用的"打开方式"（Rust 侧探测：notepad 一定有，code 看 PATH）。 */
  openApps: string[];
  isBookmarked: (path: string) => boolean;
  tabCount: number;
  /** 预览区当前选中的文本（空 = 没选）。 */
  selection: string;
  /** 输入框里选中的文字（空 = 没选——「复制选区」据此置灰）。 */
  fieldSelection: string;

  openEntry: (path: string, isDir: boolean) => void;
  openWith: (path: string, app: string) => void;
  showProperties: (path: string) => void;
  reveal: (path: string) => void;
  copyPath: (path: string) => void;
  copyFiles: (paths: string[]) => void;
  pasteFiles: (dir: string) => void;
  copyText: (text: string) => void;
  pasteText: () => void;
  /** 输入框的文本编辑动作（粘贴 / 复制选区 / 全选）。 */
  fieldAction: (action: "paste" | "copy" | "selectAll") => void;
  toggleBookmark: (path: string, name: string, isDir: boolean) => void;
  addToAi: (path: string, name: string) => void;
  refresh: () => void;
  goUp: () => void;
  closeTab: (id?: number) => void;
  closeOtherTabs: (id: number) => void;
  closeAllTabs: () => void;
  setSort: (kind: SortKind) => void;
  setShowHidden: (visible: boolean) => void;
  setTheme: (mode: ThemeMode) => void;
  setMode: (mode: MenuMode) => void;
}

const THEME_ITEMS: Array<{ mode: ThemeMode; label: string; icon: typeof Sun }> = [
  { mode: "light", label: "浅色主题", icon: Sun },
  { mode: "dark", label: "深色主题", icon: Moon },
  { mode: "system", label: "跟随系统", icon: Monitor },
];

/** 打开方式项：Rust 侧探测不到就置灰（而不是藏起来——藏了用户不知道有这回事）。 */
function openWithItems(path: string, ctx: MenuContext): MenuItem[] {
  const items: MenuItem[] = [];
  const hasNotepad = ctx.openApps.includes("notepad");
  const hasCode = ctx.openApps.includes("code");
  items.push({
    id: "open-notepad",
    label: "用记事本打开",
    icon: <PenLine size={15} />,
    disabled: !hasNotepad,
    onSelect: () => ctx.openWith(path, "notepad"),
  });
  items.push({
    id: "open-code",
    label: "用 VS Code 打开",
    icon: <Code size={15} />,
    disabled: !hasCode,
    onSelect: () => ctx.openWith(path, "code"),
  });
  return items;
}

/** 文件 / 文件夹 / 树节点 / 书签——落点不同，分组微调，主体一致。 */
function entryMenu(
  t: Extract<MenuTarget, { kind: "entry" }>,
  ctx: MenuContext,
): MenuItem[][] {
  const groups: MenuItem[][] = [];

  const open: MenuItem[] = [];
  if (t.isDir) {
    open.push({
      id: "open",
      label: "打开文件夹",
      icon: <FolderOpen size={15} />,
      onSelect: () => ctx.openEntry(t.path, true),
    });
  } else {
    open.push({
      id: "open",
      label: "打开",
      icon: <FileText size={15} />,
      onSelect: () => ctx.openEntry(t.path, false),
    });
    open.push({
      id: "open-default",
      label: "用系统默认程序打开",
      icon: <ExternalLink size={15} />,
      onSelect: () => ctx.openWith(t.path, "default"),
    });
  }
  groups.push(open);

  const locate: MenuItem[] = [
    {
      id: "reveal",
      label: "在资源管理器中显示",
      icon: <FolderSearch size={15} />,
      onSelect: () => ctx.reveal(t.path),
    },
    {
      id: "copy-path",
      label: "复制完整路径",
      icon: <Copy size={15} />,
      hint: "Ctrl+Shift+C",
      onSelect: () => ctx.copyPath(t.path),
    },
    {
      id: "copy-file",
      label: t.isDir ? "复制文件夹" : "复制文件",
      icon: <ClipboardCopy size={15} />,
      onSelect: () => ctx.copyFiles([t.path]),
    },
  ];
  groups.push(locate);

  // 只有文件才有"打开方式"；目录给的是"用资源管理器打开"。
  if (!t.isDir) {
    groups.push(openWithItems(t.path, ctx));
  } else if (t.from === "tree") {
    groups.push([
      {
        id: "refresh",
        label: "刷新此节点",
        icon: <RefreshCw size={15} />,
        onSelect: () => ctx.refresh(),
      },
    ]);
  }

  const extras: MenuItem[] = [];
  if (!t.isDir) {
    extras.push({
      id: "ai",
      label: "加入 AI 对话",
      icon: <Sparkles size={15} />,
      onSelect: () => ctx.addToAi(t.path, t.name),
    });
  }
  const bookmarked = ctx.isBookmarked(t.path);
  extras.push({
    id: "bookmark",
    label: bookmarked ? "移除书签" : "添加书签",
    icon: <Star size={15} />,
    onSelect: () => ctx.toggleBookmark(t.path, t.name, t.isDir),
  });
  groups.push(extras);

  groups.push([
    {
      id: "properties",
      label: "属性",
      icon: <Info size={15} />,
      onSelect: () => ctx.showProperties(t.path),
    },
  ]);

  return groups;
}

/** 文件列表空白处：刷新 / 排序 / 隐藏项 / 粘贴。 */
function explorerMenu(ctx: MenuContext): MenuItem[][] {
  return [
    [
      {
        id: "refresh",
        label: "刷新",
        icon: <RefreshCw size={15} />,
        hint: "F5",
        onSelect: () => ctx.refresh(),
      },
      {
        id: "up",
        label: "上一级",
        icon: <ArrowUp size={15} />,
        hint: "Alt+↑",
        disabled: !ctx.currentPath,
        onSelect: () => ctx.goUp(),
      },
    ],
    [
      {
        id: "sort",
        label: "按名称排序",
        icon: <FileText size={15} />,
        checked: ctx.sortBy === "name",
        onSelect: () => ctx.setSort("name"),
      },
      {
        id: "sort-size",
        label: "按大小排序",
        icon: <ArrowUpDown size={15} />,
        checked: ctx.sortBy === "size",
        onSelect: () => ctx.setSort("size"),
      },
      {
        id: "sort-mtime",
        label: "按修改时间排序",
        icon: <ArrowUpDown size={15} />,
        checked: ctx.sortBy === "mtime",
        onSelect: () => ctx.setSort("mtime"),
      },
    ],
    [
      {
        id: "hidden",
        label: ctx.showHidden ? "隐藏隐藏文件" : "显示隐藏文件",
        icon: <FolderOpen size={15} />,
        checked: ctx.showHidden,
        onSelect: () => ctx.setShowHidden(!ctx.showHidden),
      },
    ],
    [
      {
        id: "paste",
        label: "粘贴文件",
        icon: <ClipboardPaste size={15} />,
        hint: "Ctrl+V",
        disabled: !ctx.currentPath,
        onSelect: () => ctx.currentPath && ctx.pasteFiles(ctx.currentPath),
      },
    ],
  ];
}

/** 预览标签：关闭 / 定位 / 打开。 */
function tabMenu(t: Extract<MenuTarget, { kind: "tab" }>, ctx: MenuContext): MenuItem[][] {
  return [
    [
      {
        id: "close",
        label: "关闭标签",
        icon: <FileText size={15} />,
        hint: "Ctrl+W",
        onSelect: () => ctx.closeTab(t.id),
      },
      {
        id: "close-others",
        label: "关闭其他标签",
        icon: <FileText size={15} />,
        disabled: ctx.tabCount <= 1,
        onSelect: () => ctx.closeOtherTabs(t.id),
      },
      {
        id: "close-all",
        label: "关闭全部标签",
        icon: <FileText size={15} />,
        disabled: ctx.tabCount === 0,
        onSelect: () => ctx.closeAllTabs(),
      },
    ],
    [
      {
        id: "copy-path",
        label: "复制完整路径",
        icon: <Copy size={15} />,
        hint: "Ctrl+Shift+C",
        onSelect: () => ctx.copyPath(t.path),
      },
      {
        id: "reveal",
        label: "在资源管理器中显示",
        icon: <FolderSearch size={15} />,
        onSelect: () => ctx.reveal(t.path),
      },
    ],
    [
      {
        id: "open-default",
        label: "用系统默认程序打开",
        icon: <ExternalLink size={15} />,
        onSelect: () => ctx.openWith(t.path, "default"),
      },
    ],
  ];
}

/** 预览区：定位 / 打开 / 复制选中文本 / 交给 AI。 */
function previewMenu(
  t: Extract<MenuTarget, { kind: "preview" }>,
  ctx: MenuContext,
): MenuItem[][] {
  return [
    [
      {
        id: "copy-path",
        label: "复制完整路径",
        icon: <Copy size={15} />,
        hint: "Ctrl+Shift+C",
        onSelect: () => ctx.copyPath(t.path),
      },
      {
        id: "reveal",
        label: "在资源管理器中显示",
        icon: <FolderSearch size={15} />,
        onSelect: () => ctx.reveal(t.path),
      },
    ],
    [
      {
        id: "open-default",
        label: "用系统默认程序打开",
        icon: <ExternalLink size={15} />,
        onSelect: () => ctx.openWith(t.path, "default"),
      },
      {
        id: "ai",
        label: "加入 AI 对话",
        icon: <Sparkles size={15} />,
        onSelect: () => ctx.addToAi(t.path, t.name),
      },
    ],
    [
      {
        id: "copy-selection",
        label: "复制选中文本",
        icon: <ClipboardCopy size={15} />,
        disabled: !ctx.selection,
        onSelect: () => ctx.copyText(ctx.selection),
      },
    ],
  ];
}

/** 路径栏输入框：文本编辑（复制选区 / 全选）+ 粘贴 + 复制当前路径。 */
function pathbarMenu(ctx: MenuContext): MenuItem[][] {
  return [
    [
      {
        id: "paste",
        label: "粘贴",
        icon: <ClipboardPaste size={15} />,
        hint: "Ctrl+V",
        onSelect: () => ctx.pasteText(),
      },
    ],
    [
      {
        id: "field-copy",
        label: "复制选区",
        icon: <Copy size={15} />,
        hint: "Ctrl+C",
        disabled: !ctx.fieldSelection,
        onSelect: () => ctx.fieldAction("copy"),
      },
      {
        id: "field-select",
        label: "全选",
        icon: <TextSelect size={15} />,
        hint: "Ctrl+A",
        onSelect: () => ctx.fieldAction("selectAll"),
      },
    ],
    [
      {
        id: "copy-path",
        label: "复制当前路径",
        icon: <ClipboardCopy size={15} />,
        hint: "Ctrl+Shift+C",
        disabled: !ctx.currentPath,
        onSelect: () => ctx.currentPath && ctx.copyPath(ctx.currentPath),
      },
    ],
  ];
}

/** 输入框：文本编辑三件套（拦掉原生右键后，右键必须还能粘贴）。 */
function fieldMenu(ctx: MenuContext): MenuItem[][] {
  return [
    [
      {
        id: "field-paste",
        label: "粘贴",
        icon: <ClipboardPaste size={15} />,
        hint: "Ctrl+V",
        onSelect: () => ctx.fieldAction("paste"),
      },
    ],
    [
      {
        id: "field-copy",
        label: "复制选区",
        icon: <Copy size={15} />,
        hint: "Ctrl+C",
        disabled: !ctx.fieldSelection,
        onSelect: () => ctx.fieldAction("copy"),
      },
      {
        id: "field-select",
        label: "全选",
        icon: <TextSelect size={15} />,
        hint: "Ctrl+A",
        onSelect: () => ctx.fieldAction("selectAll"),
      },
    ],
  ];
}

/** 兜底（工具条 / 侧栏空白等）：刷新 / 主题 / 视图模式。 */
function appMenu(ctx: MenuContext): MenuItem[][] {
  return [
    [
      {
        id: "refresh",
        label: "刷新",
        icon: <RefreshCw size={15} />,
        hint: "F5",
        onSelect: () => ctx.refresh(),
      },
    ],
    [
      {
        id: "copy-path",
        label: "复制当前路径",
        icon: <Copy size={15} />,
        hint: "Ctrl+Shift+C",
        disabled: !ctx.currentPath,
        onSelect: () => ctx.currentPath && ctx.copyPath(ctx.currentPath),
      },
    ],
    THEME_ITEMS.map((t) => ({
      id: `theme-${t.mode}`,
      label: t.label,
      icon: <t.icon size={15} />,
      checked: ctx.themeMode === t.mode,
      onSelect: () => ctx.setTheme(t.mode),
    })),
    [
      {
        id: "mode-pure",
        label: "纯净模式（只预览）",
        icon: <FileText size={15} />,
        checked: ctx.mode === "pure",
        onSelect: () => ctx.setMode("pure"),
      },
      {
        id: "mode-heavy",
        label: "重量模式（资源管理器）",
        icon: <FolderOpen size={15} />,
        checked: ctx.mode === "heavy",
        onSelect: () => ctx.setMode("heavy"),
      },
    ],
  ];
}

/** 按落点生成菜单分组。 */
export function buildMenu(target: MenuTarget, ctx: MenuContext): MenuItem[][] {
  switch (target.kind) {
    case "entry":
      return entryMenu(target, ctx);
    case "explorer":
      return explorerMenu(ctx);
    case "tab":
      return tabMenu(target, ctx);
    case "preview":
      return previewMenu(target, ctx);
    case "pathbar":
      return pathbarMenu(ctx);
    case "field":
      return fieldMenu(ctx);
    default:
      return appMenu(ctx);
  }
}
