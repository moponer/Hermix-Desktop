// 工作台空态。
//
// 一个工作台都没有时，用户没有任何画布可看，所以这里必须承担「教他怎么
// 起步」的职责：给一段可以整段复制到对话里的提示词，让 agent 知道要按
// 工作台的规矩（骨架契约、网关能力、单文件上限）来生成，而不是随口聊。
//
// 提示词是宿主写的固定文案，不动态拼骨架细节 —— 契约细节由 agent 侧的
// workbench skill 负责，复制过去的这段话只解决「用户不知道怎么开口」。
import { useState } from "react";
import type { WorkbenchManifest } from "../workbench/api";

const PROMPT = `帮我在工作台上做一个「安全管理工作台」。

要求：
1. 用数据表存业务数据，不要写死在代码里。
2. 界面按我实际要干的事来设计，你先把要放哪些区域、每个区域干什么列出来，等我确认再动手。
3. 我需要能录入、查询、导出，其他功能你按安全管理这个场景自己补。`;

interface WorkbenchEmptyProps {
  items: WorkbenchManifest[];
  onCreate: (name: string) => Promise<void>;
  error: string | null;
}

export function WorkbenchEmpty({ onCreate, error }: WorkbenchEmptyProps) {
  const [name, setName] = useState("");
  const [copied, setCopied] = useState(false);

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(PROMPT);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 2000);
    } catch {
      // 剪贴板被拒（无焦点/权限）时不静默 —— 用户需要知道得手动选。
      setCopied(false);
    }
  };

  return (
    <div className="wb-empty">
      <h2 className="wb-empty-title">还没有工作台</h2>
      <p className="wb-empty-lead">
        工作台由对话生成：说清楚你要干什么，界面和数据就长出来了。数据独立于代码永久保留。
      </p>

      <div className="wb-empty-block">
        <div className="wb-empty-block-head">
          <span className="wb-empty-block-title">第一步：复制这段话到对话里</span>
          <button type="button" className="wb-btn" onClick={() => void copy()}>
            {copied ? "已复制" : "复制"}
          </button>
        </div>
        <pre className="wb-empty-prompt">{PROMPT}</pre>
      </div>

      <div className="wb-empty-block">
        <div className="wb-empty-block-head">
          <span className="wb-empty-block-title">第二步：给工作台起个名字</span>
        </div>
        <div className="wb-empty-create">
          <input
            className="wb-input"
            value={name}
            placeholder="例如：安全管理工作台"
            onChange={(e) => setName(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && name.trim()) void onCreate(name.trim());
            }}
          />
          <button
            type="button"
            className="wb-btn"
            disabled={!name.trim()}
            onClick={() => name.trim() && void onCreate(name.trim())}
          >
            新建
          </button>
        </div>
      </div>

      {error && <p className="wb-error">{error}</p>}
    </div>
  );
}
