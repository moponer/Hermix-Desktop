// components/ReviewTab.tsx
//
// 「审查」视图 —— 聚合本会话所有 `file_change` 事件，按文件归并后列出，
// 点任意一行开一个预览标签。
//
// 数据来源：消息的 `execution_timeline` 里 type === "file_change" 的事件，
// 由 App.tsx 的 onFileChange 写入（后端权威路径，不经正则猜测）。
//
// **能力边界（先说清，免得误以为这是 diff 审查）**：后端当前只在
// file_change 事件里带 `file_path` / `change_type` / `file_summary`，
// **不带 diff 正文**。所以这里能回答"这次动了哪些文件、怎么动的"，
// 回答不了"每一行改成了什么"。要真 diff 需要后端补字段，那是独立改动。
// 因此本视图刻意叫「变更」而不是「Diff」——名字要说实话。

import { useMemo } from "react";
import { FilePlus2, FilePenLine, FileX2, FileClock } from "lucide-react";
import { clsx } from "clsx";
import type { ChatMessage, ExecutionTimelineEvent } from "../types";
import { canonicalPath } from "../utils/artifacts";
import { FileTypeIcon } from "@hermix/preview/FileTypeIcon";

type ChangeType = NonNullable<ExecutionTimelineEvent["change_type"]>;

const CHANGE_META: Record<ChangeType, { label: string; Icon: typeof FilePenLine }> = {
  created: { label: "新建", Icon: FilePlus2 },
  modified: { label: "修改", Icon: FilePenLine },
  deleted: { label: "删除", Icon: FileX2 },
  renamed: { label: "重命名", Icon: FileClock },
};

interface ChangedFile {
  path: string;
  name: string;
  /** 同一文件可能被多次改动（改了又改），保留最后一次的类型与时间。 */
  changeType: ChangeType;
  summary?: string;
  at: string;
  /** 被改动次数 —— 编码时"改了三遍同一个文件"是需要知道的信号。 */
  hits: number;
}

function collect(messages: ChatMessage[] | null | undefined): ChangedFile[] {
  const byPath = new Map<string, ChangedFile>();
  for (const m of messages ?? []) {
    for (const ev of m.execution_timeline ?? []) {
      if (ev.type !== "file_change" || !ev.file_path) continue;
      const key = canonicalPath(ev.file_path);
      const name = ev.file_path.split(/[\\/]/).pop() ?? ev.file_path;
      const prev = byPath.get(key);
      if (prev) {
        prev.hits += 1;
        prev.changeType = ev.change_type ?? prev.changeType;
        prev.summary = ev.file_summary ?? prev.summary;
        prev.at = ev.created_at ?? prev.at;
        continue;
      }
      byPath.set(key, {
        path: ev.file_path,
        name,
        changeType: ev.change_type ?? "modified",
        summary: ev.file_summary,
        at: ev.created_at ?? m.timestamp,
        hits: 1,
      });
    }
  }
  // 最近的排前面 —— 审查时刚改完的那个最可能是你要看的。
  return Array.from(byPath.values()).sort((a, b) => (a.at < b.at ? 1 : -1));
}

export function ReviewTab({
  messages,
  onOpenFile,
}: {
  messages: ChatMessage[] | null | undefined;
  onOpenFile: (path: string, name: string) => void;
}) {
  const files = useMemo(() => collect(messages), [messages]);

  if (files.length === 0) {
    return (
      <div className="review-empty">
        <FileClock className="icon-lg" />
        <p className="review-empty__title">本次会话还没有文件变更</p>
        <p className="review-empty__hint">
          当助手写入、修改或删除文件时，这里会按文件列出改动，点任意一行可在预览栏打开。
        </p>
      </div>
    );
  }

  return (
    <div className="review-list">
      <div className="review-list__head">
        <span>{files.length} 个文件</span>
        <span className="cs-spacer" />
        <span className="text-text-faint">仅路径与类型，无 diff 正文</span>
      </div>
      {files.map((f) => {
        const { label, Icon } = CHANGE_META[f.changeType];
        return (
          <button
            key={f.path}
            type="button"
            className="review-row"
            onClick={() => onOpenFile(f.path, f.name)}
            title={f.path}
          >
            <FileTypeIcon name={f.name} size={16} />
            <span className="review-row__name truncate">{f.name}</span>
            {f.hits > 1 && <span className="review-row__hits">×{f.hits}</span>}
            <span className={clsx("review-row__tag", `is-${f.changeType}`)}>
              <Icon className="icon-sm" />
              {label}
            </span>
          </button>
        );
      })}
    </div>
  );
}
