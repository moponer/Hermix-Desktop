// ScreenshotEditor 文本工具：mousedown 的默认聚焦行为必须被吃掉。
//
// 2026-10-28 的真实缺陷：canvas 不可聚焦，浏览器在 mousedown 的**默认动作**
// 里把焦点交给 body；而文本输入框恰在同一次事件里挂载并 autoFocus，于是
// 焦点一到手就被夺走 → onBlur 拿着空串把输入框关掉 → 「选文本工具后点画布
// 什么也不发生」。jsdom 不实现这套默认聚焦行为，所以这里直接钉住根因：
// 文本工具下画布收到的 mousedown 必须 `defaultPrevented`，且输入框在 DOM 里
// 活下来。删掉 preventDefault 这一行，本用例即失败。
import { render, screen, fireEvent, createEvent } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { ScreenshotEditor, pxForDisplay } from "./ScreenshotEditor";

vi.mock("../ipc", () => ({ invoke: vi.fn(async () => null) }));

const canvas = () => document.querySelector(".se-canvas") as HTMLCanvasElement;

describe("ScreenshotEditor 文本工具", () => {
  it("点画布会阻止 mousedown 默认聚焦，并留下可输入的文本框", () => {
    render(<ScreenshotEditor source={{ src: "data:image/png;base64,", scale: 1 }} onCancel={() => {}} />);
    fireEvent.click(screen.getByRole("button", { name: "点击画布输入文字" }));

    const ev = createEvent.mouseDown(canvas(), { clientX: 40, clientY: 40, button: 0 });
    fireEvent(canvas(), ev);

    expect(ev.defaultPrevented).toBe(true);
    expect(document.querySelector(".se-text-input")).not.toBeNull();
  });
});

describe("pxForDisplay 标注尺寸换算", () => {
  it("1:1 显示时不改变尺寸（dpr 1）", () => {
    expect(pxForDisplay(20, 1, 1)).toBe(20);
    expect(pxForDisplay(4, 1, 1)).toBe(4);
  });

  it("高 DPI 屏放大到物理像素（150% 屏 → ×1.5）", () => {
    // dpr=1.5 且图像 1:1 显示时 disp.k = 1/1.5
    expect(pxForDisplay(20, 1.5, 1 / 1.5)).toBe(30);
  });

  it("图像缩小显示时按比例放大（4000px 宽的截图缩到 884px）", () => {
    // disp.k ≈ 0.221 → 屏幕上 20px 的字要占 90 个物理像素，
    // 否则就是 4.4px 的"看不见"（2026-10-28 用户报）。
    expect(pxForDisplay(20, 1, 0.221)).toBe(90);
  });

  it("首帧没量到舞台尺寸（disp.k=0）时退回 dpr", () => {
    expect(pxForDisplay(20, 2, 0)).toBe(40);
  });
});
