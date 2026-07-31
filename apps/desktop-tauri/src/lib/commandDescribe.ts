// Decodes a `terminal` / `bash` / `exec` tool command into a human-readable
// Chinese summary with a danger flag. PowerShell `-EncodedCommand` (Base64
// UTF-16LE) is decoded before summarizing; plain commands are shown verbatim
// (truncated) with a danger prefix when applicable.

const PS_ENCODED_RE =
  /(?:pwsh|powershell)\s.+?-Enc(?:odedCommand)?\s+([A-Za-z0-9+/=]+)/i;

// Returns the decoded UTF-16LE string from a PowerShell `-EncodedCommand`,
// or null when the command is not encoded or the Base64 is invalid.
export function decodePowerShellEncoded(cmd: string): string | null {
  const m = cmd.match(PS_ENCODED_RE);
  if (!m) return null;
  const b64 = m[1];
  try {
    const clean = b64.replace(/[^A-Za-z0-9+/=]/g, "");
    const buf = atob(clean);
    const bytes = new Uint8Array(buf.length);
    for (let i = 0; i < buf.length; i++) bytes[i] = buf.charCodeAt(i);
    // PowerShell EncodedCommand 一律 UTF-16LE
    const text = new TextDecoder("utf-16le").decode(bytes);
    return text.trim();
  } catch {
    return null;
  }
}

// Pattern-matches a decoded command line and returns a Chinese summary
// plus a `danger` flag (delete / network download / registry / process kill).
function summarizeReadableCommand(decoded: string): {
  summary: string;
  danger: boolean;
} {
  const c = decoded.toLowerCase();

  if (/\b(remove-item|rm|del|rd|rmdir|remove-childitem)\b/.test(c) ||
      /remove-item\s+-recurse/.test(c)) {
    return { summary: "⚠ 删除文件/目录（可能递归）", danger: true };
  }
  if (/\b(invoke-webrequest|iwr|wget|curl|downloadfile|downloadstring)\b/.test(c) ||
      /system\.net\.webclient/.test(c)) {
    return { summary: "⚠ 从网络下载文件/内容", danger: true };
  }
  if (/\b(set-itemproperty|new-itemproperty|remove-itemproperty|reg\s+add)\b/.test(c)) {
    return { summary: "⚠ 修改注册表", danger: true };
  }
  if (/\b(stop-process|start-process|taskkill)\b/.test(c)) {
    return { summary: "启动/终止进程", danger: true };
  }
  if (/\b(get-wmiobject|get-ciminstance|get-computerinfo|get-process|get-service|systeminfo|ver\b)\b/.test(c)) {
    return { summary: "查询系统信息（名称/版本/架构/进程/服务）", danger: false };
  }
  if (/\b(get-content|set-content|get-childitem|copy-item|move-item|out-file)\b/.test(c)) {
    return { summary: "读取/写入/复制文件", danger: false };
  }
  if (/\b(get-item|set-item|env:)\b/.test(c) || /\[environment\]::/.test(c)) {
    return { summary: "读取/设置环境变量", danger: false };
  }
  const firstLine = decoded
    .split(/\r?\n/)
    .map((l) => l.trim())
    .find((l) => l && !l.startsWith("#") && !l.startsWith("//")) ?? decoded;
  const trimmed = firstLine.length > 80 ? firstLine.slice(0, 77) + "…" : firstLine;
  return { summary: `命令内容: ${trimmed}`, danger: false };
}

export interface CommandDescription {
  text: string;
  danger: boolean;
  wasEncoded: boolean;
}

// Single entry point used by approval cards. Decodes Base64 commands
// (returning a Chinese summary), or shows the raw text (truncated to
// 120 chars) with a danger prefix when the pattern matcher flags it.
export function describeShellCommand(raw: string): CommandDescription {
  const encoded = decodePowerShellEncoded(raw);
  if (encoded) {
    const { summary, danger } = summarizeReadableCommand(encoded);
    return { text: summary, danger, wasEncoded: true };
  }
  const display = raw.trim().length > 120 ? raw.trim().slice(0, 117) + "…" : raw.trim();
  const { danger } = summarizeReadableCommand(raw);
  const prefix = danger ? "⚠ 执行危险命令: " : "执行命令: ";
  return { text: prefix + display, danger, wasEncoded: false };
}
