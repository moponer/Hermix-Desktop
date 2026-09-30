// "刚刚 / N 秒前 / N 分钟前 / N 小时前 / N 天前" — within 7 days; falls back to locale date.
export function formatRelativeTime(iso: string | undefined | null, now: Date = new Date()): string {
  if (!iso) return "";
  const ts = new Date(iso);
  if (Number.isNaN(ts.getTime())) return "";
  const diffMs = now.getTime() - ts.getTime();
  const diffSec = Math.max(0, Math.round(diffMs / 1000));
  if (diffSec < 30) return "刚刚";
  if (diffSec < 60) return `${diffSec} 秒前`;
  const diffMin = Math.round(diffSec / 60);
  if (diffMin < 60) return `${diffMin} 分钟前`;
  const diffHr = Math.round(diffMin / 60);
  if (diffHr < 24) return `${diffHr} 小时前`;
  const diffDay = Math.round(diffHr / 24);
  if (diffDay < 7) return `${diffDay} 天前`;
  return ts.toLocaleDateString();
}

// 侧栏行内紧凑时间："刚刚 / Nm / Nh / Nd / M月d日"。
// 完整相对时间（"N 小时前"）在 130px 宽的侧栏行里会挤压标题，
// 这里把 7 天内的单位压到最短，更早的退化为短日期。
export function formatSidebarTime(iso: string | undefined | null, now: Date = new Date()): string {
  if (!iso) return "";
  const ts = new Date(iso);
  if (Number.isNaN(ts.getTime())) return "";
  const diffMs = now.getTime() - ts.getTime();
  const diffSec = Math.max(0, Math.round(diffMs / 1000));
  if (diffSec < 60) return "刚刚";
  const diffMin = Math.round(diffSec / 60);
  if (diffMin < 60) return `${diffMin}分钟前`;
  const diffHr = Math.floor(diffMin / 60);
  if (diffHr < 24) {
    const rest = diffMin % 60;
    // cc-haha 侧栏是「3天9小时前」这种中文相对时间，不是 3d/7h。
    return rest > 0 ? `${diffHr}小时${rest}分钟前` : `${diffHr}小时前`;
  }
  const diffDay = Math.floor(diffHr / 24);
  if (diffDay === 1) return "昨天";
  if (diffDay < 7) {
    const restHr = diffHr % 24;
    return restHr > 0 ? `${diffDay}天${restHr}小时前` : `${diffDay}天前`;
  }
  if (diffDay < 30) return `${diffDay}天前`;
  return `${ts.getMonth() + 1}月${ts.getDate()}日`;
}

// "Xms / X.Ys / Xm Ys".
export function formatDuration(ms: number): string {
  if (!Number.isFinite(ms) || ms < 0) return "";
  if (ms < 1000) return `${Math.round(ms)}ms`;
  const sec = ms / 1000;
  if (sec < 60) return `${sec.toFixed(sec < 10 ? 1 : 0)}s`;
  const m = Math.floor(sec / 60);
  const s = Math.round(sec - m * 60);
  return `${m}m ${s}s`;
}

/**
 * 会话是否来自 IM 远程通道。IM 会话的 id 是后端派生的 `im:{platform}:{user}`
 * （channels/session.rs::session_id）—— 靠 id 前缀就能在侧栏/聊天头部
 * 标出「这条是从 QQ/企业微信发来的」，不必给 Session 表加字段。
 */
export function imSourceOf(sessionId: string | null | undefined): "qq" | "wecom" | null {
  if (!sessionId) return null;
  if (sessionId.startsWith("im:qq:")) return "qq";
  if (sessionId.startsWith("im:wecom:")) return "wecom";
  return null;
}

export const IM_SOURCE_LABEL = { qq: "QQ", wecom: "企业微信" } as const;
