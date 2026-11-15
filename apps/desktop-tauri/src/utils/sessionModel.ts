// 会话切换时解析应生效的 selectedModel。
// 优先采用会话自身持久化的 "provider:model"（用户曾为该会话切过模型）；
// 否则回落到当前默认模型，避免使用上一个会话残留的 selectedModel 导致模型串台。
export function resolveSessionSelectedModel(
  sessionModel: string | null | undefined,
  defaultModel: string,
): string {
  if (sessionModel && sessionModel.includes(":") && sessionModel !== "unknown") {
    return sessionModel;
  }
  return defaultModel || "";
}
