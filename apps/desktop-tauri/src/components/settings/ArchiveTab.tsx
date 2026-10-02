import { useCallback, useEffect, useState } from "react";
import { ArchiveRestore, Loader2 } from "lucide-react";

import { getSessions } from "../../api/rest";
import { usePinnedArchived } from "../../hooks/usePinnedArchived";
import { SettingsGroup, SettingsRow } from "../ui";

interface ArchivedSession {
  id: string;
  title: string;
  updated: string;
}

/**
 * 已归档会话。
 *
 * 归档是**纯本地视图状态**（`hermix.desktop.archived.sessions` 存 id 集合），
 * 会话本体仍在后端。所以这里的"取消归档"只是把 id 从集合里摘掉，
 * 不碰后端数据——真删会话是侧边栏的删除，两件事刻意不混。
 *
 * 标题靠 `getSessions()` 反查。取不到的 id（会话已在后端被删）照旧列出，
 * 只显示 id：静默隐藏会让用户以为归档丢了。
 */
export function ArchiveTab() {
  const { archivedIds, toggleArchive } = usePinnedArchived();
  const [rows, setRows] = useState<ArchivedSession[] | null>(null);

  const load = useCallback(async () => {
    const ids = archivedIds("session");
    if (ids.length === 0) {
      setRows([]);
      return;
    }
    const sessions = await getSessions().catch(() => []);
    const byId = new Map(sessions.map((s) => [s.id, s]));
    setRows(
      ids.map((id) => {
        const s = byId.get(id);
        return {
          id,
          title: s?.title?.trim() || "（无标题会话）",
          updated: s?.updated_at ? new Date(s.updated_at).toLocaleString() : "—",
        };
      }),
    );
  }, [archivedIds]);

  useEffect(() => {
    void load();
  }, [load]);

  if (rows === null) {
    return (
      <div className="flex items-center gap-2 py-6 text-[length:var(--text-ui)] text-[var(--color-text-muted)]">
        <Loader2 className="icon-sm animate-spin" /> 正在读取归档…
      </div>
    );
  }

  return (
    <div className="space-y-6">
      <header>
        <h2 className="text-[length:var(--text-body)] font-medium text-[var(--color-text-primary)]">
          已归档
        </h2>
        <p className="mt-1 text-[length:var(--text-meta)] text-[var(--color-text-muted)]">
          归档只是把会话从侧边栏收起来，数据仍在本机。取消归档即可回到列表。
        </p>
      </header>

      {rows.length === 0 ? (
        <div className="rounded-[var(--radius-lg)] border border-[var(--color-border-primary)] bg-[var(--color-bg-card)] px-4 py-10 text-center text-[length:var(--text-ui)] text-[var(--color-text-muted)]">
          还没有归档的会话。在侧边栏会话上右键即可归档。
        </div>
      ) : (
        <SettingsGroup>
          {rows.map((r) => (
            <SettingsRow
              key={r.id}
              title={r.title}
              description={`最近更新 ${r.updated}`}
              control={
                <button
                  onClick={() => { toggleArchive(r.id, "session"); void load(); }}
                  className="flex h-7 items-center gap-1.5 rounded-lg border border-[var(--color-border-secondary)] px-2 text-[length:var(--text-meta)] text-[var(--color-text-secondary)] transition-colors hover:border-[var(--color-border-primary)] hover:text-[var(--color-text-primary)]"
                >
                  <ArchiveRestore className="icon-sm" /> 取消归档
                </button>
              }
            />
          ))}
        </SettingsGroup>
      )}
    </div>
  );
}
