// Markdown 预览的纯文本逻辑 + 本地相对路径解析。
//
// 这两样是"预览出来的文档还认不认识自己的文件"的关键：frontmatter 混进
// 正文会让文档头部多出一坨元数据，相对路径不解会让满屏裂图。

import { describe, expect, it } from "vitest";
import { dirnameOf, isExternalHref, resolveRelative } from "./localAssets";
import { extractHeadings, stripFrontmatter } from "./markdownDoc";

describe("stripFrontmatter", () => {
  it("去掉开头的 YAML frontmatter", () => {
    const src = "---\ntitle: x\n---\n\n# 正文\n";
    expect(stripFrontmatter(src)).toBe("\n# 正文\n");
  });

  it("支持 TOML 定界符", () => {
    expect(stripFrontmatter("+++\na = 1\n+++\n正文")).toBe("正文");
  });

  it("没有 frontmatter 时原样返回", () => {
    expect(stripFrontmatter("# 正文\n")).toBe("# 正文\n");
  });

  it("正文中间的 --- 分隔线不能被吃掉", () => {
    const src = "# 标题\n\n---\n\n正文\n";
    expect(stripFrontmatter(src)).toBe(src);
  });
});

describe("extractHeadings", () => {
  it("取出各级标题", () => {
    expect(extractHeadings("# A\n\n## B\n\n### C\n")).toEqual([
      { level: 1, text: "A" },
      { level: 2, text: "B" },
      { level: 3, text: "C" },
    ]);
  });

  it("围栏代码块里的 # 行不算标题", () => {
    const src = "# 真标题\n\n```sh\n# 这是注释\n```\n\n## 另一个\n";
    expect(extractHeadings(src)).toEqual([
      { level: 1, text: "真标题" },
      { level: 2, text: "另一个" },
    ]);
  });

  it("剥掉标题里的行内记号", () => {
    expect(extractHeadings("## **粗** 与 `码`\n")).toEqual([
      { level: 2, text: "粗 与 码" },
    ]);
  });
});

describe("本地相对路径", () => {
  it("取目录并保留尾部分隔符", () => {
    expect(dirnameOf("C:\\docs\\notes\\a.md")).toBe("C:\\docs\\notes\\");
    expect(dirnameOf("C:\\a.md")).toBe("C:\\");
  });

  it("同目录相对路径直接拼接", () => {
    expect(resolveRelative("C:\\docs\\", "img/a.png")).toBe("C:\\docs\\img\\a.png");
  });

  it("../ 回退不会弹掉盘符", () => {
    expect(resolveRelative("C:\\docs\\sub\\", "../img/a.png")).toBe("C:\\docs\\img\\a.png");
    expect(resolveRelative("C:\\docs\\", "../../../a.png")).toBe("C:\\a.png");
  });

  it("百分号编码被解码（Markdown 里空格必须写 %20）", () => {
    expect(resolveRelative("C:\\docs\\", "my%20file.png")).toBe("C:\\docs\\my file.png");
  });

  it("绝对地址与 data: 原样返回", () => {
    expect(resolveRelative("C:\\docs\\", "https://x.com/a.png")).toBe("https://x.com/a.png");
    expect(resolveRelative("C:\\docs\\", "data:image/png;base64,AA")).toBe(
      "data:image/png;base64,AA",
    );
  });

  it("POSIX 根目录保留前导斜杠", () => {
    expect(resolveRelative("/srv/docs/", "img/a.png")).toBe("/srv/docs/img/a.png");
  });

  it("isExternalHref 识别协议与协议相对地址", () => {
    expect(isExternalHref("https://x.com")).toBe(true);
    expect(isExternalHref("file:///c/a.png")).toBe(true);
    expect(isExternalHref("//cdn/a.png")).toBe(true);
    expect(isExternalHref("img/a.png")).toBe(false);
    expect(isExternalHref("./a.png")).toBe(false);
  });
});
