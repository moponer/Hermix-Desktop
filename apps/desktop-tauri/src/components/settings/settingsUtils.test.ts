import { describe, expect, it } from "vitest";
import { isChatModel, pickDefaultModel } from "./settingsUtils";

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

  it("全是非对话模型时退回原始列表，避免一个都不选", () => {
    expect(pickDefaultModel(["text-embedding-3-small", "tts-1"])).toBe("text-embedding-3-small");
  });

  it("空列表返回 null（调用方应提示手选，不硬塞默认）", () => {
    expect(pickDefaultModel([])).toBeNull();
    expect(pickDefaultModel([], "gpt-4o")).toBeNull();
  });
});
