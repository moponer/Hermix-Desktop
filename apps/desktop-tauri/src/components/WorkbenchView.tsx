// 工作台入口：列出已有工作台，新建时由宿主预生成骨架（AI 只在骨架里填）。
// 打开工作台会切到它绑定的持久会话，右侧对话栏因此天然延续上下文。
import { useCallback, useEffect, useState, type ReactNode } from "react";
import { createSession } from "../api/rest";
import { workbenchCreate, workbenchEnsureSession, workbenchList, type WorkbenchManifest } from "../workbench/api";
import { WorkbenchShell } from "./WorkbenchShell";

interface WorkbenchViewProps {
  chatNode: ReactNode;
  onSelectSession: (id: string) => void;
}

export function WorkbenchView({ chatNode, onSelectSession }: WorkbenchViewProps) {
  const [items, setItems] = useState<WorkbenchManifest[]>([]);
  const [active, setActive] = useState<WorkbenchManifest | null>(null);
  const [draftName, setDraftName] = useState("");
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      setItems(await workbenchList());
    } catch (e) {
      setError(String(e));
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const create = useCallback(async () => {
    const name = draftName.trim();
    if (!name) return;
    try {
      const manifest = await workbenchCreate(name);
      setDraftName("");
      await refresh();
      await open(manifest);
    } catch (e) {
      setError(String(e));
    }
  }, [draftName, refresh]);

  /**
   * 打开工作台：先确保它有绑定会话，再切过去。
   *
   * 已绑定的工作台沿用原会话 —— 每次新建会让 skill 常驻漂到新会话，
   * 而画布用的是当前 active 会话，一错位 agent 就没了工作台 skill。
   */
  const open = useCallback(
    async (manifest: WorkbenchManifest) => {
      try {
        const fresh = manifest.session_id ?? (await createSession(manifest.name)).id;
        const sessionId = await workbenchEnsureSession(manifest.id, fresh);
        setActive({ ...manifest, session_id: sessionId });
        onSelectSession(sessionId);
        setError(null);
      } catch (e) {
        setError(String(e));
      }
    },
    [onSelectSession],
  );

  if (active) {
    return <WorkbenchShell manifest={active} chatNode={chatNode} />;
  }

  return (
    <div className="wb-list">
      <div className="wb-list-head">
        <h2 className="wb-list-title">工作台</h2>
      </div>
      <p className="wb-list-hint">
        说一句话就能建，界面与功能全部由对话生成；数据独立于代码永久保留。
      </p>
      <div className="wb-list-head">
        <input
          className="wb-input"
          value={draftName}
          placeholder="例如：安全管理工作台"
          onChange={(e) => setDraftName(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") void create();
          }}
        />
        <button type="button" className="wb-btn" onClick={() => void create()}>
          新建
        </button>
      </div>
      {error && <p className="wb-list-hint">{error}</p>}
      {items.map((wb) => (
        <div
          key={wb.id}
          className="wb-card-row"
          onClick={() => void open(wb)}
          role="button"
          tabIndex={0}
          onKeyDown={(e) => {
            if (e.key === "Enter") void open(wb);
          }}
        >
          <span className="wb-card-name">{wb.name}</span>
          <span className="wb-card-meta">{wb.session_id ? "已绑定会话" : "未绑定"}</span>
        </div>
      ))}
    </div>
  );
}
