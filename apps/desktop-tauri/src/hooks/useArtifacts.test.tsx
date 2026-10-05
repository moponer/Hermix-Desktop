import { describe, it, expect, vi, beforeEach } from "vitest";
import { renderHook, act } from "@testing-library/react";
import { useArtifacts } from "./useArtifacts";
import { tryArtifactPathExists } from "../utils/artifactReader";
import { subscribeToasts, __resetToastsForTest, type ToastItem } from "./useToast";

/**
 * 点击正文里的路径 → 侧栏预览。存在性闸门在这里。
 *
 * 原来对提取器没抓到的路径是**无条件**合成产物再开预览，文件已删时点开
 * 就是个打不开的空白标签，看着像"点了没反应"。现在先探盘：探不到就报错，
 * 不开标签。
 */
vi.mock("../utils/artifactReader", () => ({
  tryArtifactPathExists: vi.fn(),
}));

const exists = vi.mocked(tryArtifactPathExists);

function setup() {
  const onOpenArtifact = vi.fn();
  const { result } = renderHook(() =>
    useArtifacts({ activeSession: null, workspaceRoot: "F:\\work", onOpenArtifact }),
  );
  return { result, onOpenArtifact };
}

describe("handlePathClick 的存在性闸门", () => {
  beforeEach(() => {
    __resetToastsForTest();
    exists.mockReset();
  });

  it("文件存在 → 开预览，产物 name 是文件名而不是整条路径", async () => {
    exists.mockResolvedValue(true);
    const { result, onOpenArtifact } = setup();

    await act(async () => {
      await result.current.handlePathClick("F:\\work\\pkg\\db1.py");
    });

    expect(exists).toHaveBeenCalledWith("F:\\work\\pkg\\db1.py", "F:\\work");
    expect(onOpenArtifact).toHaveBeenCalledTimes(1);
    expect(onOpenArtifact.mock.calls[0][0]).toMatchObject({
      name: "db1.py",
      path: "F:\\work\\pkg\\db1.py",
    });
  });

  it("文件不存在 → 不开标签，报错", async () => {
    exists.mockResolvedValue(false);
    const seen: ToastItem[] = [];
    const unsub = subscribeToasts((t) => seen.splice(0, seen.length, ...t));
    const { result, onOpenArtifact } = setup();

    await act(async () => {
      await result.current.handlePathClick("F:\\work\\gone\\db1.py");
    });
    unsub();

    expect(onOpenArtifact).not.toHaveBeenCalled();
    expect(seen.some((t) => t.variant === "error" && t.message.includes("db1.py"))).toBe(true);
  });

  it("裸文件名探不到 → 说清是缺目录，而不是谎报文件不存在", async () => {
    exists.mockResolvedValue(false);
    const seen: ToastItem[] = [];
    const unsub = subscribeToasts((t) => seen.splice(0, seen.length, ...t));
    const { result, onOpenArtifact } = setup();

    await act(async () => {
      await result.current.handlePathClick("App.tsx");
    });
    unsub();

    expect(onOpenArtifact).not.toHaveBeenCalled();
    const msg = seen.find((t) => t.variant === "error")?.message ?? "";
    expect(msg).toContain("没有目录");
  });
});
