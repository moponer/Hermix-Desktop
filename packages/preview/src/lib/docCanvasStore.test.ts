// docCanvasStore 单元测试：marker 分流协议的完整状态机覆盖。
// 验收焦点（流畅性铁律的协议侧）：
//  - 块级缓冲：半截 HTML 停在 tail，只在完整块边界上屏
//  - marker 跨 chunk 边界的拼合（切在任意位置都不丢字、不重复进气泡）
//  - done/error 强制收口后缓冲不残留
// 协议：feed() 直接返回"本次应进气泡的文本"；不变量 = tail 里的字符
// 尚未进过气泡也尚未上屏，每个字符恰好出现在气泡或画布之一。
import { describe, it, expect } from "vitest";
import { DocCanvasStore, CANVAS_OPEN_TAG, CANVAS_CLOSE_TAG } from "./docCanvasStore";

/** 收集 store 的块与元信息事件，返回句柄便于断言。 */
function harness() {
  const s = new DocCanvasStore();
  const blocks: string[] = [];
  const metas: { geometry: string | null; blockCount: number; closed: boolean }[] = [];
  s.onBlock((html) => blocks.push(html));
  s.onMeta((m) => metas.push({ ...m }));
  return { s, blocks, metas };
}

const OPEN_A4 = `${CANVAS_OPEN_TAG} page="a4">`;
const OPEN_SLIDE = `${CANVAS_OPEN_TAG} page="slide16x9">`;

/** 把整段文本按 cut 宽度切块喂入，汇总气泡与块。 */
function stream(s: DocCanvasStore, full: string, cut: number) {
  let bubble = "";
  for (let i = 0; i < full.length; i += cut) {
    bubble += s.feed(full.slice(i, i + cut));
  }
  bubble += s.close();
  return bubble;
}

describe("docCanvasStore marker 分流", () => {
  it("无 marker：全部归气泡，store 不开启", () => {
    const h = harness();
    expect(h.s.feed("普通回答第一段。")).toBe("普通回答第一段。");
    expect(h.s.feed("第二段继续。")).toBe("第二段继续。");
    expect(h.s.getMeta().geometry).toBeNull();
    expect(h.blocks.length).toBe(0);
  });

  it("单 chunk 内完整 marker：marker 前文字进气泡，画布开启", () => {
    const h = harness();
    const bubble = h.s.feed(`前言文字。${OPEN_SLIDE}`);
    expect(bubble).toBe("前言文字。");
    expect(h.s.getMeta().geometry).toBe("slide16x9");
    expect(h.s.getMeta().closed).toBe(false);
  });

  it("slide 几何：块边界 </slide> 才上屏，半截停在缓冲", () => {
    const h = harness();
    h.s.feed(OPEN_SLIDE);
    h.s.feed('<slide><h1>封面</h1');
    expect(h.blocks.length).toBe(0);
    h.s.feed("></slide>");
    expect(h.blocks.length).toBe(1);
    expect(h.blocks[0]).toMatch(/<h1>封面<\/h1>/);
    expect(h.s.getMeta().blockCount).toBe(1);
  });

  it("a4 几何：块边界是 </section>", () => {
    const h = harness();
    h.s.feed(OPEN_A4);
    h.s.feed("<section><p>第一页</p></section><section><p>第二");
    expect(h.blocks.length).toBe(1);
    h.s.feed("_页</section>");
    expect(h.blocks.length).toBe(2);
  });

  it("marker 跨 chunk 边界：切在任意位置都不丢字、不漏进气泡", () => {
    const full = `开场白。${OPEN_A4}<section>内容一</section>${CANVAS_CLOSE_TAG}`;
    for (const cut of [1, 2, 3, 5, 7, 12, 20, 64]) {
      const h = harness();
      const bubble = stream(h.s, full, cut);
      expect(bubble, `cut=${cut} 气泡`).toBe("开场白。");
      expect(h.blocks, `cut=${cut} 块`).toEqual(["<section>内容一</section>"]);
    }
  });

  it("正文含 '<' 与 'hermix' 字样但不构成 marker：不误分流", () => {
    const h = harness();
    const text = "如果 a < b 且 hermix-canvas 出现在散文里 <hermix-x 也不算";
    expect(h.s.feed(text)).toBe(text);
    expect(h.s.getMeta().geometry).toBeNull();
    h.s.close();
    expect(h.blocks.length).toBe(0);
  });

  it("close(): 未闭合 marker 强制收口，尾缓冲上屏", () => {
    const h = harness();
    h.s.feed(OPEN_SLIDE);
    h.s.feed("<slide>未闭合的页");
    expect(h.blocks.length).toBe(0);
    h.s.close();
    expect(h.blocks.length).toBe(1);
    expect(h.blocks[0]).toMatch(/未闭合的页/);
    expect(h.s.getMeta().closed).toBe(true);
  });

  it("close(): 流断在疑似 marker 前缀上，残留补回气泡", () => {
    const h = harness();
    // 最后一帧只送了 "<hermix" —— 是真前缀，被扣留。
    const bubble = h.s.feed("结论如下 <hermix");
    expect(bubble).toBe("结论如下 ");
    expect(h.s.close()).toBe("<hermix");
    expect(h.blocks.length).toBe(0);
  });

  it("close() 后再 feed：开启新画布", () => {
    const h = harness();
    h.s.feed(`${OPEN_A4}<section>A</section>${CANVAS_CLOSE_TAG}`);
    expect(h.s.getMeta().closed).toBe(true);
    h.s.feed(`${OPEN_A4}<section>B</section>`);
    expect(h.s.getMeta().closed).toBe(false);
    expect(h.blocks.length).toBe(2);
  });

  it("空块不上屏：纯空白的缓冲不触发 block 事件", () => {
    const h = harness();
    h.s.feed(`${OPEN_A4}   <section>x</section>`);
    expect(h.blocks.length).toBe(1);
  });

  it("canvas 后尾注被安全丢弃", () => {
    const h = harness();
    h.s.feed(`${OPEN_A4}<section>A</section>${CANVAS_CLOSE_TAG}尾部絮语`);
    expect(h.blocks.length).toBe(1);
    expect(h.s.getMeta().closed).toBe(true);
  });
});
