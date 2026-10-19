import { describe, it, expect } from "vitest";
import { decodeTextBytes, textResultFromBytes } from "./textDecode";

// @vitest-environment node

const utf16le = (s: string) => {
  const out = new Uint8Array(2 + s.length * 2);
  out[0] = 0xff;
  out[1] = 0xfe;
  for (let i = 0; i < s.length; i++) {
    const c = s.charCodeAt(i);
    out[2 + i * 2] = c & 0xff;
    out[3 + i * 2] = c >> 8;
  }
  return out;
};

const gbk = (s: string) => {
  // "你好" = C4 E3 BA C3（GBK）
  const bytes = new Uint8Array([0xc4, 0xe3, 0xba, 0xc3]);
  return s === "你好" ? bytes : new TextEncoder().encode(s);
};

describe("decodeTextBytes", () => {
  it("空字节 → empty", () => {
    expect(decodeTextBytes(new Uint8Array(0)).kind).toBe("empty");
  });

  it("普通 UTF-8 文本原样返回", () => {
    const d = decodeTextBytes(new TextEncoder().encode("你好 world\n第二行"));
    expect(d.kind).toBe("text");
    if (d.kind === "text") {
      expect(d.text).toBe("你好 world\n第二行");
      expect(d.encoding).toBe("utf-8");
    }
  });

  it("UTF-8 BOM 被剥掉而不是变成 U+FEFF", () => {
    const bytes = new Uint8Array([0xef, 0xbb, 0xbf, ...new TextEncoder().encode("hi")]);
    const d = decodeTextBytes(bytes);
    expect(d.kind).toBe("text");
    if (d.kind === "text") {
      expect(d.text).toBe("hi");
      expect(d.encoding).toBe("utf-8");
    }
  });

  it("UTF-16 LE BOM → 正确中文（Windows 记事本默认编码）", () => {
    const d = decodeTextBytes(utf16le("使用手册（简体中文）"));
    expect(d.kind).toBe("text");
    if (d.kind === "text") {
      expect(d.text).toBe("使用手册（简体中文）");
      expect(d.encoding).toBe("utf-16le");
    }
  });

  it("无 BOM 的 UTF-16 LE 靠 NUL 分布嗅探出来", () => {
    const s = "hello";
    const bytes = new Uint8Array(s.length * 2);
    for (let i = 0; i < s.length; i++) {
      bytes[i * 2] = s.charCodeAt(i);
      bytes[i * 2 + 1] = 0;
    }
    const d = decodeTextBytes(bytes);
    expect(d.kind).toBe("text");
    if (d.kind === "text") {
      expect(d.text).toBe("hello");
      expect(d.encoding).toBe("utf-16le");
    }
  });

  it("含 NUL 的字节 → binary", () => {
    const d = decodeTextBytes(new Uint8Array([0x50, 0x4b, 0x03, 0x04, 0x00, 0x01]));
    expect(d.kind).toBe("binary");
  });

  it("GBK 文本在支持该编码的运行时解出中文", () => {
    let supported = true;
    try {
      new TextDecoder("gbk");
    } catch {
      supported = false;
    }
    if (!supported) return; // 运行时无 GBK 解码器（small-icu），跳过断言
    const d = decodeTextBytes(gbk("你好"));
    expect(d.kind).toBe("text");
    if (d.kind === "text") expect(d.text).toBe("你好");
  });
});

describe("textResultFromBytes", () => {
  it("文本 → content", () => {
    const r = textResultFromBytes(new TextEncoder().encode("abc"), "C:/a.txt");
    expect(r).toEqual({ ok: true, content: "abc", resolvedPath: "C:/a.txt" });
  });

  it("二进制 → isBinary + size", () => {
    const r = textResultFromBytes(new Uint8Array([0x00, 0x01, 0x02, 0x00]), "C:/a.bin");
    expect(r).toEqual({ ok: true, isBinary: true, size: 4, resolvedPath: "C:/a.bin" });
  });

  it("空文件 → isEmpty", () => {
    const r = textResultFromBytes(new Uint8Array(0), "C:/a.txt");
    expect(r).toEqual({ ok: true, isEmpty: true, resolvedPath: "C:/a.txt" });
  });
});
