import { useEffect, useState } from "react";
import { ArrowUp, ArrowDown, GitBranch, Loader2 } from "lucide-react";
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
          setError(String(e));
          setLoading(false);
        }
      },
    );
    return () => {
      cancelled = true;
    };
  }, [sessionId]);

  if (loading)
    return (
      <div className="py-4 text-center text-text-faint text-[11px]">
        <Loader2 size={12} className="inline animate-spin mr-1" />
        加载会话谱系…
      </div>
    );

  if (error)
    return (
      <div className="py-2 text-center text-error text-[11px]">
        加载失败: {error}
      </div>
    );

  if (!data) return null;

  const hasLineage =
    data.ancestors.length > 0 || data.children.length > 0;

  if (!hasLineage)
    return (
      <div className="py-3 text-center text-text-faint text-[11px]">
        暂无谱系信息
      </div>
    );

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
