// 工作台空态。
//
// 一个工作台都没有时的起步界面（工作台对话重设计方案 C 定案版）：
// 一个 input + 一个 button，Enter 即建，零教学。契约细节由 agent 侧的
// workbench skill 负责，宿主不再堆"复制提示词"的教程块。
// 错误走 input 下方 inline（不堆在页面底部堆一坨），与 toast 双通道。
import { useState } from "react";
import { toastError } from "../hooks/useToast";
import type { WorkbenchManifest } from "../workbench/api";

interface WorkbenchEmptyProps {
  items: WorkbenchManifest[];
  onCreate: (name: string) => Promise<void>;
  error: string | null;
}

export function WorkbenchEmpty({ onCreate, error }: WorkbenchEmptyProps) {
  const [name, setName] = useState("");
  const [creating, setCreating] = useState(false);

  const submit = async () => {
    const trimmed = name.trim();
    if (!trimmed || creating) return;
    setCreating(true);
    try {
      await onCreate(trimmed);
    } catch (e) {
      // onCreate 内部已 setError，这里补 toast 保证用户一定看得到。
      toastError(e instanceof Error ? e.message : String(e));
    } finally {
      setCreating(false);
    }
  };

  return (
    <div className="wb-empty">
      <h2 className="wb-empty-title">工作台</h2>

      <div className="wb-empty-create">
        <input
          className="wb-input"
          value={name}
          placeholder="例如：安全管理工作台"
          autoFocus
          onChange={(e) => setName(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") void submit();
          }}
        />
        <button
          type="button"
          className="wb-btn"
          disabled={!name.trim() || creating}
          onClick={() => void submit()}
        >
          新建
        </button>
      </div>
      {error && <p className="wb-error-inline">{error}</p>}
    </div>
  );
}
