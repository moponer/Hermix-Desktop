import { useState, useEffect, useMemo } from "react";
import {
  Bot, Plus, Sparkles, Brain, ArrowLeft,
} from "lucide-react";
import { clsx } from "clsx";
import { BUILTIN_EXPERTS } from "../data/builtinExperts";
import {
  pickColor,
  type AgentPersona,
} from "./agents/types";
import {
  loadAgentsAsync, loadAgentsLocal,
  saveAgentAsync, deleteAgentAsync, buildSystemPrompt,
} from "./agents/storage";
import { AgentCard } from "./agents/AgentCard";
import { AgentForm } from "./agents/AgentForm";
import { AIGenerateWizard } from "./agents/AIGenerateWizard";
import { TestDialog } from "./agents/TestDialog";
import { BuiltInExpertsTab } from "./agents/BuiltInExpertsTab";










/* ════════════════════════════════════════════════════════════════
   Main View
   ════════════════════════════════════════════════════════════════ */

export type AgentsTab = "my" | "builtin";

interface AgentsViewProps {
  initialTab?: AgentsTab;
}

export function AgentsView({
  initialTab = "my",
}: AgentsViewProps = {}) {
  const [tab, setTab] = useState<AgentsTab>(initialTab);
  const [agents, setAgents] = useState<AgentPersona[]>(loadAgentsLocal);
  const [mode, setMode] = useState<"list" | "create" | "generate">("list");
  const [editingId, setEditingId] = useState<string | null>(null);
  const [testingId, setTestingId] = useState<string | null>(null);
  const [sortBy, setSortBy] = useState<"recent" | "popular" | "name">("recent");

  // Load agents from backend API on mount (falls back to localStorage)
  useEffect(() => {
    loadAgentsAsync().then(setAgents);
  }, []);

  // NOTE: no auto-persist useEffect here. The backend (`/api/agents/custom`)
  // is the source of truth; individual handlers call saveAgentAsync /
  // deleteAgentAsync which upsert the localStorage fallback cache
  // persist half-typed drafts if the app crashed mid-edit.

  /** 切 tab 时如果向导/表单开着,自动重置 — 否则用户切到"内置专家"再切回"我的专家"还会被向导锁住 */
  const switchTab = (newTab: AgentsTab) => {
    if (tab === newTab) return;
    if (mode !== "list") {
      setMode("list");
      setEditingId(null);
    }
    setTab(newTab);
  };

  const handleSave = (data: Partial<AgentPersona>) => {
    const now = Date.now();
    const systemPrompt = data.systemPrompt || buildSystemPrompt(data);
    if (editingId) {
      const updated = agents.map((a) =>
        a.id === editingId
          ? { ...a, ...data, systemPrompt, colorTheme: data.colorTheme || a.colorTheme, updatedAt: now }
          : a
      );
      setAgents(updated);
      // Persist to backend
      const agent = updated.find((a) => a.id === editingId);
      if (agent) saveAgentAsync(agent);
      setEditingId(null);
    } else {
      const newAgent: AgentPersona = {
        id: `agent-${now}`,
        name: data.name || "未命名专家",
        avatar: data.avatar || "🤖",
        role: data.role || "AI 助手",
        identity: data.identity || "",
        soul: data.soul || "",
        skills: data.skills || [],
        specialty: data.specialty || "",
        dialogStyle: data.dialogStyle || "",
        background: data.background || "",
        personality: data.personality || [],
        systemPrompt,
        model: data.model,
        sampleDialog: data.sampleDialog,
        colorTheme: data.colorTheme || pickColor(data.name || `agent-${now}`),
        usageCount: 0,
        lastUsedAt: now,
        createdAt: now,
        updatedAt: now,
      };
      setAgents((prev) => [...prev, newAgent]);
      saveAgentAsync(newAgent);
    }
    setMode("list");
  };

  const handleDelete = (id: string) => {
    const remaining = agents.filter((a) => a.id !== id);
    setAgents(remaining);
    deleteAgentAsync(id, remaining);
  };

  const handleEdit = (id: string) => {
    setEditingId(id);
    setMode("create");
  };

  const handleTest = (id: string) => {
    setTestingId(id);
  };

  const editingAgent = editingId ? agents.find((a) => a.id === editingId) : null;
  const testingAgent = testingId ? agents.find((a) => a.id === testingId) : null;

  // Sort agents
  const sortedAgents = useMemo(() => {
    const list = [...agents];
    if (sortBy === "recent") list.sort((a, b) => b.lastUsedAt - a.lastUsedAt);
    else if (sortBy === "popular") list.sort((a, b) => b.usageCount - a.usageCount);
    else if (sortBy === "name") list.sort((a, b) => a.name.localeCompare(b.name, "zh-CN"));
    return list;
  }, [agents, sortBy]);

  return (
    <div className="h-full flex flex-col overflow-hidden">
      {/* Header */}
      <div className="flex items-center justify-between px-6 py-4">
        <div className="flex items-center gap-2.5">
          <Brain size={20} className="text-text-secondary" />
          <h2 className="text-[16px] font-semibold text-text-primary">专家</h2>
          <span className="text-[11px] text-text-faint font-mono">{agents.length}</span>
        </div>
        <div className="flex items-center gap-1 rounded-lg bg-bg-tertiary p-0.5">
          <button
            onClick={() => switchTab("my")}
            className={clsx(
              "px-3 py-1 rounded-md text-[13px]",
              tab === "my" ? "bg-accent-soft text-accent font-medium" : "text-text-muted hover:text-text-primary"
            )}
          >
            我的专家
            {agents.length > 0 && <span className="ml-1 text-[11px] text-text-faint">{agents.length}</span>}
          </button>
          <button
            onClick={() => switchTab("builtin")}
            className={clsx(
              "px-3 py-1 rounded-md text-[13px]",
              tab === "builtin" ? "bg-accent-soft text-accent font-medium" : "text-text-muted hover:text-text-primary"
            )}
          >
            内置专家
            <span className="ml-1 text-[11px] text-text-faint">{BUILTIN_EXPERTS.length}</span>
          </button>
        </div>
        <div className="flex gap-2">
          {tab === "my" && mode === "list" && (
            <>
              <button
                onClick={() => { setEditingId(null); setMode("create"); }}
                className="flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-[13px] font-medium"
                style={{ background: "var(--color-btn-secondary-bg)", color: "var(--color-btn-secondary-text)" }}
              >
                <Plus className="icon-md" /> 手动创建
              </button>
              <button
                onClick={() => setMode("generate")}
                className="flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-[13px] font-medium"
                style={{ background: "var(--color-btn-primary-bg)", color: "var(--color-btn-primary-text)" }}
              >
                <Sparkles className="icon-md" /> AI 生成
              </button>
            </>
          )}
          {tab === "my" && mode !== "list" && (
            <button
              onClick={() => { setMode("list"); setEditingId(null); }}
              className="flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-[13px] font-medium"
              style={{ background: "var(--color-btn-secondary-bg)", color: "var(--color-btn-secondary-text)" }}
            >
              <ArrowLeft className="icon-md" /> 返回列表
            </button>
          )}
        </div>
      </div>

      {/* Content */}
      <div className="flex-1 overflow-y-auto min-h-0">
        {tab === "my" && (
          <>
            {mode === "list" && (
              <div className="p-6">
                {agents.length === 0 ? (
                  <div className="flex flex-col items-center justify-center py-20 text-center">
                    <Bot size={48} className="text-text-faint mb-4" />
                    <h3 className="text-[15px] font-medium text-text-secondary mb-2">还没有专家</h3>
                    <p className="text-[13px] text-text-muted mb-6">手动创建或让 AI 帮你生成一个专家</p>
                    <div className="flex gap-2">
                      <button
                        onClick={() => { setEditingId(null); setMode("create"); }}
                        className="px-4 py-2 rounded-lg text-[13px] font-medium"
                        style={{ background: "var(--color-btn-secondary-bg)", color: "var(--color-btn-secondary-text)" }}
                      >
                        手动创建
                      </button>
                      <button
                        onClick={() => setMode("generate")}
                        className="px-4 py-2 rounded-lg text-[13px] font-medium"
                        style={{ background: "var(--color-btn-primary-bg)", color: "var(--color-btn-primary-text)" }}
                      >
                        AI 生成
                      </button>
                    </div>
                  </div>
                ) : (
                  <>
                    {/* Sort tabs */}
                    <div className="flex items-center justify-between mb-4">
                      <div className="flex items-center gap-1 text-[12px]">
                        <span className="text-text-faint">排序:</span>
                        {([["recent", "最近使用"], ["popular", "最常使用"], ["name", "按名称"]] as const).map(([k, l]) => (
                          <button
                            key={k}
                            onClick={() => setSortBy(k)}
                            className={clsx(
                              "px-2 py-0.5 rounded",
                              sortBy === k ? "bg-bg-tertiary text-text-primary" : "text-text-muted hover:text-text-primary"
                            )}
                          >
                            {l}
                          </button>
                        ))}
                      </div>
                      <p className="text-[11px] text-text-faint">{sortedAgents.length} 个专家</p>
                    </div>

                    <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-4">
                      {sortedAgents.map((agent) => (
                        <AgentCard
                          key={agent.id}
                          agent={agent}
                          onEdit={() => handleEdit(agent.id)}
                          onDelete={() => handleDelete(agent.id)}
                          onTest={() => handleTest(agent.id)}
                        />
                      ))}
                    </div>
                  </>
                )}
              </div>
            )}

            {mode === "create" && (
              <div className="p-6">
                <div className="max-w-2xl mx-auto">
                  <h3 className="text-[15px] font-semibold text-text-primary mb-4">
                    {editingAgent ? `编辑「${editingAgent.name}」` : "创建新专家"}
                  </h3>
                  <div className="rounded-xl bg-bg-card p-5">
                    <AgentForm
                      initial={editingAgent || undefined}
                      onSave={handleSave}
                      onCancel={() => { setMode("list"); setEditingId(null); }}
                    />
                  </div>
                </div>
              </div>
            )}

            {mode === "generate" && (
              <AIGenerateWizard
                onComplete={handleSave}
                onCancel={() => setMode("list")}
              />
            )}
          </>
        )}

        {tab === "builtin" && <BuiltInExpertsTab />}
      </div>

      {/* Test dialog */}
      {testingAgent && (
        <TestDialog agent={testingAgent} onClose={() => setTestingId(null)} />
      )}
    </div>
  );
}
