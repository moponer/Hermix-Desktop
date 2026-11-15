// 文本字节 → 字符串的**唯一**解码规则（desktop / viewer 共用）。
//
// 为什么不能一律按 UTF-8 解：Windows 上记事本、Word「纯文本」导出默认写
// UTF-16 LE（带 BOM），国内老软件导出的 csv/txt 常年是 GBK。这些字节按
// UTF-8 解要么满屏乱码（viewer 之前就是这样），要么被判成二进制
// （desktop 之前走的是严格 from_utf8）。两者都不是给人看的。
//
// 判定顺序（先命中先返回）：
//   1. BOM            → utf-8 / utf-16le / utf-16be
//   2. 无 BOM 的 UTF-16 → 奇偶位 NUL 分布嗅探（ASCII 文本在 LE 下 NUL 全落奇数位）
//   3. 含 NUL 字节     → 二进制
//   4. 严格 UTF-8      → 文本
//   5. 严格 GBK        → 文本
//   6. 宽松 UTF-8      → 兜底，保证永远返回可渲染的字符串
//
// 二进制判定单独靠 NUL 嗅探（第 3 步），不靠"解不出来"——GBK 能解码的
// 字节范围很宽，用它当二进制判据会把二进制文件显示成乱码而不是占位符。
//
// UTF-32 文本文件现实中基本不存在，Chromium 也没有 utf-32 解码器，
// 它的 BOM 直接判成二进制（第 3 步会命中 NUL）。

import type { ReadTextResult } from "./fileSource";

export type DecodedText =
  | { kind: "text"; text: string; encoding: string }
  | { kind: "binary" }
  | { kind: "empty" };

/** 二进制嗅探只看开头这么多字节——真二进制在头几百字节里就有 NUL。 */
const NUL_SNIFF_WINDOW = 4096;

type Bom = { encoding: "utf-8" | "utf-16le" | "utf-16be"; length: number };

function sniffBom(bytes: Uint8Array): Bom | null {
  if (bytes.length >= 3 && bytes[0] === 0xef && bytes[1] === 0xbb && bytes[2] === 0xbf) {
    return { encoding: "utf-8", length: 3 };
  }
  if (bytes.length >= 2 && bytes[0] === 0xff && bytes[1] === 0xfe) {
    return { encoding: "utf-16le", length: 2 };
  }
  if (bytes.length >= 2 && bytes[0] === 0xfe && bytes[1] === 0xff) {
    return { encoding: "utf-16be", length: 2 };
  }
  return null;
}

/**
 * 无 BOM 的 UTF-16 嗅探：ASCII 文本编码成 UTF-16LE 后是 "h\0e\0l\0l\0o"，
 * NUL 全部落在奇数位；UTF-16BE 则全在偶数位。两者都要求 NUL 占比够高，
 * 否则中文文本（没有 NUL）会被误判。
 */
function sniffUtf16NoBom(bytes: Uint8Array): "utf-16le" | "utf-16be" | null {
  const window = Math.min(bytes.length, 64);
  if (window < 4) return null;
  let zerosEven = 0;
  let zerosOdd = 0;
  let pairs = 0;
  for (let i = 0; i + 1 < window; i += 2) {
    pairs += 1;
    if (bytes[i] === 0) zerosEven += 1;
    if (bytes[i + 1] === 0) zerosOdd += 1;
  }
  if (pairs === 0) return null;
  if (zerosEven / pairs > 0.7 && zerosOdd === 0) return "utf-16be";
  if (zerosOdd / pairs > 0.7 && zerosEven === 0) return "utf-16le";
  return null;
}

function hasNul(bytes: Uint8Array): boolean {
  const n = Math.min(bytes.length, NUL_SNIFF_WINDOW);
  for (let i = 0; i < n; i++) {
    if (bytes[i] === 0) return true;
  }
  return false;
}

/** 严格解码：解不出来返回 null（区分于"解出来是空串"）。 */
function tryDecode(encoding: string, bytes: Uint8Array): string | null {
  try {
    return new TextDecoder(encoding, { fatal: true }).decode(bytes);
  } catch {
    return null;
  }
}

function looseDecode(encoding: string, bytes: Uint8Array): string {
  try {
    return new TextDecoder(encoding).decode(bytes);
  } catch {
    return "";
  }
}

/** 字节 → 文本。永不抛异常：解不出来也给一个可渲染的字符串。 */
export function decodeTextBytes(bytes: Uint8Array): DecodedText {
  if (bytes.length === 0) return { kind: "empty" };

  const bom = sniffBom(bytes);
  if (bom) {
    return {
      kind: "text",
      text: looseDecode(bom.encoding, bytes.subarray(bom.length)),
      encoding: bom.encoding,
    };
  }

  const utf16 = sniffUtf16NoBom(bytes);
  if (utf16) {
    return { kind: "text", text: looseDecode(utf16, bytes), encoding: utf16 };
  }

  if (hasNul(bytes)) return { kind: "binary" };

  const utf8 = tryDecode("utf-8", bytes);
  if (utf8 !== null) return { kind: "text", text: utf8, encoding: "utf-8" };

  const gbk = tryDecode("gbk", bytes);
  if (gbk !== null) return { kind: "text", text: gbk, encoding: "gbk" };

  return { kind: "text", text: looseDecode("utf-8", bytes), encoding: "utf-8" };
}

/**
 * 字节 → `FileSource.readText` 的返回值。两端 adapter 统一走这里，
 * 保证"什么算文本、什么算二进制"的判定只有一份。
 */
export function textResultFromBytes(
  bytes: Uint8Array,
  resolvedPath: string | null,
): ReadTextResult {
  const decoded = decodeTextBytes(bytes);
  if (decoded.kind === "empty") return { ok: true, isEmpty: true, resolvedPath };
  if (decoded.kind === "binary") {
    return { ok: true, isBinary: true, size: bytes.byteLength, resolvedPath };
  }
  return { ok: true, content: decoded.text, resolvedPath };
}
