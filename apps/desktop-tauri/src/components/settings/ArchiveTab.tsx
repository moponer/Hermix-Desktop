import { useCallback, useEffect, useState } from "react";
import { Archive, ArchiveRestore, Loader2 } from "lucide-react";

import { getSessions } from "../../api/rest";
import { usePinnedArchived } from "../../hooks/usePinnedArchived";
import { SettingsGroup, SettingsRow, IconTile } from "../ui";
import { LOADING } from "../strings";

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
      <div className="flex items-center gap-2 py-6 text-[length:var(--text-ui)] text-[var(--color-text-muted)]" aria-busy="true">
        <Loader2 className="icon-sm animate-spin" /> {LOADING}
      </div>
    );
  }

  return (
    <div className="space-y-6">
      {rows.length === 0 ? (
        <div className="rounded-[var(--radius-lg)] bg-[var(--color-bg-card)] px-4 py-10 shadow-[var(--shadow-paper)] text-center text-[length:var(--text-ui)] text-[var(--color-text-muted)]">
          还没有归档的会话。在侧边栏会话上右键即可归档。
        </div>
      ) : (
        <SettingsGroup>
          {rows.map((r) => (
            <SettingsRow
              key={r.id}
              icon={<IconTile icon={<Archive className="icon-md" />} tone="slate" />}
              title={r.title}
              description={`最近更新 ${r.updated}`}
              control={
                <button
                  onClick={() => { toggleArchive(r.id, "session"); void load(); }}
                  className="flex h-7 items-center gap-1.5 rounded-[var(--radius-md)] bg-[var(--color-bg-tertiary)] px-2 text-[length:var(--text-meta)] text-[var(--color-text-secondary)] transition-colors hover:bg-[var(--color-bg-quaternary)] hover:text-[var(--color-text-primary)]"
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
