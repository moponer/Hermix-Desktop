import { useCallback, useEffect, useState } from "react";
import {
  Plus,
  Trash2,
  Play,
  Power,
  Pencil,

  AlertTriangle,
  Info,
  Clock,
  Megaphone,
  ShieldAlert,
  ClipboardList,
  FileText,
  Repeat2,
  type LucideIcon,
} from "lucide-react";
import * as api from "../api";
import type { AutomationTask } from "../types";
import type { IconTone } from "../lib/categoryTone";
import { toneForCategory } from "../lib/categoryTone";
import { ToolCardIcon } from "./config/ToolCardIcon";
import { AiGenerateButton } from "./AiGenerateButton";
import { storage, STORAGE_KEYS } from "../utils/storageCompat";
import { setKeepAwake, KEEP_AWAKE_CHANGED_EVENT } from "../desktopBridge";
import { useWorkspaces } from "../hooks/useWorkspaces";
import { toastInfo, toastError, toastSuccess } from "../hooks/useToast";
import { Button, ConfirmDialog, EmptyState, Input, Modal, PageHeader, SelectField, SkeletonCard, TextArea } from "./ui";
import { DELETE, SAVE, SAVED, SAVING } from "./strings";

/** A small set of cron presets the editor offers as quick-pick chips.
 *  Each entry: [label, cronExpression, humanDescription]. */
const CRON_PRESETS: Array<[string, string, string]> = [
  ["每 15 分钟", "*/15 * * * *", "Quarter-hour cadence"],
  ["每小时", "0 * * * *", "Top of every hour"],
  ["每天 08:00", "0 8 * * *", "Morning brief"],
  ["每天 21:00", "0 21 * * *", "End-of-day review"],
  ["工作日 09:00", "0 9 * * 1-5", "Weekday standup"],
  ["每周一 09:00", "0 9 * * 1", "Weekly plan"],
  ["每周五 17:00", "0 17 * * 5", "Friday wrap-up"],
  ["每月 1 日 10:00", "0 10 1 * *", "Monthly report"],
];

/** 定时任务模板（对齐 ZCode 工作流页）：一键预填编辑器，存档前可再改。
 *
 *  2026-10-23：`icon` 从 emoji 字符串改成 lucide 组件 + 色调。
 *  emoji 的问题不是"不好看"，而是**不可控**：字形由系统字体决定（Win10/Win11
 *  两套风格）、尺寸跟着字号走、颜色写死在字体里、无法跟随主题，且与旁边
 *  lucide 的描边图标根本不是一个视觉体系。四个模板给四个不同色相，一眼能分。 */
interface TaskTemplate {
  icon: LucideIcon;
  tone: IconTone;
  name: string;
  description: string;
  schedule: string;
  scheduleLabel: string;
  prompt: string;
}

const TEMPLATES: TaskTemplate[] = [
  {
    icon: Megaphone,
    tone: "amber",
    name: "晨会动态",
    description:
      "汇总上一个工作日以来的提交、模块变化、CI 状态和待跟进事项，最终生成不超过 6 条的晨会口述摘要。只读分析，不做任何修改。",
    schedule: "0 9 * * 1-5",
    scheduleLabel: "每工作日 09:00",
    prompt: `汇总自上一个工作日以来的仓库动态，输出一份不超过 6 条的晨会口述摘要。
要求：
1) 覆盖：git 提交、模块/文件变化、CI 运行状态、待跟进事项；
2) 只读分析：不修改任何文件、不执行任何写入操作；
3) 每条一句话，按重要性排序，末尾标出今天最需要关注的一件事。`,
  },
  {
    icon: ShieldAlert,
    tone: "rose",
    name: "风险扫描",
    description:
      "检查最近 24 小时的代码变更，识别运行错误、数据丢失、权限绕过、资源泄漏及跨端兼容等高置信风险，并附代码位置。",
    schedule: "0 10 * * *",
    scheduleLabel: "每天 10:00",
    prompt: `检查最近 24 小时的代码变更，识别高置信风险并逐条报告：
1) 运行错误、数据丢失、权限绕过、资源泄漏、跨端兼容问题；
2) 每条附代码位置（文件:行号）与一句风险说明；
3) 只读分析：不修改文件；没有发现风险就明确说"未发现高置信风险"。`,
  },
  {
    icon: ClipboardList,
    tone: "cyan",
    name: "发布简报",
    description:
      "整理本周合并的 PR 和 commit，按功能、修复、体验及工程改进分类，同时生成团队版和面向用户的精简发布说明。",
    schedule: "0 16 * * 5",
    scheduleLabel: "每周五 16:00",
    prompt: `整理本周合并的 PR 和 commit，输出发布简报：
1) 按功能、修复、体验、工程改进四类分组；
2) 生成两版：团队版（细节完整）与用户版（精简、面向使用者）；
3) 引用具体 PR / commit 标题作为依据，不编造条目。`,
  },
  {
    icon: FileText,
    tone: "green",
    name: "文档同步检查",
    description:
      "对照最近 7 天的代码、配置、接口与文档变更，识别已改变公开行为但文档尚未同步的高置信差异，并附文件路径。",
    schedule: "0 15 * * 3",
    scheduleLabel: "每周三 15:00",
    prompt: `对照最近 7 天的代码、配置与接口变更，找出已改变公开行为但文档尚未同步的高置信差异：
1) 逐条列出：变更的文件路径、对应的过时文档位置、建议的更新要点；
2) 只读分析：不修改文件；
3) 没有差异就明确说"文档与代码当前一致"。`,
  },
];

function templateToTask(t: TaskTemplate): AutomationTask {
  return {
    ...blankDraft(),
    name: t.name,
    description: t.description,
    /* 不再写 emoji 图标：卡片的图形由 category 推导的色调 + lucide 图标承担。 */
    icon: "",
    schedule: t.schedule,
    schedule_label: t.scheduleLabel,
    prompt: t.prompt,
  };
}

export function AutomationView({ openChat, onCreateWorkflow }: { openChat: () => void; onCreateWorkflow: () => void }) {
  const [tab, setTab] = useState<"auto" | "flow">("auto");
  const [tasks, setTasks] = useState<AutomationTask[]>([]);
  const [editing, setEditing] = useState<AutomationTask | null>(null);
  const [creating, setCreating] = useState<AutomationTask | null>(null);
  const [confirmReset, setConfirmReset] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  // 「运行会话时保持电脑唤醒」开关。写档 + 广播事件；App 层监听事件并
  // 联动会话流式状态调用 set_keep_awake（本组件只在自动化页挂载，
  // 生效逻辑不能住在这里）。
  const [keepAwake, setKeepAwakeState] = useState(
    () => storage.getItem(STORAGE_KEYS.keepAwake) === "true",
  );

  const toggleKeepAwake = (on: boolean) => {
    setKeepAwakeState(on);
    try {
      storage.setItem(STORAGE_KEYS.keepAwake, String(on));
    } catch {}
    setKeepAwake(on);
    window.dispatchEvent(new Event(KEEP_AWAKE_CHANGED_EVENT));
  };

  const refresh = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const list = await api.getAutomationTasks();
      setTasks(list);
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const onSave = useCallback(
    async (task: AutomationTask, isCreate: boolean) => {
      const saved = isCreate
        ? await api.createAutomationTask(task)
        : await api.updateAutomationTask(task.id, task);
      if (saved) {
        await refresh();
        setEditing(null);
        setCreating(null);
      } else {
        setError("保存失败，请检查 schedule 表达式或后端日志");
      }
    },
    [refresh],
  );

  const onDelete = useCallback(
    async (id: string) => {
      if (await api.deleteAutomationTask(id)) {
        await refresh();
      } else {
        setError("删除失败");
      }
    },
    [refresh],
  );

  /** 恢复官方任务清单：POST /api/automation/reset。后端丢弃当前清单、
   *  重新播种内置官方任务；成功后重拉列表。 */
  const doReset = useCallback(async () => {
    try {
      await api.fetchJSON("/api/automation/reset", { method: "POST" });
      toastSuccess("已恢复官方任务清单");
      await refresh();
    } catch (e) {
      toastError(`恢复失败：${e instanceof Error ? e.message : String(e)}`);
    }
  }, [refresh]);

  const onToggle = useCallback(
    async (task: AutomationTask) => {
      const updated = await api.updateAutomationTask(task.id, {
        enabled: !task.enabled,
      });
      if (updated) {
        setTasks((prev) => prev.map((t) => (t.id === task.id ? updated : t)));
      }
    },
    [],
  );

  const onTrigger = useCallback(
    async (id: string) => {
      if (await api.triggerAutomationTask(id)) {
        // Optimistic UI — the actual run is async on the server.
        setTasks((prev) =>
          prev.map((t) =>
            t.id === id ? { ...t, last_status: "running" } : t,
          ),
        );
        // Refresh in a moment to pick up the post-run status.
        setTimeout(() => void refresh(), 1500);
      } else {
        setError("触发失败");
      }
    },
    [refresh],
  );

  // Group by category (use "未分类" for tasks without a category).
  const grouped = tasks.reduce<Record<string, AutomationTask[]>>(
    (acc, task) => {
      const cat = task.category ?? "未分类";
      if (!acc[cat]) acc[cat] = [];
      acc[cat].push(task);
      return acc;
    },
    {},
  );
  const categories = Object.keys(grouped).sort((a, b) =>
    a === "未分类" ? 1 : b === "未分类" ? -1 : a.localeCompare(b, "zh"),
  );

  return (
    <div className="flex-1 flex flex-col min-h-0">
      {/* Header：双 tab（自动化 | 工作流），统一 PageHeader（R1-3，废止 text-2xl 大字 tab） */}
      <PageHeader icon={<Clock className="icon-lg" />} title="自动化">
        <div className="flex items-center gap-1 rounded-lg bg-bg-tertiary p-0.5">
          <button
            type="button"
            onClick={() => setTab("auto")}
            className={`px-3 py-1 rounded-md text-[length:var(--text-ui)] transition-colors ${
              tab === "auto" ? "bg-bg-active text-text-primary font-medium" : "text-text-muted hover:text-text-primary"
            }`}
          >
            自动化
          </button>
          <button
            type="button"
            onClick={() => setTab("flow")}
            className={`px-3 py-1 rounded-md text-[length:var(--text-ui)] transition-colors ${
              tab === "flow" ? "bg-bg-active text-text-primary font-medium" : "text-text-muted hover:text-text-primary"
            }`}
          >
            工作流
          </button>
        </div>
      </PageHeader>
      <div className="flex-1 overflow-y-auto px-8 pb-6 min-h-0">
      <div className="max-w-4xl mx-auto">
        {tab === "auto" ? (
          <>
            <div className="flex items-start justify-between mb-6 gap-4">
              <p className="text-sm text-text-muted mt-1">
                通过设置定期聊天，实现工作自动化。后端每 30 秒检查一次，
                到点自动用当前会话的 agent 跑 prompt。
              </p>
              <div className="flex items-center gap-2 shrink-0">
                <button
                  onClick={() => setConfirmReset(true)}
                  disabled={tasks.length === 0}
                  className="flex items-center gap-1.5 px-3 py-2 rounded-lg text-text-secondary text-[length:var(--text-ui)] hover:text-text-error hover:bg-error-soft disabled:opacity-40"
                  title="恢复为内置官方任务清单（丢弃全部现有任务）"
                >
                  恢复官方清单
                </button>
                <button
                  onClick={() => setCreating(blankDraft())}
                  className="flex items-center gap-1.5 px-3 py-2 rounded-lg bg-accent text-bg-primary text-[length:var(--text-ui)] font-medium hover:bg-accent-hover"
                >
                  <Plus className="icon-md" /> 新建自动化
                </button>
              </div>
            </div>
            {error && (
              <p className="text-[length:var(--text-ui)] text-text-error -mt-4 mb-4 flex items-start gap-1.5">
                <AlertTriangle className="icon-md shrink-0 mt-0.5" />
                <span>{error}</span>
              </p>
            )}

        {loading && tasks.length === 0 ? (
          <div className="py-8 flex flex-col items-center gap-3">
            <SkeletonCard lines={2} />
            <SkeletonCard lines={2} />
          </div>
        ) : tasks.length === 0 ? (
          <EmptyState
            variant="plain"
            title="还没有自动化任务"
            description="点击右上「新建自动化」开始"
          />
        ) : (
          categories.map((cat) => (
            <div key={cat} className="mb-6">
              <h2 className="text-[length:var(--text-ui)] font-semibold text-text-muted mb-2">
                {cat} · {grouped[cat].length}
              </h2>
              <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
                {grouped[cat].map((t) => (
                  <TaskCard
                    key={t.id}
                    task={t}
                    onEdit={() => setEditing(t)}
                    onDelete={() => setConfirmDelete(t.id)}
                    onToggle={() => void onToggle(t)}
                    onTrigger={() => void onTrigger(t.id)}
                  />
                ))}
              </div>
            </div>
          ))
        )}
        {/* Hermix 有任务在跑时保持电脑唤醒（对齐 ZCode 的 keep-awake 语义）：
            开启后，会话正在流式**或**后台定时任务正在执行，均阻止系统睡眠。
            后台任务这一路尤其重要——到点执行时没有任何 UI 会话在流式，
            只看流式会让笔记本合盖错过执行窗口。
            生效逻辑在 App 层（全局挂载），这里只负责开关与持久化。
            R1-4：从滚动根下移入同一 max-w 容器，消除两层宽度体系。 */}
        {tab === "auto" && (
          <>
        <div className="flex items-center justify-between rounded-xl bg-bg-card px-4 py-3 mb-5">
          <div className="flex items-center gap-2 text-[length:var(--text-ui)] text-text-secondary">
            <Info className="icon-md text-text-muted shrink-0" />
            <span>Hermix 有任务在跑时保持电脑唤醒（对话进行中或定时任务执行中）。</span>
          </div>
          <input
            type="checkbox"
            checked={keepAwake}
            onChange={(e) => toggleKeepAwake(e.target.checked)}
            className="rounded border-border-primary"
            aria-label="Hermix 有任务在跑时保持电脑唤醒"
          />
        </div>

        {/* 定时任务模板：一键预填编辑器，保存前可自由改。 */}
        <p className="text-[length:var(--text-ui)] text-text-muted mb-2">定时任务模板</p>
        <div className="grid grid-cols-1 md:grid-cols-2 gap-4 mb-6">
          {TEMPLATES.map((t) => {
            /* 大写化再渲染：JSX 里 `<t.icon />` 会被当成名为 "t.icon" 的
               HTML 标签（小写开头），React 会静默渲染成空元素。 */
            const Glyph = t.icon;
            return (
              <button
                key={t.name}
                onClick={() => setCreating(templateToTask(t))}
                className="m-lift text-left rounded-xl bg-bg-card p-3.5 hover:bg-bg-hover"
              >
                <div className="flex items-center gap-2">
                  <ToolCardIcon tone={t.tone}><Glyph className="icon-md" /></ToolCardIcon>
                  <span className="text-[length:var(--text-ui)] font-semibold text-text-primary">
                    {t.name}
                  </span>
                </div>
                <p className="mt-1 text-[length:var(--text-ui)] text-text-muted leading-snug line-clamp-2">
                  {t.description}
                </p>
                <p className="mt-1.5 text-[length:var(--text-ui)] text-text-faint">{t.scheduleLabel}</p>
              </button>
            );
          })}
        </div>
          </>
        )}
        </>
        ) : (
          <WorkflowPanel openChat={openChat} onCreateWorkflow={onCreateWorkflow} />
        )}
      </div>
      </div>

      {/* Editor modal */}
      {(editing || creating) && (
        <TaskEditor
          initial={editing ?? creating ?? blankDraft()}
          isCreate={creating !== null}
          onCancel={() => {
            setEditing(null);
            setCreating(null);
          }}
          onSave={(t) => void onSave(t, creating !== null)}
        />
      )}

      <ConfirmDialog
        open={confirmDelete !== null}
        title="删除自动化任务"
        body="确定要删除这条自动化任务吗？历史运行记录会一并清掉。"
        confirmLabel="删除"
        onCancel={() => setConfirmDelete(null)}
        onConfirm={() => {
          const id = confirmDelete;
          setConfirmDelete(null);
          if (id) void onDelete(id);
        }}
      />

      <ConfirmDialog
        open={confirmReset}
        title="恢复官方任务清单"
        body={`将删除全部 ${tasks.length} 个自动化任务（含运行历史），恢复为内置官方清单。`}
        confirmLabel="恢复官方清单"
        onCancel={() => setConfirmReset(false)}
        onConfirm={() => {
          setConfirmReset(false);
          void doReset();
        }}
      />
    </div>
  );
}

function blankDraft(): AutomationTask {
  return {
    id: "",
    name: "",
    prompt: "",
    schedule: "0 9 * * 1-5",
    enabled: false,
    category: "工作",
    /* 保留字段以兼容后端落库契约，但前端不再渲染 emoji（见 TaskCard）。 */
    icon: "",
    run_count: 0,
    history: [],
    created_at: "",
    updated_at: "",
  };
}

function TaskCard({
  task,
  onEdit,
  onDelete,
  onToggle,
  onTrigger,
}: {
  task: AutomationTask;
  onEdit: () => void;
  onDelete: () => void;
  onToggle: () => void;
  onTrigger: () => void;
}) {
  return (
    <div
      className={`relative flex flex-col gap-2 p-3.5 rounded-xl group ${
        task.enabled
          ? "bg-bg-card"
          : "bg-bg-secondary"
      }`}
    >
      <div className="flex items-start gap-2.5">
        {/* 2026-10-23：此前这里渲染 `task.icon`（一个 emoji 字符串），
            缺省还兜底 "⏰"。emoji 的四个硬伤：字形由系统字体决定（Win10 /
            Win11 两套风格）、尺寸跟随字号、颜色写死在字体里不跟主题、
            与旁边 lucide 描边图标不是同一视觉体系。改为按 category 推导
            色调的彩色图标块 + 固定的 Clock 图标（定时任务的语义恒等于时钟，
            不需要靠用户填字符串来表达）。旧任务里的 emoji 数据仍在库里，
            只是不再渲染。 */}
        <ToolCardIcon tone={task.category ? toneForCategory(task.category) : "amber"}>
          <Clock className="icon-md" />
        </ToolCardIcon>
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-1.5">
            <p className="text-[length:var(--text-ui)] font-semibold text-text-primary truncate">
              {task.name}
            </p>
            {task.last_status === "running" && (
              <span className="text-[length:var(--text-ui)] text-text-info animate-pulse">
                运行中
              </span>
            )}
            {task.last_status === "failed" && (
              <span className="text-[length:var(--text-ui)] text-text-error">失败</span>
            )}
          </div>
          {task.description && (
            <p className="text-[length:var(--text-ui)] text-text-muted leading-relaxed mt-0.5 line-clamp-2">
              {task.description}
            </p>
          )}
          <div className="flex items-center gap-2 mt-1.5 text-[length:var(--text-ui)] text-text-muted">
            <span className="font-mono">{task.schedule}</span>
            {task.schedule_label && <span>· {task.schedule_label}</span>}
          </div>
          <div className="flex items-center gap-2 mt-1 text-[length:var(--text-ui)] text-text-muted">
            <span>
              累计 {task.run_count} 次
              {task.max_runs ? ` / 上限 ${task.max_runs}` : ""}
            </span>
            {task.last_run_at && (
              <span>
                · 上次{" "}
                {new Date(task.last_run_at).toLocaleString("zh-CN", {
                  month: "numeric",
                  day: "numeric",
                  hour: "2-digit",
                  minute: "2-digit",
                })}
              </span>
            )}
          </div>
        </div>
      </div>

      {/* Action row */}
      <div className="flex items-center gap-1.5 -mb-1 mt-1">
        <button
          onClick={onTrigger}
          disabled={task.last_status === "running"}
          className="flex items-center gap-1 px-2 py-1 text-[length:var(--text-ui)] text-text-secondary hover:text-text-primary hover:bg-bg-hover rounded disabled:opacity-40"
          title="立刻跑一次"
        >
          <Play className="icon-xs" /> 立即运行
        </button>
        <button
          onClick={onEdit}
          className="flex items-center gap-1 px-2 py-1 text-[length:var(--text-ui)] text-text-secondary hover:text-text-primary hover:bg-bg-hover rounded"
          title="编辑"
        >
          <Pencil className="icon-xs" /> 编辑
        </button>
        <button
          onClick={onDelete}
          className="flex items-center gap-1 px-2 py-1 text-[length:var(--text-ui)] text-text-faint hover:text-text-error hover:bg-bg-hover rounded"
          title={DELETE}
          aria-label={DELETE}>
          <Trash2 className="icon-xs" />
        </button>
        <div className="flex-1" />
        <button
          onClick={onToggle}
          className={`flex items-center gap-1 px-2 py-1 text-[length:var(--text-ui)] rounded ${
            task.enabled
              ? "text-text-success hover:bg-bg-hover"
              : "text-text-faint hover:text-text-secondary hover:bg-bg-hover"
          }`}
          title={task.enabled ? "点击停用" : "点击启用"}
        >
          <Power className="icon-xs" />
          {task.enabled ? "已启用" : "已停用"}
        </button>
      </div>
    </div>
  );
}

function TaskEditor({
  initial,
  isCreate,
  onCancel,
  onSave,
}: {
  initial: AutomationTask;
  isCreate: boolean;
  onCancel: () => void;
  onSave: (t: AutomationTask) => void;
}) {
  const [draft, setDraft] = useState<AutomationTask>(initial);
  const [availableSkills, setAvailableSkills] = useState<
    Array<{ name: string; description: string; category: string }>
  >([]);
  /** 表单校验错误（内联展示，不用原生 alert —— 原生弹窗不跟随主题，
   *  且与全仓「确认/校验一律走应用内组件」的约定冲突）。 */
  const [formError, setFormError] = useState<string | null>(null);
  const update = <K extends keyof AutomationTask>(k: K, v: AutomationTask[K]) => {
    // 字段一改就撤掉上一次的校验错误 —— 否则错误横幅会一直挂着，
    // 用户已经补好了却还被告知「必填」。
    setFormError(null);
    setDraft((p) => ({ ...p, [k]: v }));
  };

  // fetch skill list for cron-job assignment.
  useEffect(() => {
    let cancelled = false;
    api.getSkillsList().then((skills) => {
      if (!cancelled) setAvailableSkills(skills);
    });
    return () => { cancelled = true; };
  }, []);

  const toggleSkill = (name: string) => {
    const current = draft.skills ?? [];
    if (current.includes(name)) {
      update("skills", current.filter((s) => s !== name));
    } else {
      update("skills", [...current, name]);
    }
  };

  const submit = () => {
    const missing: string[] = [];
    if (!draft.name.trim()) missing.push("名称");
    if (!draft.prompt.trim()) missing.push("Prompt");
    if (!draft.schedule.trim()) missing.push("Schedule");
    if (missing.length > 0) {
      setFormError(`${missing.join("、")}是必填的。补齐后即可保存。`);
      return;
    }
    setFormError(null);
    onSave(draft);
  };

  return (
    <Modal
      open
      onClose={onCancel}
      width="wide"
      scrim
      /* 表单弹窗：点遮罩不该把填了一半的任务扔掉。 */
      closeOnBackdrop={false} closeOnEscape={false}
      title={isCreate ? "新建自动化" : `编辑 · ${draft.name || "未命名"}`}
      bodyClassName="flex flex-col gap-3.5 overflow-y-auto px-5 pb-5"
      footer={
        <>
          <Button variant="secondary" size="base" onClick={onCancel}>
            取消
          </Button>
          <Button variant="primary" size="base" onClick={submit}>
            保存
          </Button>
        </>
      }
    >
        {formError && (
          <div
            role="alert"
            className="mb-3 flex items-start gap-2 rounded-lg bg-[var(--color-error-soft)] px-3 py-2 text-[length:var(--text-ui)] text-[var(--color-error)]"
          >
            <AlertTriangle className="icon-sm mt-0.5 shrink-0" aria-hidden="true" />
            <span>{formError}</span>
          </div>
        )}
        <Input
          label="名称"
          hint="给任务起个名字（卡片标题）"
          value={draft.name}
          onChange={(e) => update("name", e.target.value)}
          placeholder="每日早报"
        />
        <Input
          label="描述"
          hint="（可选）一句话讲清任务目的"
          value={draft.description ?? ""}
          onChange={(e) => update("description", e.target.value)}
          placeholder="每天早上整理昨天的工作进展"
        />
        <TextArea
          label="Prompt"
          hint="到点会原样发给 agent 的内容"
          rightAction={
            <AiGenerateButton
              draft={draft.prompt}
              onResult={(generated) => update("prompt", generated)}
              triggerKind="cron"
              scheduleHint={draft.schedule}
            />
          }
          value={draft.prompt}
          onChange={(e) => update("prompt", e.target.value)}
          rows={6}
          placeholder={"请帮我整理昨天的工作进展：\n1) 关键成果\n2) 阻塞项\n3) 今日建议"}
        />
        <div className="flex flex-col gap-1">
          <span className="text-[length:var(--text-ui)] font-medium text-[var(--color-text-primary)]">
            Schedule（cron 表达式）
          </span>
          <div className="flex flex-wrap gap-1 mb-1">
            {CRON_PRESETS.map(([label, expr, desc]) => (
              <button
                key={expr}
                onClick={() => update("schedule", expr)}
                className={`px-2 py-0.5 text-[length:var(--text-ui)] rounded ${
                  draft.schedule === expr
                    ? "bg-bg-active text-text-primary font-medium"
                    : "bg-bg-secondary text-text-secondary hover:bg-bg-hover"
                }`}
                title={desc}
              >
                {label}
              </button>
            ))}
          </div>
          <Input
            size="lg"
            labelHidden
            label="Schedule（cron 表达式）"
            hint="5 字段：分 时 日 月 周。例：0 9 * * 1-5 = 工作日 9 点"
            value={draft.schedule}
            onChange={(e) => update("schedule", e.target.value)}
            className="font-mono"
            placeholder="0 9 * * 1-5"
          />
        </div>
        {/* 原先这里并排「分类」与「图标 emoji」两个输入框。emoji 输入已删除：
            卡片图标改由分类推导色调（见 categoryTone），用户不必再手填一个
            字形怪异的字符。字段本身保留在数据层，不影响后端契约。 */}
        <Input
          label="分类"
          hint="（可选）决定卡片图标的色调"
          value={draft.category ?? ""}
          onChange={(e) => update("category", e.target.value)}
          placeholder="工作"
        />
        <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
          <Input
            label="模型"
            hint="（可选）provider:model"
            value={draft.model ?? ""}
            onChange={(e) => update("model", e.target.value || undefined)}
            className="font-mono"
            placeholder="openai:gpt-4o"
          />
          {/* 界面上说概念，不说字段名（问题 14）：后端仍收 default / full，
              落库契约不变；用户看到的是「标准 / 完全」。 */}
          <SelectField
            label="权限"
            hint="标准：危险工具需审批；完全：自动批准"
            value={draft.permission_level ?? "default"}
            onChange={(e) =>
              update(
                "permission_level",
                e.target.value as "default" | "full",
              )
            }
            options={[
              { value: "default", label: "标准（危险工具需审批）" },
              { value: "full", label: "完全（自动批准）" },
            ]}
          />
        </div>
        <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
          <Input
            label="运行次数上限"
            hint="（可选）累计跑满 N 次后自动停用；留空 = 无限循环"
            type="number"
            min={1}
            value={draft.max_runs ?? ""}
            onChange={(e) =>
              update(
                "max_runs",
                e.target.value.trim() === "" ? undefined : Math.max(1, Number(e.target.value)),
              )
            }
            placeholder="无限"
          />
          <div />
        </div>
        {/* skills multi-select for cron-job execution */}
        {availableSkills.length > 0 && (
          <FieldGroup label="技能（可选）" hint="勾选的技能会在任务执行时自动加载">
            <div className="flex flex-wrap gap-1.5 max-h-28 overflow-y-auto">
              {availableSkills.map((sk) => {
                const selected = (draft.skills ?? []).includes(sk.name);
                return (
                  <button
                    key={sk.name}
                    type="button"
                    onClick={() => toggleSkill(sk.name)}
                    title={sk.description}
                    /* 无描边：选中/未选中已经由 bg-active 与 bg-secondary 两档
                       底色区分，再加一圈边线是同一件事说两遍。 */
                    className={`px-2 py-1 text-[length:var(--text-ui)] rounded transition-colors ${
                      selected
                        ? "bg-bg-active text-text-primary font-medium"
                        : "bg-bg-secondary text-text-secondary hover:bg-bg-hover"
                    }`}
                  >
                    {sk.name}
                    {sk.category ? <span className="ml-1 text-[length:var(--text-ui)] opacity-60">({sk.category})</span> : null}
                  </button>
                );
              })}
            </div>
          </FieldGroup>
        )}
        <label className="flex items-center gap-2 text-[length:var(--text-ui)] text-text-secondary cursor-pointer select-none">
          <input
            type="checkbox"
            checked={draft.enabled}
            onChange={(e) => update("enabled", e.target.checked)}
            className="rounded border-border-primary"
          />
          创建后立即启用
        </label>
    </Modal>
  );
}

/**
 * 字段分组壳 —— 只给「一组控件」用（技能多选这种没有单一输入框的场合）。
 * 单个输入框一律走 `ui/Input` / `ui/TextArea` / `ui/SelectField`，
 * 那里自带 label↔控件的 `htmlFor`/`id` 关联与 `aria-describedby`。
 * 这里没有控件可关联，所以只给视觉分组 + 说明文字。
 */
function FieldGroup({
  label,
  hint,
  children,
}: {
  label: string;
  hint?: string;
  children: React.ReactNode;
}) {
  return (
    <div className="flex flex-col gap-1">
      <span className="text-[length:var(--text-ui)] font-medium text-[var(--color-text-primary)]">
        {label}
      </span>
      {children}
      {hint && (
        <p className="text-[length:var(--text-meta)] text-[var(--color-text-tertiary)]">{hint}</p>
      )}
    </div>
  );
}

// ── 工作流 tab：对话里保存的再跑流程 ─────────────────────────

function WorkflowPanel({ openChat, onCreateWorkflow }: { openChat: () => void; onCreateWorkflow: () => void }) {
  const { workspaces } = useWorkspaces();
  const [items, setItems] = useState<api.WorkflowEntry[] | null>(null);
  const [runningId, setRunningId] = useState<string | null>(null);
  const [paramFor, setParamFor] = useState<api.WorkflowEntry | null>(null);
  const [editing, setEditing] = useState<api.WorkflowEntry | null>(null);

  const load = useCallback(async () => {
    const list = await api.getWorkflows();
    setItems(list ?? []);
  }, []);
  useEffect(() => {
    void load();
  }, [load]);

  const doRun = async (wf: api.WorkflowEntry, params: Record<string, string>) => {
    setRunningId(wf.id);
    const ok = await api.runWorkflow(wf.id, params);
    setRunningId(null);
    setParamFor(null);
    if (ok) {
      toastInfo("已开始运行，跑完的会话在对话列表里。");
      void load();
      openChat();
    } else {
      toastError("运行失败");
    }
  };

  const onRun = (wf: api.WorkflowEntry) => {
    // prompt 里有 {{参数}} 占位符 → 先弹窗填参；没有就直接跑。
    const tokens = [
      ...new Set([...wf.prompt.matchAll(/\{\{\s*([^}]+?)\s*\}\}/g)].map((m) => m[1])),
    ];
    if (tokens.length === 0) void doRun(wf, {});
    else setParamFor(wf);
  };

  const wsName = (path: string) => workspaces.find((w) => w.path === path)?.name ?? path;
  const global = (items ?? []).filter((w) => !w.workspace);
  const byWs = new Map<string, api.WorkflowEntry[]>();
  for (const w of items ?? []) {
    if (!w.workspace) continue;
    const list = byWs.get(w.workspace) ?? [];
    list.push(w);
    byWs.set(w.workspace, list);
  }

  return (
    <div>
      <p className="text-sm text-text-muted mb-6">
        保存在本机的工作流：在对话里让 agent 设计流程，跑通之后让它用
        workflow 工具保存到这里，填好参数就能再跑一次。
      </p>

      <div className="flex items-center justify-between mb-2">
        <div className="flex items-center gap-2">
          <h2 className="text-[length:var(--text-ui)] font-medium text-text-primary">全局</h2>
          <span className="text-[length:var(--text-tiny)] text-text-faint">对所有项目可见</span>
        </div>
        <div className="flex items-center gap-2">
          <button
            onClick={() => setEditing({ id: "", name: "", description: "", workspace: null, prompt: "", created_at: 0, updated_at: 0 })}
            className="flex items-center gap-1.5 px-3 py-1.5 rounded-lg bg-accent text-bg-primary text-[length:var(--text-ui)] font-medium hover:bg-accent-hover"
          >
            <Plus className="icon-sm" /> 手动创建
          </button>
          <button
            onClick={onCreateWorkflow}
            className="flex items-center gap-1.5 px-3 py-1.5 rounded-lg bg-bg-secondary text-[length:var(--text-ui)] font-medium hover:bg-bg-hover"
          >
            通过对话创建
          </button>
        </div>
      </div>
      {!items ? (
        <div className="py-6 flex flex-col items-center gap-3">
          <SkeletonCard lines={2} />
        </div>
      ) : global.length === 0 ? (
        <p className="rounded-xl bg-bg-card px-4 py-6 text-center text-[length:var(--text-ui)] text-text-muted">
          还没有全局工作流。适合放不依赖某个项目的流程，例如深度调研。
        </p>
      ) : (
        <div className="grid grid-cols-1 md:grid-cols-2 gap-2.5 mb-6">
          {global.map((w) => (
            <WorkflowCard
              key={w.id}
              wf={w}
              runningId={runningId}
              onRun={() => onRun(w)}
              onDelete={async () => {
                if (await api.removeWorkflow(w.id)) void load();
              }}
            />
          ))}
        </div>
      )}

      {[...byWs.entries()].map(([path, list]) => (
        <div key={path} className="mb-6">
          <h2 className="text-[length:var(--text-ui)] font-medium text-text-primary mb-2">
            {wsName(path)}
          </h2>
          <div className="grid grid-cols-1 md:grid-cols-2 gap-2.5">
            {list.map((w) => (
              <WorkflowCard
                key={w.id}
                wf={w}
                runningId={runningId}
                onRun={() => onRun(w)}
                onDelete={async () => {
                  if (await api.removeWorkflow(w.id)) void load();
                }}
              />
            ))}
          </div>
        </div>
      ))}

      {paramFor && (
        <ParamDialog
          wf={paramFor}
          onCancel={() => setParamFor(null)}
          onRun={(params) => void doRun(paramFor, params)}
        />
      )}
      {editing && (
        <WorkflowEditorDialog
          wf={editing}
          workspaces={workspaces.map((w) => ({ path: w.path, name: w.name }))}
          onCancel={() => setEditing(null)}
          onSaved={async () => {
            setEditing(null);
            await load();
          }}
        />
      )}
    </div>
  );
}

/** 手动新建 / 编辑工作流。保存走 `POST /api/workflows`（upsert：
 *  id 为空即新建，非空即另存覆盖）。 */
function WorkflowEditorDialog({
  wf,
  workspaces,
  onCancel,
  onSaved,
}: {
  wf: api.WorkflowEntry;
  workspaces: Array<{ path: string; name: string }>;
  onCancel: () => void;
  onSaved: () => Promise<void>;
}) {
  const isNew = wf.id === "";
  const [name, setName] = useState(wf.name);
  const [description, setDescription] = useState(wf.description ?? "");
  const [prompt, setPrompt] = useState(wf.prompt);
  const [workspace, setWorkspace] = useState<string>(wf.workspace ?? "");
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const onSave = async () => {
    if (!name.trim()) {
      setError("名称不能为空");
      return;
    }
    if (!prompt.trim()) {
      setError("提示词不能为空");
      return;
    }
    setSaving(true);
    setError(null);
    const saved = await api.saveWorkflow({
      id: isNew ? undefined : wf.id,
      name: name.trim(),
      description: description.trim() || undefined,
      workspace: workspace || null,
      prompt,
    });
    setSaving(false);
    if (saved) {
      toastInfo(SAVED);
      await onSaved();
    } else {
      setError("保存失败，请检查后端日志");
    }
  };

  return (
    <Modal
      open
      onClose={onCancel}
      width="base"
      scrim
      closeOnBackdrop={false} closeOnEscape={false}
      title={isNew ? "新建工作流" : `编辑 · ${wf.name}`}
      bodyClassName="flex flex-col gap-3.5 overflow-y-auto px-5 pb-5"
      footer={
        <>
          <Button variant="secondary" size="base" onClick={onCancel}>
            取消
          </Button>
          <Button
            variant="primary"
            size="base"
            onClick={() => void onSave()}
            disabled={saving}
          >
            {saving ? SAVING : SAVE}
          </Button>
        </>
      }
    >
        <Input
          label="名称"
          value={name}
          onChange={(e) => setName(e.target.value)}
          placeholder="例如：竞品调研"
        />
        <Input
          label="说明（可选）"
          value={description}
          onChange={(e) => setDescription(e.target.value)}
          placeholder="一句话说明这个流程做什么"
        />
        <SelectField
          label="可见范围"
          value={workspace}
          onChange={(e) => setWorkspace(e.target.value)}
          options={[
            { value: "", label: "全局（对所有项目可见）" },
            ...workspaces.map((w) => ({ value: w.path, label: w.name })),
          ]}
        />
        <TextArea
          label="提示词"
          value={prompt}
          onChange={(e) => setPrompt(e.target.value)}
          className="min-h-40"
          placeholder={"跑起来后发给 agent 的指令。可用 {{参数}} 作为占位符，运行时再填。"}
        />
        {error && (
          <p className="text-[length:var(--text-ui)] text-text-error">{error}</p>
        )}
    </Modal>
  );
}

function WorkflowCard({
  wf,
  runningId,
  onRun,
  onDelete,
}: {
  wf: api.WorkflowEntry;
  runningId: string | null;
  onRun: () => void;
  onDelete: () => void;
}) {
  const running = runningId === wf.id || wf.last_status === "running";
  return (
    <div className="flex flex-col gap-2 p-3.5 rounded-xl bg-bg-card">
      <div className="flex items-start gap-2.5">
        {/* 与定时任务卡（Clock / amber）区分：工作流是「可复跑的流程」，
            用 Repeat2 + violet，两族卡片在混排时靠色相即可分辨。 */}
        <ToolCardIcon tone="violet"><Repeat2 className="icon-md" /></ToolCardIcon>
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-1.5">
            <p className="text-[length:var(--text-ui)] font-semibold text-text-primary truncate">
              {wf.name}
            </p>
            {running && (
              <span className="text-[length:var(--text-ui)] text-text-info animate-pulse">运行中</span>
            )}
            {wf.last_status === "failed" && (
              <span className="text-[length:var(--text-ui)] text-text-error">上次失败</span>
            )}
          </div>
          {wf.description && (
            <p className="text-[length:var(--text-ui)] text-text-muted leading-relaxed mt-0.5 line-clamp-2">
              {wf.description}
            </p>
          )}
          {wf.last_run_at && (
            <p className="mt-1 text-[length:var(--text-ui)] text-text-muted">
              上次{" "}
              {new Date(wf.last_run_at).toLocaleString("zh-CN", {
                month: "numeric",
                day: "numeric",
                hour: "2-digit",
                minute: "2-digit",
              })}
            </p>
          )}
        </div>
      </div>
      <div className="flex items-center gap-1.5 mt-1">
        <button
          onClick={onRun}
          disabled={running}
          className="flex items-center gap-1 px-2 py-1 text-[length:var(--text-ui)] text-text-secondary hover:text-text-primary hover:bg-bg-hover rounded disabled:opacity-40"
          title="再跑一次"
        >
          <Play className="icon-xs" /> 再跑一次
        </button>
        <div className="flex-1" />
        <button
          onClick={onDelete}
          className="px-2 py-1 rounded text-text-faint hover:text-text-error hover:bg-bg-hover"
          aria-label={DELETE}
          title={DELETE}
        >
          <Trash2 className="icon-xs" />
        </button>
      </div>
    </div>
  );
}

function ParamDialog({
  wf,
  onCancel,
  onRun,
}: {
  wf: api.WorkflowEntry;
  onCancel: () => void;
  onRun: (params: Record<string, string>) => void;
}) {
  const tokens = [
    ...new Set([...wf.prompt.matchAll(/\{\{\s*([^}]+?)\s*\}\}/g)].map((m) => m[1])),
  ];
  const [values, setValues] = useState<Record<string, string>>({});
  return (
    <Modal
      open
      onClose={onCancel}
      width="base"
      scrim
      closeOnBackdrop={false} closeOnEscape={false}
      title={`填参数 · ${wf.name}`}
      bodyClassName="flex flex-col gap-3.5 overflow-y-auto px-5 pb-5"
      footer={
        <>
          <Button variant="secondary" size="base" onClick={onCancel}>
            取消
          </Button>
          <Button variant="primary" size="base" onClick={() => onRun(values)}>
            开始运行
          </Button>
        </>
      }
    >
      {tokens.map((t) => (
        <Input
          key={t}
          label={t}
          value={values[t] ?? ""}
          onChange={(e) => setValues((p) => ({ ...p, [t]: e.target.value }))}
          placeholder={`填写 ${t}`}
        />
      ))}
    </Modal>
  );
}
