// components/settings/MemoryTextPanel.tsx
//
// 全局记忆文本：MEMORY.md（长期记忆文件）与 USER.md（用户画像），
// 即 /api/persona/memory 与 /api/persona/user 的全文编辑器。
// 说明文案点出与"记忆图谱"的关系：这里是最早的记忆通道，内容
// 会被注入系统提示；图谱里还能看到会话级与自动提取的记忆。

import { useCallback, useEffect, useState } from "react";
import { Loader2, Save } from "lucide-react";
import * as api from "../../api";
import { Button, SkeletonCard } from "../ui";

interface TextMemoryProps {
  title: string;
  description: string;
  load: () => Promise<api.PersonaTextFile>;
  save: (content: string) => Promise<void>;
}

function TextMemoryEditor({ title, description, load, save }: TextMemoryProps) {
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
      .catch((e) => alive && setNotice(String(e)))
      .finally(() => alive && setLoading(false));
    return () => { alive = false; };
  }, [load]);

  const onSave = async () => {
    setSaving(true);
    setNotice(null);
    try {
      await save(content);
      setDirty(false);
      setNotice("已保存，下一次对话生效。");
    } catch (e) {
      setNotice(e instanceof Error ? e.message : String(e));
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="rounded-lg border border-[var(--color-border-primary)] bg-[var(--color-bg-card)] p-3">
      <div className="flex items-center gap-2">
        <h3 className="text-[length:var(--text-ui)] font-medium text-[var(--color-text-primary)]">{title}</h3>
        {path && (
          <span className="truncate text-[length:var(--text-meta)] text-[var(--color-text-muted)]" title={path}>
            {path}
          </span>
        )}
        <Button variant="primary" size="sm" onClick={onSave} disabled={saving || loading || !dirty} className="ml-auto">
          {saving ? <Loader2 className="size-3.5 animate-spin" /> : <Save className="size-3.5" />} 保存
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
          className="mt-2 h-[260px] w-full resize-y rounded-md border border-[var(--color-border-primary)] bg-[var(--color-bg-tertiary)] p-3 font-mono text-[length:var(--text-meta)] leading-relaxed text-[var(--color-text-primary)] outline-none focus:border-[var(--color-accent)]"
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
        description="agent 通过 memory 工具显式写入的记忆文件，会注入系统提示。桌面私聊默认按会话隔离写入，会话结束时自动晋升到记忆库；这里的全局文件对所有会话可见。"
        load={loadMemory}
        save={api.savePersonaMemory}
      />
      <TextMemoryEditor
        title="USER.md（用户画像）"
        description="对用户偏好的结构化画像，随系统提示注入。"
        load={loadUser}
        save={api.savePersonaUser}
      />
    </div>
  );
}
