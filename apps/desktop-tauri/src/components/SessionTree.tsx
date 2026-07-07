import { useEffect, useState } from "react";
import { ArrowUp, ArrowDown, GitBranch, Loader2, Inbox, AlertCircle } from "lucide-react";
import * as api from "../api";

interface SessionNode {
  id: string;
  title: string;
  has_compression: boolean;
}

interface LineageData {
  ancestors: SessionNode[];
  children: SessionNode[];
  current: SessionNode;
}

export function SessionTree({ sessionId }: { sessionId: string }) {
  const [data, setData] = useState<LineageData | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError(null);
    api.getSessionLineage(sessionId).then(
      (d) => {
        if (!cancelled) {
          setData(d);
          setLoading(false);
        }
      },
      (e) => {
        if (!cancelled) {
          // Round 6, 2026-07-06: 404 不再硬塞到 UI, 改为标记为"无谱系"
          // (empty state) 而不是 error state. 后端 404 表示该 session
          // 还没建立 lineage 关系 — 不是真正的失败. 其他错误保留 error.
          const msg = String(e);
          const isNotFound = /404|not\s*found|doesn.?t exist/i.test(msg);
          if (isNotFound) {
            setData({ ancestors: [], children: [], current: { id: sessionId, title: "", has_compression: false } });
            setError(null);
          } else {
            setError(msg);
          }
          setLoading(false);
        }
      },
    );
    return () => {
      cancelled = true;
    };
  }, [sessionId]);

  if (loading)
    return <PanelState kind="loading" title="加载会话谱系…" />;

  if (error)
    return <PanelState kind="error" title="加载失败" body={error} />;

  if (!data) return null;

  const hasLineage =
    data.ancestors.length > 0 || data.children.length > 0;

  if (!hasLineage)
    return <PanelState kind="empty" title="暂无谱系信息" body="该会话没有父级或子会话。" />;

  const nodeLabel = (n: SessionNode, highlight: boolean) => (
    <span
      className={`truncate text-[11px] ${
        highlight ? "text-accent font-semibold" : "text-text-secondary"
      }`}
    >
      {n.title || n.id.slice(0, 12)}
      {n.has_compression && (
        <span className="ml-1 px-1 py-px rounded text-[9px] bg-bg-secondary text-text-faint">
          压缩
        </span>
      )}
    </span>
  );

  return (
    <div className="py-2 px-1">
      <div className="flex items-center gap-1.5 mb-2 text-[10px] text-text-faint">
        <GitBranch size={11} />
        会话谱系
      </div>

      {/* Ancestors — walk upward */}
      {data.ancestors.length > 0 && (
        <div className="mb-1.5">
          <div className="flex items-center gap-1 text-[10px] text-text-faint mb-1">
            <ArrowUp size={10} />
            父级 ({data.ancestors.length})
          </div>
          <div className="space-y-0.5 border-l border-border-primary ml-1 pl-3">
            {data.ancestors.map((n) => (
              <div key={n.id} className="truncate">
                {nodeLabel(n, false)}
              </div>
            ))}
          </div>
        </div>
      )}

      {/* Current — highlighted */}
      <div className="border-l-2 border-accent ml-1 pl-3 py-1 mb-1.5">
        {nodeLabel(data.current, true)}
      </div>

      {/* Children */}
      {data.children.length > 0 && (
        <div>
          <div className="flex items-center gap-1 text-[10px] text-text-faint mb-1">
            <ArrowDown size={10} />
            子会话 ({data.children.length})
          </div>
          <div className="space-y-0.5 border-l border-border-primary ml-1 pl-3">
            {data.children.map((n) => (
              <div key={n.id} className="truncate">
                {nodeLabel(n, false)}
              </div>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}

/**
 * PanelState — 三态统一组件 (loading / empty / error).
 * Round 6, 2026-07-06: 提取出来取代裸 <Spin> / 红色文本, 让折叠面板
 * 内部的 loading/empty/error 都有明确的视觉规范.
 *
 * - loading: Loader2 spinning + muted text
 * - empty:   Inbox icon + 中性文字 + 副文案
 * - error:   AlertCircle icon + 文字 + 副文案 (第一行截断 120 chars 防 stack dump)
 */
function PanelState({
  kind,
  title,
  body,
}: {
  kind: "loading" | "empty" | "error";
  title: string;
  body?: string;
}) {
  const Icon = kind === "loading" ? Loader2 : kind === "empty" ? Inbox : AlertCircle;
  const cls =
    kind === "error"
      ? "panel-state--error"
      : kind === "loading"
      ? "panel-state--loading"
      : "panel-state--empty";
  return (
    <div className={`panel-state ${cls}`}>
      <Icon
        size={14}
        className={`shrink-0 ${kind === "loading" ? "animate-spin" : ""}`}
      />
      <div className="min-w-0">
        <div className="panel-state-title">{title}</div>
        {body && <div className="panel-state-body">{body.slice(0, 120)}</div>}
      </div>
    </div>
  );
}
