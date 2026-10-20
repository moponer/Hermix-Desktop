// Office 编辑器（Word / Excel）的共享小工具。
//
// 只放两个方向的东西：base64 编解码（与 viewer 既有约定一致——二进制过
// Tauri IPC 一律走 base64，裸字节数组会展开成几百万个 JSON 数字）。
// 业务逻辑各归各的编辑器文件，不在这里堆。

/** Uint8Array → base64。分块编码：一次性 spread 大数组会爆调用栈。 */
export function encodeBase64(bytes: Uint8Array): string {
  let bin = "";
  const chunk = 0x8000;
  for (let i = 0; i < bytes.length; i += chunk) {
    bin += String.fromCharCode(...bytes.subarray(i, i + chunk));
  }
  return btoa(bin);
}

/** base64 → Uint8Array。 */
export function decodeBase64(b64: string): Uint8Array {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

/** 产物文件名：`<原名>.edited.<扩展名>`（原文件永不覆盖）。 */
export function editedName(name: string, fallbackExt: string): string {
  const dot = name.lastIndexOf(".");
  const stem = dot > 0 ? name.slice(0, dot) : name;
  const ext = dot > 0 ? name.slice(dot + 1) : fallbackExt;
  return `${stem}.edited.${ext}`;
}
