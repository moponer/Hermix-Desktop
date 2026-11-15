// 助手消息底部的操作行：复制 / 赞 / 踩 / 重新生成 / 时间戳。
//
// 2026-10-01 起替换原先只有「复制」一个按钮的 `MessageCopyAction`。
//
// ⚠ 关于赞 / 踩的诚实说明：项目里**没有** feedback 接口（后端 `ws.rs`
// 里那几处 "feedback" 是审批拒绝时的备注文案，与此无关）。所以这里
// 做的两个按钮**不会把评价发到任何地方**，只写进 localStorage 作为
// 本机记录（刷新后仍在，可改可撤）。
//
// 为什么不干脆不做：两个点了没反应的按钮比没有更糟 —— 用户以为反馈
// 提交了也没有。本地记录至少是「真的记下了」，而且不假装上报。要让它
// 真的上报是另一件事（需要新端点 + 存储），不在本次范围内。
import { useEffect, useState } from "react";
import {
  Check, Copy, ThumbsUp, ThumbsDown, RefreshCw, Quote,
} from "lucide-react";
import { useSafeTimeout } from "../hooks/useSafeTimeout";
import { injectIntoComposer } from "../lib/composerInjection";
import { storage, STORAGE_KEYS } from "../utils/storageCompat";
import { COPIED, COPY_MESSAGE } from "./strings";

type Vote = "up" | "down";
const VOTE_KEY = STORAGE_KEYS.messageVotes;

/** 读全部投票记录。坏数据直接当空，不让它把整个消息列表带崩。 */
function readVotes(): Record<string, Vote> {
  try {
    const raw = storage.getItem(VOTE_KEY);
    if (!raw) return {};
    const v = JSON.parse(raw);
    return v && typeof v === "object" ? (v as Record<string, Vote>) : {};
  } catch {
    return {};
  }
}

function writeVote(messageId: string, vote: Vote | null) {
  try {
    const all = readVotes();
    if (vote) all[messageId] = vote;
    else delete all[messageId];
    storage.setItem(VOTE_KEY, JSON.stringify(all));
  } catch {
    // 隐私模式 / 配额满：静默失败，UI 已经给了即时反馈，不阻塞使用
  }
}

/** `22:18` 这样的本地时刻。参考界面就是这种「几点几分」而非相对时间。 */
function formatClock(iso?: string): string {
  if (!iso) return "";
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "";
  return `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
}

export function MessageActions({
  message,
  className,
  onRetry,
}: {
  message: { id: string; content: string; timestamp: string };
  className: string;
  /** 重新生成。**仅有**流式轮次才接得上（重试是「再发一次上一条」，
   *  与具体某条助手消息无关），所以父级可能不传 —— 不传就不渲染
   *  那个按钮，而不是渲染一个点了没反应的。 */
  onRetry?: () => void;
}) {
  const safeSetTimeout = useSafeTimeout();
  const [copied, setCopied] = useState(false);
  const [vote, setVote] = useState<Vote | null>(null);

  // 挂载后从 localStorage 恢复；用 id 作依赖，换会话时重新读。
  useEffect(() => {
    setVote(readVotes()[message.id] ?? null);
  }, [message.id]);

  // R3-3（2026-10-16）：空正文（纯工具轮）不再整行 return null ——
  // CSS 侧 `.message-copy-row` 常驻 min-height:24px，整行消失会让
  // 消息间距塌陷。保留行容器，只隐藏对空内容无意义的动作（复制/赞踩），
  // 重新生成与时间戳仍然可用。
  const hasContent = !!message.content.trim();

  const copyMessage = async () => {
    try {
      await navigator.clipboard.writeText(message.content);
      setCopied(true);
      safeSetTimeout(() => setCopied(false), 1500);
    } catch {
      setCopied(false);
    }
  };

  const toggle = (v: Vote) => {
    const next = vote === v ? null : v;
    setVote(next);
    writeVote(message.id, next);
  };

  const clock = formatClock(message.timestamp);

  return (
    <div className={className}>
      {hasContent && (
        <button
          type="button"
          onClick={copyMessage}
          className="message-action-btn"
          title={COPY_MESSAGE}
          aria-label={COPY_MESSAGE}
        >
          {copied ? <Check className="icon-md" /> : <Copy className="icon-md" />}
          <span>{copied ? COPIED : "复制"}</span>
        </button>
      )}

      {hasContent && (
        <button
          type="button"
          onClick={() =>
            injectIntoComposer({
              source: "quote",
              // 只引前 12 行：整段长回答塞进输入框会把用户自己写的话顶走。
              text: message.content
                .split("\n")
                .slice(0, 12)
                .map((line) => `> ${line}`)
                .join("\n") + "\n\n",
              mode: "at-caret",
            })
          }
          className="message-action-btn"
          title="引用到输入框"
          aria-label="引用到输入框"
        >
          <Quote className="icon-md" />
          <span>引用</span>
        </button>
      )}

      {/* 赞 / 踩 —— 本机记录，不上报。见文件头说明。空正文无意义，隐藏。 */}
      {hasContent && (
        <>
          <button
            type="button"
            onClick={() => toggle("up")}
            className="message-action-btn message-action-icon"
            data-active={vote === "up" ? "true" : undefined}
            title="这条回答有帮助（仅记录在本机）"
            aria-label="有帮助"
            aria-pressed={vote === "up"}
          >
            <ThumbsUp className="icon-md" />
          </button>
          <button
            type="button"
            onClick={() => toggle("down")}
            className="message-action-btn message-action-icon"
            data-active={vote === "down" ? "true" : undefined}
            title="这条回答没帮上忙（仅记录在本机）"
            aria-label="没帮上忙"
            aria-pressed={vote === "down"}
          >
            <ThumbsDown className="icon-md" />
          </button>
        </>
      )}

      {onRetry && (
        <button
          type="button"
          onClick={onRetry}
          className="message-action-btn message-action-icon"
          title="重新生成"
          aria-label="重新生成"
        >
          <RefreshCw className="icon-md" />
        </button>
      )}

      {/* 时刻与其它动作同进退：空正文不渲染操作行，clock 也不例外。
          （此前 clock 漏了 hasContent 守卫，空正文消息仍挂一个时间戳。） */}
      {hasContent && clock && (
        <span className="message-action-clock tabular-nums">{clock}</span>
      )}
    </div>
  );
}
