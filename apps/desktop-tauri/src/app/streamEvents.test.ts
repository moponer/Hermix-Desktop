import { describe, it, expect } from "vitest";
import { classifyStreamErrorKind, localizeStreamError } from "./streamEvents";

// 这些用例锁死一条易回退的约定：**本地后端故障**与 **provider 故障**
// 即使 HTTP 状态码相同，也必须分类成不同结果。曾经的实现只看数字，
// 把后端 503（服务没起来）和 provider 503（对方过载）混为一谈，
// 用户按"稍后重试"的提示等下去永远等不到结果。
describe("classifyStreamErrorKind — 本地后端与 provider 的区分", () => {
  it("本地后端 4xx 归为 bad_request，而不是网络故障", () => {
    expect(classifyStreamErrorKind("HTTP 422: body")).toBe("bad_request");
    expect(classifyStreamErrorKind("HTTP 400: body")).toBe("bad_request");
  });

  it("本地后端 5xx 归为 overloaded（可重试），不是 network", () => {
    expect(classifyStreamErrorKind("HTTP 503: body")).toBe("overloaded");
    expect(classifyStreamErrorKind("HTTP 500: body")).toBe("overloaded");
  });

  it("后端进程下线仍识别为 bad_request 类别（无需用户查网络）", () => {
    expect(classifyStreamErrorKind("backend offline")).toBe("bad_request");
    expect(classifyStreamErrorKind("backend exited")).toBe("bad_request");
    expect(classifyStreamErrorKind("stdio bridge not initialised")).toBe("bad_request");
  });

  it("provider 侧的同类状态码不受影响", () => {
    expect(classifyStreamErrorKind("LLM API error: 529 overloaded")).toBe("overloaded");
    expect(classifyStreamErrorKind("429 too many requests")).toBe("quota");
    expect(classifyStreamErrorKind("401 unauthorized")).toBe("auth");
  });

  it("HTTP 4xx 不会被误判成 auth（否则会把用户引去改 API key）", () => {
    expect(classifyStreamErrorKind("HTTP 401: body")).toBe("bad_request");
  });
});

describe("localizeStreamError — 文案不得误导", () => {
  it("后端下线提示去重启服务，而不是查端口", () => {
    const msg = localizeStreamError("backend offline");
    expect(msg).toContain("本地后端未运行");
    expect(msg).not.toContain("端口");
  });

  it("后端 4xx 明确告知无需检查网络或端口", () => {
    const msg = localizeStreamError("HTTP 422: invalid type: string");
    expect(msg).toContain("后端拒绝");
    expect(msg).toContain("无需检查网络或端口");
  });

  it("provider 过载仍然提示稍后重试", () => {
    expect(localizeStreamError("LLM API error: 529 overloaded")).toContain("过载");
  });
});
