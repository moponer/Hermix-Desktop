// AI 辅助清理的前端桥：把清理候选交给后端裁决，回来的结果按项落到界面上。
//
// 契约（后端 apps/viewer/src-tauri/src/cleanup_ai.rs）：
// - 后端只接受「候选元信息」，不接受路径 —— 执行时服务端按规则重新展开，
//   前端传什么路径都删不到规则之外的东西；
// - 返回的每条裁决都经过服务端校验（id 必须在候选集内、decision 三选一、
//   本地高风险项的越权建议被丢弃并计数）；
// - 解析失败时后端返回空建议 → 这里表现为"没有任何 AI 标注"，
//   **不会**让界面看起来像 AI 认可了什么。

import { invoke } from "@tauri-apps/api/core";

/** 送进模型的候选（只送元信息，不送路径）。 */
export interface AiCandidateInput {
  id: string;
  title: string;
  group: string;
  bytes: number;
  files: number;
  risky: boolean;
  needsAdmin: boolean;
  hint: string;
}

/** 模型对一项的裁决。 */
export interface AiVerdict {
  id: string;
  decision: "clean" | "keep" | "unsure";
  confidence: number;
  reason: string;
}

export interface AiAdviceReport {
  verdicts: AiVerdict[];
  /** 被本地安全网拦下的越权建议条数。 */
  blocked: number;
  /** 模型给了未知 / 非法 id 而被丢弃的条数。 */
  dropped: number;
  truncated: boolean;
  considered: number;
  model: string;
  notes: string;
}

/** AI 后端是否可用（未配置时界面据此置灰按钮并显示原因）。 */
export interface AiEndpointInfo {
  source: string;
  provider: string;
  baseUrl: string;
  model: string;
  hasKey: boolean;
  ready: boolean;
  problem: string | null;
}

export async function fetchAiStatus(): Promise<AiEndpointInfo> {
  return invoke<AiEndpointInfo>("viewer_ai_status");
}

/**
 * 请求 AI 裁决。后端出错（未配置 / 端点挂了 / 模型返回了非法 JSON）时
 * **抛错**，由调用方决定怎么提示 —— 静默失败会让用户以为"AI 什么都没说"。
 */
export async function adviseCleanup(items: AiCandidateInput[]): Promise<AiAdviceReport> {
  return invoke<AiAdviceReport>("cleanup_ai_advise", { items });
}

/** 裁决 → 界面用的 Map（id → 裁决）。 */
export function verdictMap(report: AiAdviceReport | null): Map<string, AiVerdict> {
  const map = new Map<string, AiVerdict>();
  for (const verdict of report?.verdicts ?? []) map.set(verdict.id, verdict);
  return map;
}
