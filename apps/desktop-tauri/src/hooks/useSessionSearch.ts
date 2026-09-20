import { useMemo, useState } from "react";
import type { Session } from "../types";

/** 命中结果：会话本身 + 命中的那条消息（标题命中时为 null）。 */
export interface SessionSearchHit {
  session: Session;
  /** 命中的消息内容片段，用于展示摘要。标题命中时为空。 */
  snippet: string | null;
}

/**
 * 会话搜索。标题与消息内容都参与匹配，命中消息时给出片段。
 *
 * 抽成 hook 是因为它有两个消费方：独立的搜索页，以及侧栏"查看全部"
 * 弹窗（后者自带搜索框）。两处必须给出完全一致的结果，否则同一个
 * 关键词在不同入口搜出不同列表。
 *
 * 注意这里只搜 `session.messages`——即前端已加载的消息。未加载的
 * 历史消息不在范围内，调用方不要把空结果当成"不存在"。
 */
export function useSessionSearch(sessions: Session[]) {
  const [query, setQuery] = useState("");

  const trimmed = query.trim();

  const hits = useMemo<SessionSearchHit[]>(() => {
    if (!trimmed) return [];
    const q = trimmed.toLowerCase();
    const out: SessionSearchHit[] = [];
    for (const session of sessions) {
      const titleHit = session.title.toLowerCase().includes(q);
      const msg = session.messages.find((m) => m.content.toLowerCase().includes(q));
      // 标题或任一条消息命中即算命中；标题命中优先展示（无片段）。
      if (titleHit || msg) {
        out.push({
          session,
          snippet: titleHit ? null : (msg?.content.slice(0, 120) ?? null),
        });
      }
    }
    return out;
  }, [trimmed, sessions]);

  return { query, setQuery, trimmed, hits, isSearching: trimmed.length > 0 };
}
