/* ════════════════════════════════════════════════════════════════
   AI Generation — 4-step wizard
   ════════════════════════════════════════════════════════════════ */

import { useState, useEffect, useRef } from "react";
import { ArrowLeft, ArrowRight, Check, Sparkle, Wand2, RefreshCw, Copy, MessageSquare, Layers, Lightbulb } from "lucide-react";
import { clsx } from "clsx";
import {
  PALETTES, TEMPLATES, pickColor,
  type AgentPersona, type AgentColorTheme, type AgentTemplate,
} from "./types";
import { buildSystemPrompt } from "./storage";
import { resolveProviderConfig } from "./providerConfig";

/* ════════════════════════════════════════════════════════════════
   AI Generation — 4-step wizard
   ════════════════════════════════════════════════════════════════ */

type GenStep = "template" | "input" | "clarify" | "preview";
type GenClarify = {
  domain: string | null;        // 领域
  style: string | null;         // 风格
  tools: string[];               // 工具能力
};

interface GeneratedAgent extends Partial<AgentPersona> {
  reasoning?: string;            // AI's design thinking
  systemPrompt: string;
  sampleDialog?: { q: string; a: string };
  colorTheme: AgentColorTheme;
}

const DOMAIN_OPTIONS = [
  { id: "creative",   label: "创意写作",   emoji: "✍️" },
  { id: "code",       label: "编程开发",   emoji: "💻" },
  { id: "analysis",   label: "数据分析",   emoji: "📊" },
  { id: "design",     label: "设计辅助",   emoji: "🎨" },
  { id: "learning",   label: "学习答疑",   emoji: "🎓" },
  { id: "biz",        label: "商业战略",   emoji: "📈" },
  { id: "translate",  label: "翻译",       emoji: "🌐" },
  { id: "chat",       label: "日常对话",   emoji: "💬" },
];

const STYLE_OPTIONS = [
  { id: "professional", label: "专业严谨" },
  { id: "humorous",     label: "幽默风趣" },
  { id: "concise",      label: "简洁直接" },
  { id: "patient",      label: "耐心温和" },
  { id: "sarcastic",    label: "犀利吐槽" },
];

const TOOL_OPTIONS = [
  { id: "search",     label: "网络搜索" },
  { id: "files",      label: "文件操作" },
  { id: "shell",      label: "执行命令" },
  { id: "browser",    label: "浏览器" },
  { id: "write",      label: "写文档" },
  { id: "image",      label: "生成图片" },
];

/** Step 1: when user is stuck, show these as clickable starters that pre-fill description. */
const EXAMPLE_DESCRIPTIONS = [
  "想做一个能帮我审合同的法律顾问，用语严谨，会主动标出风险点",
  "想做一个能陪我练英语口语的朋友，生活话题为主，纠错但不打击人",
  "想做一个能帮我减肥的运动教练，每周出计划，根据反馈调整强度",
  "想做一个能帮我写小红书爆款的写手，熟悉 5 类标题套路，带 emoji",
  "想做一个能帮我读书的助手,30 分钟讲透一本非虚构书的逻辑",
];

interface AIGenerateWizardProps {
  onComplete: (agent: Partial<AgentPersona>) => void;
  onCancel: () => void;
  /** optional initial description if user came from elsewhere */
  initialDescription?: string;
}

export function AIGenerateWizard({ onComplete, onCancel, initialDescription = "" }: AIGenerateWizardProps) {
  const [step, setStep] = useState<GenStep>("template");
  const [selectedTpl, setSelectedTpl] = useState<AgentTemplate | null>(null);
  const [description, setDescription] = useState(initialDescription);
  const [clarify, setClarify] = useState<GenClarify>({ domain: null, style: null, tools: [] });
  const [generated, setGenerated] = useState<GeneratedAgent | null>(null);
  const [generating, setGenerating] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const topRef = useRef<HTMLDivElement>(null);

  // Scroll to top on step change
  useEffect(() => {
    topRef.current?.scrollIntoView({ behavior: "smooth", block: "start" });
  }, [step]);

  // ── Step transitions ──
  const pickTemplate = (tpl: AgentTemplate) => {
    setSelectedTpl(tpl);
    if (!description.trim()) {
      setDescription(`${tpl.tagline},能帮我解决 ${tpl.category} 相关问题`);
    }
    setStep("input");
  };
  const skipToCustom = () => {
    setSelectedTpl(null);
    setStep("input");
  };
  const submitInput = () => {
    if (!description.trim()) return;
    setStep("clarify");
  };
  const submitClarify = () => {
    if (!clarify.domain) {
      setError("请至少选择 1 个领域");
      return;
    }
    setError(null);
    setStep("preview");
    runGeneration();
  };

  // ── AI generation with strengthened prompt ──
  const runGeneration = async () => {
    setGenerating(true);
    setError(null);
    try {
      // local server. Falls back to the user-configured agentsServerUrl.
      // (provider map extracted to providerConfig.ts — used by both this
      // wizard and TestDialog; was duplicated ~50 lines before.)
      const { apiUrl, apiKey } = await resolveProviderConfig();

      const domainText = DOMAIN_OPTIONS.find((o) => o.id === clarify.domain)?.label || clarify.domain;
      const styleText  = STYLE_OPTIONS.find((o) => o.id === clarify.style)?.label  || clarify.style || "未指定";
      const toolsText  = clarify.tools.map((id) => TOOL_OPTIONS.find((o) => o.id === id)?.label).filter(Boolean).join("、") || "无";

      const sysPrompt = `你是一个专业的专家设计师 (Expert Designer)。根据用户的需求设计一个高度专业、表现稳定的 AI 专家。

**设计要求**:
1. 名字简洁,2-4 个汉字或 1 个英文单词，不要包含「AI」「助手」「机器人」等通用词
2. 角色 (role) 必须是具体的职业身份，例如「资深 React 架构师」「小红书爆款写手」
3. 身份标语 (identity) 一句话讲清「这个专家能帮用户做什么」,30 字以内
4. 核心理念 (soul) 1-2 句话，体现价值观和工作方式
5. 技能 (skills) 3-5 个具体能力，避免「对话/分析/生成」这种万能词
6. 性格 (personality) 2-3 个形容词
7. systemPrompt 写一段 80-200 字的完整 system prompt,告诉 LLM 如何扮演这个角色，包含：角色定位、回答风格、回答模板、避免什么
8. sampleDialog 必须给一个真实场景的 Q/A 演示，展示这个专家的说话方式
9. reasoning 简要说明你为什么这样设计 (1-2 句话),让用户知道你是否理解正确

**用户上下文**:
- 需求描述: ${description}
- 领域: ${domainText}
- 风格: ${styleText}
- 工具能力: ${toolsText}
${selectedTpl ? `- 用户从模板「${selectedTpl.name}」开始，可参考其人设` : ""}

**输出**: 严格的 JSON,不要 markdown 包裹。格式:
{"name":"","avatar":"一个emoji","role":"","identity":"","soul":"","skills":[],"specialty":"","dialogStyle":"","personality":[],"systemPrompt":"","sampleDialog":{"q":"","a":""},"reasoning":""}`;

      const res = await fetch(`${apiUrl}/v1/chat/completions`, {
        method: "POST",
        headers: { "Content-Type": "application/json", ...(apiKey ? { Authorization: `Bearer ${apiKey}` } : {}) },
        body: JSON.stringify({
          messages: [
            { role: "system", content: sysPrompt },
            { role: "user", content: description },
          ],
          stream: false,
          temperature: 0.8,
        }),
      });
      if (!res.ok) throw new Error(`API ${res.status}`);
      const data = await res.json();
      const content: string = data.choices?.[0]?.message?.content || "";
      const jsonMatch = content.match(/\{[\s\S]*\}/);
      if (!jsonMatch) throw new Error("AI 返回格式错误");
      const parsed = JSON.parse(jsonMatch[0]);
      const merged: GeneratedAgent = {
        ...parsed,
        colorTheme: selectedTpl?.colorTheme || pickColor(parsed.name || description),
        systemPrompt: parsed.systemPrompt || buildSystemPrompt(parsed),
      };
      setGenerated(merged);
    } catch (e) {
      // Fallback to template-based generation
      const fb = generateFromTemplate(description, selectedTpl, clarify);
      setGenerated(fb);
      setError("AI 服务不可用，已使用模板生成");
    } finally {
      setGenerating(false);
    }
  };

  const handleConfirm = () => {
    if (!generated) return;
    onComplete(generated);
  };

  // ── Renders ──
  const STEPS: GenStep[] = ["template", "input", "clarify", "preview"];
  const STEP_INDEX = STEPS.indexOf(step);
  const g = PALETTES[generated?.colorTheme || selectedTpl?.colorTheme || "violet"];

  return (
    <div className="p-6" ref={topRef}>
      <div className="max-w-2xl mx-auto">
        {/* Header */}
        <div className="flex items-center justify-between mb-5">
          <div className="flex items-center gap-2.5">
            <button
              onClick={step === "template" ? onCancel : () => setStep(STEPS[STEP_INDEX - 1])}
              className="p-1.5 rounded-md hover:bg-bg-hover text-text-muted"
              title="返回"
            >
              <ArrowLeft className="icon-lg" />
            </button>
            <h3 className="text-[length:var(--text-body)] font-semibold text-text-primary flex items-center gap-2">
              <Sparkle className="icon-lg text-text-secondary" />
              AI 生成专家
            </h3>
          </div>
          <div className="flex items-center gap-1.5">
            {STEPS.map((s, i) => (
              <div key={s} className="flex items-center">
                <div className={clsx(
                  "w-6 h-6 rounded-full flex items-center justify-center text-[length:var(--text-meta)] font-medium transition-colors",
                  i < STEP_INDEX ? "bg-green-500 text-white"
                  : i === STEP_INDEX ? "bg-bg-active text-text-primary"
                  : "bg-bg-tertiary text-text-faint"
                )}>
                  {i < STEP_INDEX ? <Check className="icon-sm" /> : i + 1}
                </div>
                {i < STEPS.length - 1 && (
                  <div className={clsx("w-6 h-px", i < STEP_INDEX ? "bg-green-500" : "bg-bg-tertiary")} />
                )}
              </div>
            ))}
          </div>
        </div>

        {/* ── Step 1: Template picker + 不知道写什么时的示例语句 ── */}
        {step === "template" && (
          <div className="rounded-xl bg-bg-card p-5 space-y-4">
            <div>
              <p className="text-[length:var(--text-body)] font-medium text-text-primary mb-1">
                想要一个什么类型的专家？
              </p>
              <p className="text-[length:var(--text-meta)] text-text-muted">
                点示例快速开始，或选模板改，或自己写
              </p>
            </div>

            {/* 示例语句 （灵感起点） */}
            <div>
              <p className="text-[length:var(--text-meta)] text-text-muted mb-1.5">不知道怎么描述?从这些开始:</p>
              <div className="flex flex-wrap gap-1.5">
                {EXAMPLE_DESCRIPTIONS.map((d) => (
                  <button
                    key={d}
                    onClick={() => { setSelectedTpl(null); setDescription(d); setStep("input"); }}
                    className="text-left px-2.5 py-1.5 rounded-md text-[length:var(--text-meta)] text-text-secondary bg-bg-tertiary hover:bg-bg-hover transition-colors"
                  >
                    {d}
                  </button>
                ))}
              </div>
            </div>

            {/* 模板 */}
            <div>
              <p className="text-[length:var(--text-meta)] text-text-muted mb-1.5">或者从模板开始改:</p>
              <div className="grid grid-cols-2 gap-2">
                {TEMPLATES.map((tpl) => (
                  <button
                    key={tpl.id}
                    onClick={() => pickTemplate(tpl)}
                    className="text-left rounded-lg bg-bg-primary p-2.5 hover:bg-bg-hover transition-colors"
                  >
                    <div className="flex items-center gap-2 mb-1">
                      <div
                        className="w-8 h-8 rounded-full flex items-center justify-center text-base shrink-0 bg-bg-tertiary"
                      >
                        {tpl.emoji}
                      </div>
                      <div className="min-w-0 flex-1">
                        <p className="text-[length:var(--text-ui)] font-semibold text-text-primary truncate">{tpl.name}</p>
                        <p className="text-[length:var(--text-meta)] text-text-muted">{tpl.category}</p>
                      </div>
                    </div>
                    <p className="text-[length:var(--text-meta)] text-text-secondary line-clamp-2 leading-relaxed">{tpl.tagline}</p>
                  </button>
                ))}
              </div>
            </div>

            <button
              onClick={skipToCustom}
              className="w-full py-2 rounded-md text-[length:var(--text-meta)] text-text-muted hover:text-text-primary hover:bg-bg-hover transition-colors"
            >
              + 跳过模板，完全自定义
            </button>
          </div>
        )}

        {/* ── Step 2: Description input ── */}
        {step === "input" && (
          <div className="rounded-xl bg-bg-card p-5 space-y-4">
            {selectedTpl && (
              <div className="flex items-center gap-2.5 p-3 rounded-lg bg-bg-primary">
                <div
                  className="w-8 h-8 rounded-lg flex items-center justify-center text-base shrink-0"
                  style={{ background: PALETTES[selectedTpl.colorTheme].avatar }}
                >
                  {selectedTpl.emoji}
                </div>
                <div className="min-w-0 flex-1">
                  <p className="text-[length:var(--text-meta)] text-text-muted">基于模板</p>
                  <p className="text-[length:var(--text-ui)] font-medium text-text-primary">{selectedTpl.name}</p>
                </div>
                <button onClick={() => { setSelectedTpl(null); setStep("template"); }} className="text-[length:var(--text-meta)] text-text-muted hover:text-text-primary">
                  换一个
                </button>
              </div>
            )}
            <div>
              <label className="block text-[length:var(--text-meta)] text-text-muted uppercase tracking-wider mb-1.5 font-semibold">
                描述你想要的专家
              </label>
              <textarea
                rows={4}
                value={description}
                onChange={(e) => setDescription(e.target.value)}
                placeholder="越具体越好：要解决什么问题?服务谁?在什么场景下使用?有什么特殊要求？"
                className="w-full rounded-lg bg-bg-tertiary px-3 py-2.5 text-[length:var(--text-body)] text-text-primary placeholder:text-text-faint outline-none resize-none"
                autoFocus
              />
              <p className="mt-1.5 text-[length:var(--text-meta)] text-text-faint">
                提示：不要只写名字，告诉 AI 这个专家要「做什么」「给谁用」「怎么回答」
              </p>
            </div>
            <div className="flex justify-end gap-2 pt-2">
              <button
                onClick={() => setStep("template")}
                className="px-3.5 py-2 rounded-lg text-[length:var(--text-ui)] font-medium"
                style={{ background: "var(--color-btn-secondary-bg)", color: "var(--color-btn-secondary-text)" }}
              >
                上一步
              </button>
              <button
                onClick={submitInput}
                disabled={!description.trim()}
                className="flex items-center gap-1.5 px-4 py-2 rounded-lg text-[length:var(--text-ui)] font-medium disabled:opacity-40"
                style={{ background: "var(--color-btn-primary-bg)", color: "var(--color-btn-primary-text)" }}
>
                下一步 <ArrowRight className="icon-md" />
              </button>
            </div>
          </div>
        )}

        {/* ── Step 3: Clarify ── */}
        {step === "clarify" && (
          <div className="rounded-xl bg-bg-card p-5 space-y-4">
            <div>
              <p className="text-[length:var(--text-ui)] font-medium text-text-primary mb-0.5">再补几个关键问题</p>
              <p className="text-[length:var(--text-meta)] text-text-muted">回答几个问题,AI 会设计得更准</p>
            </div>

            <div>
              <p className="text-[length:var(--text-meta)] font-medium text-text-secondary mb-2">
                1. 这个专家主要在哪个领域？ <span className="text-error">*</span>
              </p>
              <div className="flex flex-wrap gap-1.5">
                {DOMAIN_OPTIONS.map((o) => (
                  <button
                    key={o.id}
                    onClick={() => setClarify((c) => ({ ...c, domain: o.id }))}
                    className={clsx(
                      "px-2.5 py-1.5 rounded-full text-[length:var(--text-meta)] font-medium transition-colors",
                      clarify.domain === o.id
                        ? "bg-bg-active text-text-primary"
                        : "bg-bg-tertiary text-text-secondary hover:bg-bg-hover"
                    )}
                  >
                    <span className="mr-1">{o.emoji}</span>{o.label}
                  </button>
                ))}
              </div>
            </div>

            <div>
              <p className="text-[length:var(--text-meta)] font-medium text-text-secondary mb-2">2. 对话风格偏好？</p>
              <div className="flex flex-wrap gap-1.5">
                {STYLE_OPTIONS.map((o) => (
                  <button
                    key={o.id}
                    onClick={() => setClarify((c) => ({ ...c, style: o.id }))}
                    className={clsx(
                      "px-2.5 py-1.5 rounded-full text-[length:var(--text-meta)] font-medium transition-colors",
                      clarify.style === o.id
                        ? "bg-bg-active text-text-primary"
                        : "bg-bg-tertiary text-text-secondary hover:bg-bg-hover"
                    )}
                  >
                    {o.label}
                  </button>
                ))}
              </div>
            </div>

            <div>
              <p className="text-[length:var(--text-meta)] font-medium text-text-secondary mb-2">3. 需要什么工具能力？ （可多选）</p>
              <div className="flex flex-wrap gap-1.5">
                {TOOL_OPTIONS.map((o) => {
                  const on = clarify.tools.includes(o.id);
                  return (
                    <button
                      key={o.id}
                      onClick={() => setClarify((c) => ({
                        ...c,
                        tools: on ? c.tools.filter((t) => t !== o.id) : [...c.tools, o.id],
                      }))}
                      className={clsx(
                        "px-2.5 py-1.5 rounded-full text-[length:var(--text-meta)] font-medium transition-colors flex items-center gap-1",
                        on
                          ? "bg-bg-active text-text-primary"
                          : "bg-bg-tertiary text-text-secondary hover:bg-bg-hover"
                      )}
                    >
                      {on && <Check className="icon-sm" />}{o.label}
                    </button>
                  );
                })}
              </div>
            </div>

            {error && <p className="text-[length:var(--text-meta)] text-error">{error}</p>}

            <div className="flex justify-end gap-2 pt-2">
              <button
                onClick={() => setStep("input")}
                className="px-3.5 py-2 rounded-lg text-[length:var(--text-ui)] font-medium"
                style={{ background: "var(--color-btn-secondary-bg)", color: "var(--color-btn-secondary-text)" }}
              >
                上一步
              </button>
              <button
                onClick={submitClarify}
                className="flex items-center gap-1.5 px-4 py-2 rounded-lg text-[length:var(--text-ui)] font-medium"
                style={{ background: "var(--color-btn-primary-bg)", color: "var(--color-btn-primary-text)" }}
              >
                <Wand2 className="icon-md" /> 生成专家
              </button>
            </div>
          </div>
        )}

        {/* ── Step 4: Preview ── */}
        {step === "preview" && (
          <div className="space-y-4">
            {generating ? (
              <div className="rounded-xl bg-bg-card p-12 flex flex-col items-center justify-center">
                <div className="w-10 h-10 rounded-full border-2 border-bg-tertiary" style={{ borderTopColor: "var(--color-text-primary)" }} />
                <p className="text-[length:var(--text-ui)] text-text-secondary">AI 正在为你设计专家...</p>
                <p className="text-[length:var(--text-meta)] text-text-faint mt-1">推理 + 生成人设 + 写 system prompt + 想样例对话</p>
              </div>
            ) : generated ? (
              <>
                {/* Reasoning banner */}
                {generated.reasoning && (
                  <div className="rounded-lg bg-info-soft p-3.5 flex gap-2.5">
                    <div className="shrink-0"><Lightbulb className="icon-md" /></div>
                    <div className="flex-1 min-w-0">
                      <p className="text-[length:var(--text-meta)] text-text-muted uppercase tracking-wider mb-0.5 font-semibold">设计思路</p>
                      <p className="text-[length:var(--text-ui)] text-text-secondary leading-relaxed">{generated.reasoning}</p>
                    </div>
                  </div>
                )}

                {/* Card preview */}
                <div className="rounded-xl bg-bg-card overflow-hidden">
                  <div className="p-4">
                    <div className="flex items-start gap-3 mb-3">
                      <div
                        className="w-12 h-12 rounded-xl flex items-center justify-center text-2xl shrink-0 shadow-sm"
                        style={{ background: g.avatar }}
                      >
                        {generated.avatar || "🤖"}
                      </div>
                      <div className="min-w-0 flex-1">
                        <h4 className="text-[length:var(--text-body)] font-semibold text-text-primary">{generated.name || "未命名"}</h4>
                        <p className="text-[length:var(--text-meta)] text-text-muted">{generated.role || "AI 助手"}</p>
                      </div>
                    </div>
                    {generated.identity && (
                      <p className="text-[length:var(--text-ui)] text-text-secondary leading-relaxed mb-2.5">{generated.identity}</p>
                    )}
                    {generated.soul && (
                      <p className="text-[length:var(--text-meta)] text-text-muted leading-relaxed mb-3 italic">{generated.soul}</p>
                    )}
                    {(generated.skills?.length ?? 0) > 0 && (
                      <div className="flex flex-wrap gap-1 mb-2">
                        {generated.skills!.map((s) => (
                          <span key={s} className="px-1.5 py-0.5 rounded-md text-[length:var(--text-meta)] font-medium" style={{ background: g.chip.bg, color: g.chip.fg }}>{s}</span>
                        ))}
                      </div>
                    )}
                    {generated.dialogStyle && (
                      <p className="text-[length:var(--text-meta)] text-text-faint">对话风格:{generated.dialogStyle}</p>
                    )}
                  </div>
                </div>

                {/* System prompt editor */}
                <details className="rounded-xl bg-bg-card overflow-hidden group" open>
                  <summary className="flex items-center justify-between p-3.5 cursor-pointer hover:bg-bg-hover">
                    <span className="text-[length:var(--text-ui)] font-medium text-text-primary flex items-center gap-1.5">
                      <Layers className="icon-md text-text-muted" />
                      System Prompt （可编辑）
                    </span>
                    <button
                      onClick={(e) => { e.preventDefault(); navigator.clipboard?.writeText(generated.systemPrompt); }}
                      className="text-[length:var(--text-meta)] text-text-muted hover:text-text-primary flex items-center gap-1"
                    >
                      <Copy className="icon-sm" /> 复制
                    </button>
                  </summary>
                  <textarea
                    value={generated.systemPrompt}
                    onChange={(e) => setGenerated((g) => g ? { ...g, systemPrompt: e.target.value } : g)}
                    rows={6}
                    className="w-full px-3.5 pb-3.5 text-[length:var(--text-meta)] text-text-secondary font-mono leading-relaxed bg-bg-primary outline-none resize-none"
                  />
                </details>

                {/* Sample dialog */}
                {generated.sampleDialog && (
                  <div className="rounded-xl bg-bg-card p-4 space-y-2">
                    <p className="text-[length:var(--text-meta)] text-text-muted uppercase tracking-wider font-semibold flex items-center gap-1.5">
                      <MessageSquare className="icon-sm" /> 样例对话预览
                    </p>
                    <div className="bg-bg-tertiary rounded-lg px-3 py-2 text-[length:var(--text-ui)] text-text-primary">
                      <span className="text-text-faint text-[length:var(--text-meta)] mr-1.5">用户</span>
                      {generated.sampleDialog.q}
                    </div>
                    <div className="rounded-lg px-3 py-2 text-[length:var(--text-ui)] text-text-primary ml-4" style={{ background: g.chip.bg, color: g.chip.fg }}>
                      <span className="opacity-60 text-[length:var(--text-meta)] mr-1.5">{generated.name || "专家"}</span>
                      <span className="text-text-primary">{generated.sampleDialog.a}</span>
                    </div>
                  </div>
                )}

                {error && <p className="text-[length:var(--text-meta)] text-warning bg-warning-soft p-2 rounded">{error}</p>}

                <div className="flex justify-between items-center pt-2">
                  <button
                    onClick={() => setStep("clarify")}
                    className="text-[length:var(--text-ui)] text-text-muted hover:text-text-primary"
                  >
                    ← 调整参数
                  </button>
                  <div className="flex gap-2">
                    <button
                      onClick={runGeneration}
                      className="flex items-center gap-1.5 px-3.5 py-2 rounded-lg text-[length:var(--text-ui)] font-medium"
                      style={{ background: "var(--color-btn-secondary-bg)", color: "var(--color-btn-secondary-text)" }}
>
                      <RefreshCw className="icon-md" /> 换个版本
                    </button>
                    <button
                      onClick={handleConfirm}
                      className="flex items-center gap-1.5 px-4 py-2 rounded-lg text-[length:var(--text-ui)] font-medium"
                      style={{ background: "var(--color-btn-primary-bg)", color: "var(--color-btn-primary-text)" }}
                    >
                      <Check className="icon-md" /> 确认保存
                    </button>
                  </div>
                </div>
              </>
            ) : null}
          </div>
        )}
      </div>
    </div>
  );
}

/* ── Template fallback (when AI API is unreachable) ── */

export function generateFromTemplate(
  description: string,
  tpl: AgentTemplate | null,
  clarify: GenClarify,
): GeneratedAgent {
  const domainText = DOMAIN_OPTIONS.find((o) => o.id === clarify.domain)?.label || "通用";
  const styleText = STYLE_OPTIONS.find((o) => o.id === clarify.style)?.label || "简洁专业";
  return {
    name: tpl?.name || description.slice(0, 6) || "新专家",
    avatar: tpl?.emoji || "🤖",
    role: tpl?.role || `${domainText}专家`,
    identity: tpl?.tagline || description.slice(0, 30),
    soul: `专注于 ${domainText} 场景，提供 ${styleText} 的回答`,
    skills: tpl?.skills || ["对话", "分析", "建议"],
    specialty: tpl?.category || domainText,
    dialogStyle: styleText,
    personality: ["专业", "高效", "耐心"],
    systemPrompt: tpl?.systemPrompt || `你是一个 ${domainText} 领域的专家，提供 ${styleText} 的回答。`,
    sampleDialog: tpl?.sampleDialog || {
      q: "你好",
      a: "你好!有什么我可以帮你的吗？",
    },
    reasoning: "(AI 服务不可用，使用了基于模板的快速生成。请检查网络后重新生成。)",
    colorTheme: tpl?.colorTheme || pickColor(description || "agent"),
  };
}