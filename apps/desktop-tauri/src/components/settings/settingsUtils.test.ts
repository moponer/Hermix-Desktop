import { describe, expect, it } from "vitest";
import { isChatModel, mergeFetchedModels, pickDefaultModel } from "./settingsUtils";
import type { ModelConfigData } from "./settingsUtils";

// 回归背景：厂商 `/v1/models` 返回的是全量目录（含 embedding / 图像 /
// 语音 / 旧版 completion 模型）。此前「获取模型」直接取 `models.sort()[0]`
// 当默认模型，于是 OpenAI 会选中 `babbage-002`、硅基流动会选中
// `BAAI/bge-*`，配完点聊天直接报错。

// OpenAI /v1/models 的真实形态（截取相关子集）
const OPENAI_CATALOG = [
  "babbage-002",
  "dall-e-3",
  "davinci-002",
  "gpt-3.5-turbo",
  "gpt-4o",
  "text-embedding-3-small",
  "tts-1",
  "whisper-1",
];

// 硅基流动 /v1/models 的真实形态（截取相关子集）
const SILICONFLOW_CATALOG = [
  "BAAI/bge-large-zh-v1.5",
  "BAAI/bge-m3",
  "Qwen/Qwen2.5-72B-Instruct",
  "Qwen/Qwen3-8B",
  "deepseek-ai/DeepSeek-V3",
];

describe("isChatModel", () => {
  it("排除 embedding / 语音 / 图像 / 旧版 completion 模型", () => {
    const nonChat = [
      "text-embedding-3-small",
      "BAAI/bge-m3",
      "BAAI/bge-large-zh-v1.5",
      "tts-1",
      "whisper-1",
      "dall-e-3",
      "babbage-002",
      "davinci-002",
      "bge-reranker-v2-m3",
    ];
    for (const id of nonChat) expect(isChatModel(id), id).toBe(false);
  });

  it("放行对话模型（含带 vision 后缀的）", () => {
    const chat = [
      "gpt-4o",
      "gpt-3.5-turbo",
      "deepseek-chat",
      "deepseek-reasoner",
      "qwen-vl-max",
      "claude-3-5-sonnet-latest",
      "zai-org/GLM-4.6",
    ];
    for (const id of chat) expect(isChatModel(id), id).toBe(true);
  });
});

describe("pickDefaultModel", () => {
  it("优先命中后端权威默认模型", () => {
    expect(pickDefaultModel(OPENAI_CATALOG, "gpt-4o")).toBe("gpt-4o");
    expect(pickDefaultModel(SILICONFLOW_CATALOG, "Qwen/Qwen2.5-72B-Instruct")).toBe(
      "Qwen/Qwen2.5-72B-Instruct",
    );
  });

  it("权威默认不在账号可用列表时忽略大小写兜底匹配", () => {
    expect(pickDefaultModel(["DeepSeek-Chat", "DeepSeek-Reasoner"], "deepseek-chat")).toBe(
      "DeepSeek-Chat",
    );
  });

  it("没有权威默认时也不取字典序第一个的非对话模型", () => {
    expect(pickDefaultModel(OPENAI_CATALOG)).toBe("gpt-3.5-turbo");
    expect(pickDefaultModel(SILICONFLOW_CATALOG)).toBe("Qwen/Qwen2.5-72B-Instruct");
  });

  it("唯一候选是权威默认时不对话也采信（后端清单是权威）", () => {
    // 启发式过滤只用于"没有权威默认时的兜底挑选"，不该推翻后端明确指定的模型
    expect(pickDefaultModel(["dall-e-3"], "dall-e-3")).toBe("dall-e-3");
  });

  it("全是非对话模型时返回 null，不回退到不能聊天的模型", () => {
    expect(pickDefaultModel(["text-embedding-3-small", "tts-1"])).toBeNull();
    expect(pickDefaultModel(["BAAI/bge-m3"])).toBeNull();
  });

  it("空列表返回 null（调用方应提示手选，不硬塞默认）", () => {
    expect(pickDefaultModel([])).toBeNull();
    expect(pickDefaultModel([], "gpt-4o")).toBeNull();
  });
});

const empty: ModelConfigData = { enabled: [], defaultModel: "", modelContexts: {} };

describe("mergeFetchedModels", () => {
  it("新机器首次挂载：启用可对话模型并补默认模型", () => {
    const next = mergeFetchedModels(empty, [{ id: "deepseek", models: ["deepseek-chat", "deepseek-reasoner"] }], {
      deepseek: "deepseek-chat",
    });
    expect(next.enabled).toEqual([
      { providerId: "deepseek", modelId: "deepseek-chat" },
      { providerId: "deepseek", modelId: "deepseek-reasoner" },
    ]);
    expect(next.defaultModel).toBe("deepseek:deepseek-chat");
  });

  it("不把非对话模型塞进已启用列表", () => {
    const next = mergeFetchedModels(empty, [{ id: "openai", models: OPENAI_CATALOG }], { openai: "gpt-4o" });
    const ids = next.enabled.map((e) => `${e.providerId}:${e.modelId}`);
    expect(ids).toEqual(["openai:gpt-3.5-turbo", "openai:gpt-4o"]);
    expect(ids).not.toContain("openai:babbage-002");
    expect(ids).not.toContain("openai:tts-1");
  });

  it("已启用的模型不重复添加", () => {
    const base: ModelConfigData = {
      enabled: [{ providerId: "deepseek", modelId: "deepseek-chat" }],
      defaultModel: "deepseek:deepseek-chat",
      modelContexts: {},
    };
    const next = mergeFetchedModels(base, [{ id: "deepseek", models: ["deepseek-chat"] }], {});
    expect(next).toBe(base); // 引用相等 → 调用方可跳过落盘
    expect(next.enabled).toHaveLength(1);
  });

  it("已有默认模型时不被覆盖", () => {
    const base: ModelConfigData = {
      enabled: [],
      defaultModel: "kimi:moonshot-v1-128k",
      modelContexts: {},
    };
    const next = mergeFetchedModels(base, [{ id: "openai", models: ["gpt-4o"] }], { openai: "gpt-4o" });
    expect(next.defaultModel).toBe("kimi:moonshot-v1-128k");
  });

  it("没有探测结果时原样返回", () => {
    expect(mergeFetchedModels(empty, [])).toBe(empty);
  });

  it("模型列表全是非对话模型时不硬设默认", () => {
    const next = mergeFetchedModels(empty, [{ id: "siliconflow", models: ["BAAI/bge-m3"] }], {});
    expect(next.defaultModel).toBe("");
  });

  it("用户移除过的模型（墓碑）不再自动加回", () => {
    // 回归背景：厂商目录里必然还挂着被移除的模型，没有墓碑时每次合并都会
    // 把刚删的模型原样复活（"删除后刷新还在"）。
    const base: ModelConfigData = {
      enabled: [],
      defaultModel: "",
      modelContexts: {},
      dismissed: ["deepseek:deepseek-chat"],
    };
    const next = mergeFetchedModels(
      base,
      [{ id: "deepseek", models: ["deepseek-chat", "deepseek-reasoner"] }],
      {},
    );
    const ids = next.enabled.map((e) => `${e.providerId}:${e.modelId}`);
    expect(ids).toEqual(["deepseek:deepseek-reasoner"]);
  });

  it("墓碑随合并结果保留，不被丢弃", () => {
    const base: ModelConfigData = {
      enabled: [],
      defaultModel: "",
      modelContexts: {},
      dismissed: ["openai:gpt-4o"],
    };
    const next = mergeFetchedModels(
      base,
      [{ id: "openai", models: ["gpt-4o", "gpt-3.5-turbo"] }],
      {},
    );
    expect(next.dismissed).toEqual(["openai:gpt-4o"]);
  });
});
