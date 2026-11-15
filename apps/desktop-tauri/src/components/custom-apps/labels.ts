// 自定义应用类别与权限标识的本地化映射。
// 后端不强制 enum (manifest.category / permissions 都是 string[])——LLM 自由
// 填的常见值在此兜底, 让 UI 永远显示中文。原始英文 key 仍可在 tooltip/title
// 查看, 便于排错。

export const CATEGORY_LABELS: Record<string, string> = {
  // 规范值
  "file-tools": "文件工具",
  "media": "媒体",
  "productivity": "效率工具",
  "developer": "开发工具",
  "other": "其他",
  // LLM 自由填的常见兜底
  "audio": "音乐",
  "music": "音乐",
  "video": "视频",
  "image": "图片",
  "photo": "照片",
  "document": "文档",
  "file": "文件",
  "notes": "笔记",
  "game": "游戏",
  "tools": "工具",
  "utilities": "实用工具",
  "chat": "聊天",
  "ai": "AI 助手",
  "browser": "浏览器",
  "network": "网络",
  "system": "系统",
  "education": "教育",
  "finance": "财务",
  "shopping": "购物",
  "social": "社交",
};

export function formatCategory(c: string): string {
  return CATEGORY_LABELS[c] ?? c;
}

export const PERM_LABELS: Record<string, string> = {
  "fs-read": "读取文件",
  "fs-write": "写入文件",
  "exec": "执行命令",
  "process": "启动进程",
  "network": "网络请求",
  "dialog": "文件对话框",
  "app": "应用自控",
  "shell": "执行 shell",
  "clipboard-read": "读取剪贴板",
  "clipboard-write": "写入剪贴板",
};

export function formatPermission(p: string): string {
  return PERM_LABELS[p] ?? p;
}

// 权限 hover 说明: 解释该项权限的实际能力. 用于 tooltip 显示更详细的能力说明.
export const PERM_HINTS: Record<string, string> = {
  "fs-read": "读取本地文件系统的文件",
  "fs-write": "写入或修改本地文件",
  "exec": "执行外部命令或进程",
  "process": "启动外部进程",
  "network": "发起网络请求",
  "dialog": "弹出文件选择对话框",
  "app": "应用自身控制权",
  "shell": "执行 shell 脚本",
  "clipboard-read": "读取剪贴板内容",
  "clipboard-write": "写入剪贴板",
};

export function permHint(p: string): string {
  return PERM_HINTS[p] ?? `应用请求的权限: ${p}`;
}