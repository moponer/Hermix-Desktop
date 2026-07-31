/**
 * StreamingArtifactCard — 实时展示 LLM「正在写哪些文件 / 写到哪了」的
 * 产物级进度组件。
 *
 * 背景：用户反馈 LLM 写代码 / 写文档时，前端只有一段「正在生成…」的
 * 文字，看不到具体在写什么、写到哪一步，容易误以为卡死。现有
 * `StreamingBubble` 会把整段流式文本（含代码块）直接 Markdown 渲染，
 * 但那是「最终产物」视角；本组件补上「写的过程」视角：
 *   解析流式文本里的 fenced code block（` ```lang `），把每个块当成
 *   一个正在生成的「文件」，实时显示：
 *     - 文件名（从首行注释 `// backend.ts` / `# ui.tsx` 提取，否则按语言推断）
 *     - 语言标签
 *     - 生成中（转圈）/ 完成（✓）状态
 *     - 已写入字符数 + 进度条（按字符数估算，开写时脉冲动画）
 *
 * 只在流式态（isStreaming=true）渲染；流结束后由 `MessageBubble` 接管，
 * 不会出现重复卡片。与下方 MarkdownBody 流式渲染并存（卡片=进度概览，
 * 正文=实际内容），互不冲突。
 *
 * 解析对「流式中间态」健壮：未闭合的代码块（模型还在写）按 open 处理，
 * 闭合后按完成处理；多个块时各显示一行。
 */

import { memo, useMemo } from "react";
import { Box, Check, FileCode2, Loader2 } from "lucide-react";

interface Artifact {
  key: number;
  name: string;
  lang: string;
  open: boolean;
  charCount: number;
}

const CHAR_BUDGET = 2500; // 进度条估算基准：约 2500 字符视为「写满一格」

/** 从代码块首行提取文件名（注释风格）。 */
function extractName(firstLine: string): string | null {
  const m = firstLine.match(/^\s*(?:\/\/|#|;|--|rem|::)\s*([\w./\\-]+\.[A-Za-z0-9]+)/);
  if (m) return m[1];
  return null;
}

/** 无显式文件名时按语言/内容推断一个合理默认名。 */
function defaultName(lang: string, idx: number): string {
  if (lang === "html" || lang === "xml") return "index.html";
  if (lang && lang !== "text") return `代码片段.${lang}`;
  return `代码片段 ${idx + 1}`;
}

/** 判断这个围栏块是否值得显示为“正在生成的文件”。
 *  核心防御：LLM 回答里的普通代码块（```tsx / ```python …）不该被
 *  当成“正在生成的文件”卡片刷屏。只有「显式声明了文件名」的块才视为
 *  文件产物：
 *    - 首行有文件扩展名注释（// backend.ts、# ui.tsx、-- main.rs）
 *    - HTML 片段（<!doctype html|<html）
 *  其余代码块一律不显示此卡片，交由正文 MarkdownBody 正常渲染成代码块。
 *  真正的「后端正在写文件」进度由 ToolEventCard（write_file 工具事件）呈现。
 */
function looksLikeArtifact(_lang: string, firstLine: string): boolean {
  if (extractName(firstLine)) return true;
  if (/^<!doctype html|<html/i.test(firstLine)) return true;
  // 不再仅凭「非 text 语言」就当文件——那会把模型的最终答案代码刷成
  // 一堆「代码片段.tsx」卡片。
  return false;
}

function parseArtifacts(text: string): Artifact[] {
  const fenceRe = /```([A-Za-z0-9_+\-]*)\n?/g;
  const matches: { start: number; lang: string }[] = [];
  let mm: RegExpExecArray | null;
  while ((mm = fenceRe.exec(text)) !== null) {
    matches.push({ start: mm.index, lang: (mm[1] || "").trim() });
  }

  const out: Artifact[] = [];
  let counter = 0;
  let i = 0;
  while (i < matches.length) {
    const openStart = matches[i].start;
    const afterOpen = text.slice(openStart);
    const nl = afterOpen.indexOf("\n");
    const contentStart = openStart + (nl >= 0 ? nl + 1 : afterOpen.length);

    if (i + 1 < matches.length) {
      // 闭合块
      const closeStart = matches[i + 1].start;
      const content = text.slice(contentStart, closeStart);
      const firstLine = content.split("\n").find((l) => l.trim().length > 0) ?? "";
      if (!looksLikeArtifact(matches[i].lang, firstLine)) {
        i += 2;
        continue;
      }
      const name =
        extractName(firstLine) ??
        (firstLine.match(/^<!doctype html|<html/i) ? "index.html" : null) ??
        defaultName(matches[i].lang, counter);
      out.push({
        key: counter++,
        name,
        lang: matches[i].lang || "text",
        open: false,
        charCount: content.length,
      });
      i += 2;
    } else {
      // 未闭合块（模型还在写）
      const content = text.slice(contentStart);
      const firstLine = content.split("\n").find((l) => l.trim().length > 0) ?? "";
      if (!looksLikeArtifact(matches[i].lang, firstLine)) {
        i += 1;
        break;
      }
      const name =
        extractName(firstLine) ??
        (firstLine.match(/^<!doctype html|<html/i) ? "index.html" : null) ??
        defaultName(matches[i].lang, counter);
      out.push({
        key: counter++,
        name,
        lang: matches[i].lang || "text",
        open: true,
        charCount: content.length,
      });
      i += 1;
      break;
    }
  }
  return out;
}

interface StreamingArtifactCardProps {
  text: string;
  isStreaming: boolean;
}

function StreamingArtifactCardImpl({ text, isStreaming }: StreamingArtifactCardProps) {
  const artifacts = useMemo(() => parseArtifacts(text), [text]);
  if (!isStreaming || artifacts.length === 0) return null;

  const openCount = artifacts.filter((a) => a.open).length;

  return (
    <div className="my-2 rounded-md border border-[var(--color-border-subtle)] bg-[var(--color-bg-secondary)] p-2.5">
      <div className="flex items-center gap-1.5 mb-1.5 text-text-faint">
        <Box size={13} className="shrink-0" />
        <span className="h-tiny">
          {openCount > 0
            ? `正在生成 ${artifacts.length} 个文件…`
            : `已生成 ${artifacts.length} 个文件`}
        </span>
      </div>
      <div className="flex flex-col gap-1">
        {artifacts.map((a) => {
          const pct = Math.min(100, Math.round((a.charCount / CHAR_BUDGET) * 100));
          return (
            <div key={a.key} className="flex items-center gap-2">
              {a.open ? (
                <Loader2 size={13} className="text-text-warning animate-spin shrink-0" />
              ) : (
                <Check size={13} className="text-text-secondary shrink-0" />
              )}
              <FileCode2 size={13} className="text-text-secondary shrink-0" />
              <span className="text-sm text-text-secondary font-mono truncate max-w-[180px]">
                {a.name}
              </span>
              <span className="h-tiny text-text-faint shrink-0">{a.lang}</span>
              <div className="flex-1 h-1 rounded bg-[var(--color-bg-tertiary)] overflow-hidden">
                <div
                  className={
                    a.open ? "h-full bg-[var(--color-accent)] animate-pulse" : "h-full bg-[var(--color-accent)]"
                  }
                  style={{ width: a.open ? `${pct}%` : "100%" }}
                />
              </div>
              <span className="h-tiny text-text-faint shrink-0 w-[64px] text-right">
                {a.charCount} 字符
              </span>
            </div>
          );
        })}
      </div>
    </div>
  );
}

export const StreamingArtifactCard = memo(StreamingArtifactCardImpl);
