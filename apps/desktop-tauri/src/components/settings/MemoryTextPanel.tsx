// components/settings/MemoryTextPanel.tsx
//
// 全局记忆文本：MEMORY.md（长期记忆文件）与 USER.md（用户画像），
// 即 /api/persona/memory 与 /api/persona/user 的全文编辑器。
// 说明文案点出与"记忆图谱"的关系：这里是最早的记忆通道，内容
// 会被注入系统提示；图谱里还能看到会话级与自动提取的记忆。

import { useCallback, useEffect, useState, type ReactNode } from "react";
import { Loader2, Save, FileText, Users } from "lucide-react";
import * as api from "../../api";
import { Button, IconTile, SkeletonCard } from "../ui";
import { LOAD_FAILED, SAVED, SAVING, SAVE, SAVE_FAILED, failure } from "../strings";

interface TextMemoryProps {
  title: string;
  description: string;
  /** 标题左侧的实心色块图标（身份色）。 */
  icon?: ReactNode;
  load: () => Promise<api.PersonaTextFile>;
  save: (content: string) => Promise<void>;
}

function TextMemoryEditor({ title, description, icon, load, save }: TextMemoryProps) {
  const [content, setContent] = useState("");
  const [path, setPath] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [dirty, setDirty] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    load()
      .then((file) => {
        if (!alive) return;
        setContent(file.content ?? "");
        setPath(file.path);
      })
      .catch((e) => {
        if (!alive) return;
        const msg = e instanceof Error ? e.message : String(e);
        setNotice(failure(LOAD_FAILED, "请重试。", msg));
      })
      .finally(() => alive && setLoading(false));
    return () => { alive = false; };
  }, [load]);

  const onSave = async () => {
    setSaving(true);
    setNotice(null);
    try {
      await save(content);
      setDirty(false);
      setNotice(SAVED);
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      setNotice(failure(SAVE_FAILED, "请重试。", msg));
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="rounded-lg bg-[var(--color-bg-card)] shadow-[var(--shadow-paper)] p-3">
      <div className="flex items-center gap-2.5">
        {icon && <span className="shrink-0">{icon}</span>}
        <h3 className="text-[length:var(--text-ui)] font-medium text-[var(--color-text-primary)]">{title}</h3>
        {path && (
          <span className="truncate text-[length:var(--text-meta)] text-[var(--color-text-muted)]" title={path}>
            {path}
          </span>
        )}
        <Button variant="primary" size="sm" onClick={onSave} disabled={saving || loading || !dirty} className="ml-auto">
          {saving ? <Loader2 className="size-3.5 animate-spin" /> : <Save className="size-3.5" />}
          {saving ? SAVING : SAVE}
        </Button>
      </div>
      <p className="mt-1 text-[length:var(--text-meta)] text-[var(--color-text-muted)]">{description}</p>
      {loading ? (
        <SkeletonCard lines={4} />
      ) : (
        <textarea
          value={content}
          onChange={(e) => { setContent(e.target.value); setDirty(true); }}
          spellCheck={false}
          className="mt-2 h-[260px] w-full resize-y rounded-[var(--radius-md)] bg-[var(--color-bg-tertiary)] p-3 font-mono text-[length:var(--text-meta)] leading-relaxed text-[var(--color-text-primary)] outline-none transition-shadow focus-visible:ring-2 focus-visible:ring-[var(--color-accent)]/40"
        />
      )}
      {notice && <p className="mt-1 text-[length:var(--text-meta)] text-[var(--color-text-secondary)]">{notice}</p>}
    </div>
  );
}

export function MemoryTextPanel() {
  const loadMemory = useCallback(() => api.getPersonaMemory(), []);
  const loadUser = useCallback(() => api.getPersonaUser(), []);
  return (
    <div className="space-y-3">
      <TextMemoryEditor
        title="MEMORY.md（全局长期记忆）"
        description="对所有会话可见，注入系统提示；保存后下一次对话生效。"
        icon={<IconTile icon={<FileText className="icon-md" />} tone="green" />}
        load={loadMemory}
        save={api.savePersonaMemory}
      />
      <TextMemoryEditor
        title="USER.md（用户画像）"
        description="用户偏好画像，注入系统提示；保存后下一次对话生效。"
        icon={<IconTile icon={<Users className="icon-md" />} tone="rose" />}
        load={loadUser}
        save={api.savePersonaUser}
      />
    </div>
  );
}
