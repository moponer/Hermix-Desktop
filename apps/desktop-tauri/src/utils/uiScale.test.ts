import { describe, it, expect, beforeEach, vi } from "vitest";
import { applyScale, readScale, restoreScale, SCALE_MIN, SCALE_MAX } from "./uiScale";
import { storage } from "./storageCompat";

/** 清干净 DOM 上的 zoom 与持久层，避免用例互相污染。 */
function reset() {
  document.documentElement.style.zoom = "";
  try { storage.removeItem("hermix.desktop.ui-scale"); } catch {}
}

describe("uiScale", () => {
  beforeEach(reset);

  it("readScale 默认 1（无 DOM 值、无存储值）", () => {
    expect(readScale()).toBe(1);
  });

  it("applyScale 同时写 DOM、持久层，并派发 UI_SCALE_EVENT", () => {
    const onScale = vi.fn();
    window.addEventListener("hermix:ui-scale", onScale);

    const applied = applyScale(1.15);

    expect(applied).toBe(1.15);
    expect(document.documentElement.style.zoom).toBe("1.15");
    expect(storage.getItem("hermix.desktop.ui-scale")).toBe("1.15");
    expect(onScale).toHaveBeenCalledTimes(1);
    // 布局层靠事件里的 detail 取值，必须是数值而非字符串
    expect((onScale.mock.calls[0][0] as CustomEvent).detail).toBe(1.15);

    window.removeEventListener("hermix:ui-scale", onScale);
  });

  it("applyScale 夹紧到 [SCALE_MIN, SCALE_MAX]", () => {
    expect(applyScale(9)).toBe(SCALE_MAX);
    expect(applyScale(0.1)).toBe(SCALE_MIN);
  });

  it("applyScale 取整到小数点后两位（避免 0.8500000000000001）", () => {
    expect(applyScale(0.85 + 0.1)).toBe(0.95);
  });

  it("restoreScale 恢复上次保存的值 —— 这是重启后不跳回 100% 的关键", () => {
    storage.setItem("hermix.desktop.ui-scale", "1.3");
    expect(document.documentElement.style.zoom).toBe("");

    restoreScale();

    expect(document.documentElement.style.zoom).toBe("1.3");
  });

  it("restoreScale 对 1 / 无效 / 缺失值不动 DOM", () => {
    document.documentElement.style.zoom = "1.15";

    storage.setItem("hermix.desktop.ui-scale", "1");
    restoreScale();
    expect(document.documentElement.style.zoom).toBe("1.15");

    storage.setItem("hermix.desktop.ui-scale", "abc");
    restoreScale();
    expect(document.documentElement.style.zoom).toBe("1.15");

    storage.removeItem("hermix.desktop.ui-scale");
    restoreScale();
    expect(document.documentElement.style.zoom).toBe("1.15");
  });

  it("readScale 优先信 DOM 实际值（启动恢复已写入时）", () => {
    storage.setItem("hermix.desktop.ui-scale", "1");
    document.documentElement.style.zoom = "1.3";
    expect(readScale()).toBe(1.3);
  });
});
