import { describe, it, expect } from "vitest";
import { resolveSessionSelectedModel } from "./sessionModel";

describe("resolveSessionSelectedModel", () => {
  it("会话自身模型有效时优先采用（不被默认覆盖）", () => {
    expect(resolveSessionSelectedModel("openai:gpt-4", "siliconflow:Qwen2.5")).toBe("openai:gpt-4");
  });

  it("会话无独立模型时回落到当前默认，避免串台", () => {
    // 上一个会话残留的 selectedModel 不在参数里，调用方传入 defaultModel
    expect(resolveSessionSelectedModel(null, "siliconflow:Qwen2.5")).toBe("siliconflow:Qwen2.5");
    expect(resolveSessionSelectedModel(undefined, "siliconflow:Qwen2.5")).toBe("siliconflow:Qwen2.5");
  });

  it("会话模型为中毒 sentinel 'unknown' 时回落默认", () => {
    expect(resolveSessionSelectedModel("unknown", "siliconflow:Qwen2.5")).toBe("siliconflow:Qwen2.5");
  });

  it("会话模型缺少 provider 前缀时不采用，回落默认", () => {
    expect(resolveSessionSelectedModel("gpt-4", "siliconflow:Qwen2.5")).toBe("siliconflow:Qwen2.5");
  });

  it("会话无独立模型且全局也没默认时回落空串", () => {
    expect(resolveSessionSelectedModel(null, "")).toBe("");
    expect(resolveSessionSelectedModel("unknown", "")).toBe("");
  });
});
