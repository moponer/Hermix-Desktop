// 系统资源管理共用的小工具：格式化、动画数字、定时器。
//
// 抽出来是因为三个页面都要用：放在任一页面文件里都会变成"别人 import
// 一个页面组件的内部函数"，删页面时连带删掉别人的依赖。

import { useEffect, useRef, useState } from "react";

export function formatBytes(bytes: number): string {
  if (!bytes) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value >= 100 || unit === 0 ? Math.round(value) : value.toFixed(1)} ${units[unit]}`;
}

export function formatTime(unixSeconds: number): string {
  if (!unixSeconds) return "未知";
  return new Date(unixSeconds * 1000).toLocaleString();
}

export function formatUptime(seconds: number): string {
  if (!seconds) return "—";
  const days = Math.floor(seconds / 86400);
  const hours = Math.floor((seconds % 86400) / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  if (days > 0) return `${days} 天 ${hours} 小时`;
  if (hours > 0) return `${hours} 小时 ${minutes} 分`;
  return `${minutes} 分钟`;
}

/** 相对时间：索引页要显示"多久没同步了"。 */
export function formatAgo(unixSeconds?: number | null): string {
  if (!unixSeconds) return "从未";
  const seconds = Math.max(0, Math.floor(Date.now() / 1000) - unixSeconds);
  if (seconds < 60) return "刚刚";
  if (seconds < 3600) return `${Math.floor(seconds / 60)} 分钟前`;
  if (seconds < 86400) return `${Math.floor(seconds / 3600)} 小时前`;
  return `${Math.floor(seconds / 86400)} 天前`;
}

/** 文件多久没动：清理页显示"多久没用"。 */
export function formatIdle(modified: number): string {
  if (!modified) return "时间未知";
  const days = Math.floor((Date.now() / 1000 - modified) / 86400);
  if (days >= 365) return `${Math.floor(days / 365)} 年未修改`;
  if (days >= 30) return `${Math.floor(days / 30)} 个月未修改`;
  if (days >= 1) return `${days} 天未修改`;
  return "今天修改";
}

export function errorText(error: unknown): string {
  if (typeof error === "string") return error;
  if (error instanceof Error) return error.message;
  return String(error);
}

/** 数值平滑过渡：大数字跳变（1.2GB → 3.4GB）会显得很"炸"，插值一下。 */
export function useAnimatedNumber(target: number, durationMs = 420): number {
  const [value, setValue] = useState(target);
  const fromRef = useRef(target);
  const startRef = useRef(0);
  const frameRef = useRef(0);

  useEffect(() => {
    cancelAnimationFrame(frameRef.current);
    fromRef.current = value;
    startRef.current = performance.now();
    const tick = (now: number) => {
      const t = Math.min(1, (now - startRef.current) / durationMs);
      // easeOutCubic：起步快、收尾稳，读数不会"冲过头"。
      const eased = 1 - Math.pow(1 - t, 3);
      setValue(fromRef.current + (target - fromRef.current) * eased);
      if (t < 1) frameRef.current = requestAnimationFrame(tick);
    };
    frameRef.current = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(frameRef.current);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [target, durationMs]);

  return value;
}

/** 固定间隔刷新：只在面板可见时跑，避免后台白烧 CPU。 */
export function useInterval(callback: () => void, delayMs: number | null) {
  const saved = useRef(callback);
  saved.current = callback;
  useEffect(() => {
    if (delayMs === null) return;
    const timer = window.setInterval(() => saved.current(), delayMs);
    return () => window.clearInterval(timer);
  }, [delayMs]);
}

export async function copyText(text: string) {
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    // WebView 里剪贴板权限缺失时静默失败：不为此弹一个错误框。
  }
}
