// iconResolver 防回归测试：钉死「icon_file 分支走 stdio bridge 拉 blob」的
// 语义，防止有人改回裸 <img src="/api/custom-apps/..."> 的老写法
// （生产环境 <img src> 解析到 Tauri 内部 URL → 永远 404 → 上传的 PNG
// 图标全部静默失效，见 2026-09-26 审计批次）。
import { describe, expect, it, vi } from "vitest";
import { render, waitFor } from "@testing-library/react";
import * as rest from "../../api/rest";
import { resolveAppIconEl, type IconLike } from "./iconResolver";

const pngBlob = () =>
  new Blob([Uint8Array.from([137, 80, 78, 71, 13, 10])], {
    type: "image/png",
  });

describe("resolveAppIconEl — icon_file 分支", () => {
  // jsdom 无 URL.createObjectURL 原生实现（返回 undefined 而非抛错），
  // 组件拿到 undefined 后会卡在不渲染 <img> 的降级分支。补 fake 实现。
  const revokeSpy = vi.fn();
  vi.stubGlobal("URL", {
    createObjectURL: vi.fn((blob: Blob) => `blob:fake-${blob.size}`),
    revokeObjectURL: revokeSpy,
  });
  it("icon_file 存在时走 fetchJSONBinary 拉 blob，不产出裸后端路径 src", async () => {
    vi.spyOn(rest, "fetchJSONBinary").mockResolvedValue(pngBlob());
    const app: IconLike = {
      id: "app-1",
      name: "Music",
      icon: "music",
      icon_file: "icon.png",
    };
    // 等待异步 effect：blob → objectURL → <img> 出现且 src 落到 blob: 前缀。
    const { container } = render(resolveAppIconEl(app));
    const img = await waitFor(() => {
      const el = container.querySelector("img");
      if (!el) throw new Error("img not yet rendered");
      return el;
    });
    expect(img.getAttribute("src")).toMatch(/^blob:/);

    // 防回归核心断言：任何 <img> 的 src 都不允许是裸后端路径。
    for (const el of container.querySelectorAll("img")) {
      expect(el.getAttribute("src")).not.toMatch(/^\/api\//);
    }
  });

  it("bridge 404（icon_file 声明了但文件不存在）时降级 lucide，不留空 <img>", async () => {
    vi.spyOn(rest, "fetchJSONBinary").mockRejectedValue(
      new Error("HTTP 404: 此应用没有自定义图标"),
    );
    const app: IconLike = {
      id: "app-2",
      name: "Music Player",
      icon: "music",
      icon_file: "missing.png",
    };
    const { container } = render(resolveAppIconEl(app));

    // 降级后容器内不允许残留 <img>（lucide 渲染为 <svg>）。
    await vi.waitFor(() =>
      expect(container.querySelector("img")).toBeNull(),
    );
    expect(container.querySelector("svg")).not.toBeNull();
  });

  it("无 icon_file 时不碰 bridge，直接走 emoji/lucide 路径", () => {
    const spy = vi.spyOn(rest, "fetchJSONBinary");
    const app: IconLike = {
      id: "app-3",
      name: "Notes",
      icon: "📝",
    };
    const { container } = render(resolveAppIconEl(app));
    expect(spy).not.toHaveBeenCalled();
    expect(container.textContent).toContain("📝");
  });
});
