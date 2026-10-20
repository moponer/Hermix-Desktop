// 钉住预览（独立浮窗）的前端桥。
//
// 浮窗由 Rust 侧建（它要拿主窗口的 URL、算落位、管上限），前端只负责
// 发起并把失败原因显示出来 —— 静默失败会变成"点了没反应"。

import { invoke } from "@tauri-apps/api/core";

/** 钉住一个路径。返回浮窗 label；失败时返回错误文案。 */
export async function pinPreview(path: string): Promise<{ label: string } | { error: string }> {
  try {
    const label = await invoke<string>("pin_open", { path });
    return { label };
  } catch (e) {
    return { error: typeof e === "string" ? e : String(e) };
  }
}
