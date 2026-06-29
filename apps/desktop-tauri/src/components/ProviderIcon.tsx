// ProviderIcon — 厂商 logo 渲染组件
//
// 数据源：@lobehub/icons
// - 1000+ AI / LLM 厂商的官方 SVG logo (OpenAI/Anthropic/Minimax/Qwen/Kimi/...)
// - LobeHub 维护，与官方站点配色一致
// - 纯 SVG forwardRef React 组件，无网络依赖，离线可用
// - Mono 用 currentColor → 容器 color 决定前景色，dark mode 自动反色
//
// ⚠️ v22 修复：
//   - minimax: null as any（硬编码齿轮） → 改用 lobehub Minimax Mono
//   - var(--pi-bg, rgba(0,0,0,0.04)) 软底（0 处 set 过） → inline 12% alpha 软底
//
// 解析规则：
//   minimax/openai/anthropic/deepseek/... → 对应 lobehub 组件
//   custom-*  / 未识别                   → 内置 Gear 图标

import React from "react";
import {
  OpenAI,
  Anthropic,
  DeepSeek,
  Qwen,
  Kimi,
  Moonshot,
  Zhipu,
  Volcengine,
  OpenRouter,
  SiliconCloud,
  GithubCopilot,
  NousResearch,
  Baichuan,
  Hunyuan,
  Spark,
  Tencent,
  Yi,
  AlibabaCloud,
  Minimax,
} from "@lobehub/icons";

/** 厂商品牌主色（用于软底计算） */
const BRAND_COLOR: Record<string, string> = {
  minimax: "#F23F5D",        // lobehub Minimax COLOR_PRIMARY
  openai: "#10A37F",         // OpenAI 官方绿
  anthropic: "#D97757",      // Anthropic 官方橙
  claude: "#D97757",
  deepseek: "#1E88E5",       // DeepSeek 蓝
  qwen: "#FF6A00",           // 阿里橙
  dashscope: "#FF6A00",
  alibaba: "#FF6A00",
  kimi: "#1F1F1F",           // 月之暗面
  moonshot: "#1F1F1F",
  zhipu: "#2563EB",          // 智谱蓝
  glm: "#2563EB",
  volcengine: "#FF4D4F",     // 火山红
  doubao: "#FF4D4F",
  openrouter: "#6366F1",     // OpenRouter 紫
  siliconflow: "#7B61FF",    // 硅基流动紫
  siliconcloud: "#7B61FF",
  copilot: "#1A1A1A",        // GitHub 黑
  github: "#1A1A1A",
  nous: "#7C3AED",           // Nous 紫
  nousresearch: "#7C3AED",
  baichuan: "#E53E3E",       // 百川红
  hunyuan: "#0052D9",        // 腾讯混元蓝
  spark: "#D33A2C",          // 讯飞红
  tencent: "#0052D9",
  yi: "#10B981",             // 零一万物绿
};

/** 渲染 fallback 图标 —— 自带简单齿轮 SVG */
function GearIcon({ size = 16, color = "currentColor" }: { size?: number; color?: string }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke={color}
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      style={{ display: "block" }}
      aria-hidden="true"
    >
      <path d="M12.22 2h-.44a2 2 0 0 0-2 2v.18a2 2 0 0 1-1 1.73l-.43.25a2 2 0 0 1-2 0l-.15-.08a2 2 0 0 0-2.73.73l-.22.38a2 2 0 0 0 .73 2.73l.15.1a2 2 0 0 1 1 1.72v.51a2 2 0 0 1-1 1.74l-.15.09a2 2 0 0 0-.73 2.73l.22.38a2 2 0 0 0 2.73.73l.15-.08a2 2 0 0 1 2 0l.43.25a2 2 0 0 1 1 1.73V20a2 2 0 0 0 2 2h.44a2 2 0 0 0 2-2v-.18a2 2 0 0 1 1-1.73l.43-.25a2 2 0 0 1 2 0l.15.08a2 2 0 0 0 2.73-.73l.22-.39a2 2 0 0 0-.73-2.73l-.15-.08a2 2 0 0 1-1-1.74v-.5a2 2 0 0 1 1-1.74l.15-.09a2 2 0 0 0 .73-2.73l-.22-.38a2 2 0 0 0-2.73-.73l-.15.08a2 2 0 0 1-2 0l-.43-.25a2 2 0 0 1-1-1.73V4a2 2 0 0 0-2-2z" />
      <circle cx="12" cy="12" r="3" />
    </svg>
  );
}

/** 厂商名 → lobehub 组件映射 */
const REGISTRY: Record<string, React.ComponentType<{ size?: string | number }>> = {
  // 主流通用
  openai: OpenAI,
  anthropic: Anthropic,
  claude: Anthropic,
  // 中国系
  deepseek: DeepSeek,
  qwen: Qwen,
  dashscope: AlibabaCloud,
  alibaba: AlibabaCloud,
  kimi: Kimi,
  moonshot: Moonshot,
  zhipu: Zhipu,
  glm: Zhipu,
  // 火山引擎
  volcengine: Volcengine,
  doubao: Volcengine,
  // 路由/转发
  openrouter: OpenRouter,
  // 硅基流动
  siliconflow: SiliconCloud,
  siliconcloud: SiliconCloud,
  // GitHub Copilot
  copilot: GithubCopilot,
  github: GithubCopilot,
  // Nous
  nous: NousResearch,
  nousresearch: NousResearch,
  // 其他
  baichuan: Baichuan,
  hunyuan: Hunyuan,
  spark: Spark,
  tencent: Tencent,
  yi: Yi,
  // 内部 minimax（v22 修复：不再走齿轮，用 lobehub 真 logo）
  minimax: Minimax,
};

/** 解析任意 provider id 到 lobehub key；找不到返 null（走 GearIcon） */
function resolveLobeKey(providerId: string): keyof typeof REGISTRY | null {
  if (!providerId) return null;
  const id = providerId.toLowerCase();
  // custom-* 走齿轮
  if (id.startsWith("custom-")) return null;
  // 精确匹配
  if (REGISTRY[id]) return id as keyof typeof REGISTRY;
  return null;
}

/** 12% alpha 软底（hex8） */
function softBg(color: string): string {
  // 支持 #RRGGBB → #RRGGBB1F
  if (color.startsWith("#") && color.length === 7) {
    return color + "1F";
  }
  return color;
}

export interface ProviderIconProps {
  providerId: string;
  /** 渲染尺寸（px）。默认 16。 */
  size?: number;
  /** 圆角半径（px）。默认 size/4。 */
  radius?: number;
  className?: string;
  title?: string;
}

/** 厂商 logo + 软色容器 */
export function ProviderIcon({ providerId, size = 16, radius, className, title }: ProviderIconProps) {
  const key = resolveLobeKey(providerId);
  const Component = key ? REGISTRY[key] : null;
  // 自定义厂商（齿轮）：灰色
  // 已知厂商：品牌主色
  const color = key
    ? (BRAND_COLOR[key] ?? "currentColor")
    : "#6B7280";
  const r = radius ?? Math.max(3, size / 4);
  const bgSoft = softBg(color);

  return (
    <span
      className={className}
      title={title ?? providerId}
      role="img"
      aria-label={`${providerId} provider`}
      style={{
        display: "inline-flex",
        alignItems: "center",
        justifyContent: "center",
        width: size,
        height: size,
        borderRadius: r,
        background: bgSoft,
        color: color,
        flexShrink: 0,
        userSelect: "none",
      } as React.CSSProperties}
    >
      {Component ? (
        <Component size={size} />
      ) : (
        <GearIcon size={size} color={color} />
      )}
    </span>
  );
}

/** 圆角矩形版 */
export function ProviderIconRounded(props: ProviderIconProps) {
  return <ProviderIcon {...props} radius={6} />;
}
