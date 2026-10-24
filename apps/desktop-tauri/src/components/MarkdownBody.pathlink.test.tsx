// @vitest-environment node

import { describe, it, expect } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { linkifyChildren } from "./MarkdownBody";

/**
 * 行内路径链接的显示契约。
 *
 * 正文里 LLM 写的是一整串绝对路径，但**显示**只能是文件名 —— 铺满整行
 * 会把句子撕成两段，读者真正要找的是 `db1.py` 本身。目录信息不能丢：
 * title 属性仍是完整路径，悬停可见、点击也仍按完整路径解析。
 *
 * 图标只在最终答复正文出现（`linkifyChildren` 的第三个参数，由
 * MarkdownBody 的 `pathIcons` 传入）：思考块 / 工具结果复用同一个渲染器，
 * 一段十几个路径全顶方块会毁掉可读性。
 *
 * 图标走 **soft 形态**（淡染底 + 文字色字形，不是品牌实色方块 + 白色
 * 粗体字母）—— 用户实机反馈"一部分重点加粗带背景色的特殊词汇……界面
 * 有点乱"，实色方块就是那个噪声源。
 *
 * 图标尺寸锁 16px：图标是「色块 + 扩展名字母」，字母在 24 viewBox 里
 * 只有 7.5–10.5px，13px 渲染出来约 5.7px，小到认不出类型。
 * 对齐说明见 components-prose.css 的 .md-path-link__icon。
 *
 * 走 node 环境而非 jsdom：satteri 是 NAPI 模块，jsdom 下会撞 DataView
 * 视图转换问题（见 MarkdownBody.test.tsx 顶部说明）。本用例只调
 * linkifyChildren，不经过 satteri 解析。
 */

const noop = () => {};

function render(text: string, icons = false): string {
  return renderToStaticMarkup(<>{linkifyChildren(text, noop, icons)}</>);
}

describe("行内路径链接（PathLink）", () => {
  it("只显示文件名，完整路径留给 title", () => {
    const html = render("改完了 F:\\spiker\\hermix\\db1.py 这个文件");

    // 可见文本 = 文件名
    expect(html).toContain(">db1.py<");
    // 完整路径仍在 title 里（悬停可见 + 点击按它解析）
    expect(html).toContain('title="打开 F:\\spiker\\hermix\\db1.py"');
  });

  it("默认不画图标（思考块 / 工具结果共用同一渲染器）", () => {
    const html = render("改完了 F:\\spiker\\hermix\\db1.py 这个文件");
    expect(html).not.toContain("md-path-link__icon");
    // 路径本身仍然是可点的链接，只是不带图标
    expect(html).toContain("md-path-link--abs");
  });

  it("最终答复正文开图标，尺寸 16px（13px 时扩展名字母认不出）", () => {
    const html = render("改完了 F:\\spiker\\hermix\\db1.py 这个文件", true);
    expect(html).toContain('width="16"');
    expect(html).toContain("md-path-link__icon");
    expect(html).not.toContain('width="13"');
  });

  it("正文图标是 soft 形态（淡染底 + 文字色字形，不是实色方块）", () => {
    const html = render("改完了 F:\\spiker\\hermix\\db1.py 这个文件", true);
    // 淡染：底色不透明度 0.22 → 0.14（实色形态是 1 → 0.86）
    expect(html).toContain('stop-opacity="0.22"');
    expect(html).toContain('stop-opacity="0.14"');
    expect(html).not.toContain('stop-opacity="1"');
    // 字形走 currentColor（正文色），不是白色
    expect(html).toContain('fill="currentColor"');
  });

  it("%VAR% 环境变量路径完整保留前缀，不再被 lookbehind 切断", () => {
    const html = render("输出到 %TEMP%\\hermix-scratch\\creds_utf8.csv（2606 字节）");
    expect(html).toContain("%TEMP%\\hermix-scratch\\creds_utf8.csv");
    expect(html).toContain(">creds_utf8.csv<");
    // 语义上等价绝对路径（后端展开后可定位），不染相对路径警告色
    expect(html).toContain("md-path-link--abs");
    expect(html).not.toContain("md-path-link--rel");
  });

  it("正斜杠路径同样只取文件名", () => {
    const html = render("see apps/desktop-tauri/src/App.tsx for details");
    expect(html).toContain(">App.tsx<");
    expect(html).not.toContain(">apps/desktop-tauri/src/App.tsx<");
  });

  it("裸文件名（无目录）原样显示，不做目录改写", () => {
    const html = render("改 App.tsx 的登录逻辑");
    expect(html).toContain(">App.tsx<");
    expect(html).toContain("md-path-link--bare");
  });
});
