/** 本地直连的请求组装（纯函数部分）——凭据/端点/鉴权头在这里定型，配错就是 401。 */
import { test } from "node:test";
import assert from "node:assert/strict";
import { buildLocalRequest } from "../core/localChat";
import type { ModelConfig } from "../core/model";

const cfg = (providers: ModelConfig["providers"]): ModelConfig => ({
  models: [{ provider_id: "deepseek", model_id: "chat" }],
  providers,
  defaultModel: "deepseek:chat",
  updatedAt: 0,
});

test("组装出 openai 兼容的 /chat/completions 请求", () => {
  const r = buildLocalRequest(
    cfg([
      {
        provider_id: "deepseek",
        base_url: "https://api.deepseek.com/v1",
        api_key: "sk-test",
        api_style: "openai",
      },
    ]),
    "deepseek:chat",
    [{ role: "user", content: "你好" }],
  );
  assert.ok(!("error" in r));
  assert.equal(r.url, "https://api.deepseek.com/v1/chat/completions");
  assert.equal(r.header["Authorization"], "Bearer sk-test");
  assert.equal((r.body as any).model, "chat");
  assert.equal((r.body as any).stream, true);
});

test("端点末尾的斜杠不会拼出双斜杠", () => {
  const r = buildLocalRequest(
    cfg([
      { provider_id: "p", base_url: "https://x.example/v1/", api_key: "k", api_style: "openai" },
    ]),
    "p:m",
    [],
  );
  assert.ok(!("error" in r));
  assert.equal(r.url, "https://x.example/v1/chat/completions");
});

test("没有 key 的端点不发 Authorization 头", () => {
  const r = buildLocalRequest(
    cfg([{ provider_id: "p", base_url: "https://x.example/v1", api_style: "openai" }]),
    "p:m",
    [],
  );
  assert.ok(!("error" in r));
  assert.equal(r.header["Authorization"], undefined);
});

test("没推过来 / 格式不对 / 缺 base_url 都给一句人话", () => {
  const noCred = buildLocalRequest(cfg([]), "deepseek:chat", []);
  assert.ok("error" in noCred && noCred.error.includes("凭据"));
  const badModel = buildLocalRequest(cfg([]), "deepseek", []);
  assert.ok("error" in badModel && badModel.error.includes("provider:model"));
  const noBase = buildLocalRequest(
    cfg([{ provider_id: "p", api_key: "k", api_style: "openai" }]),
    "p:m",
    [],
  );
  assert.ok("error" in noBase && noBase.error.includes("base_url"));
});
