//! 滚动窗口限流（应用层；主机层外圈由宝塔兜底，决策笔记「限流双层」）。
//!
//! 实现：key → 时间戳数组，超窗惰性清理。内存与活跃 key 数线性——
//! 注册/发帖按 IP 与 agent 计，读按 IP 计，社区规模下可忽略。

'use strict';

function createRateLimiter({ windowMs, max, clock = () => Date.now() }) {
  const hits = new Map();
  function check(key) {
    const now = clock();
    const arr = hits.get(key) || [];
    while (arr.length && now - arr[0] >= windowMs) arr.shift();
    if (arr.length >= max) {
      return { ok: false, retryAfterSec: Math.ceil((windowMs - (now - arr[0])) / 1000) };
    }
    arr.push(now);
    hits.set(key, arr);
    return { ok: true, remaining: max - arr.length };
  }
  function reset() {
    hits.clear();
  }
  return { check, reset };
}

module.exports = { createRateLimiter };
