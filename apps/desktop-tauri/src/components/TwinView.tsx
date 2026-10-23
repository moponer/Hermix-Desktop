/**
 * 助手阿黎 —— 常驻助理的主界面。
 *
 * 这一页不是对话页：三栏信息板（新鲜情报 / 正在执行 / 待你定夺）+ 底部
 * 单行输入口。设计依据见
 * docs/notes/proposed/feature/2026-10-21-digital-twin-assistant.md。
 *
 * 数据纪律：**只渲染真实数据**。阿黎自己的情报源、决策队列、成长档案
 * 都还没有后端，这三块一律显示「待接通」并说明将来会显示什么，不用
 * 示例数据填空——用户看到的数据必须能当真。
 *
 * 当前真实数据源：
 *   - 自动化任务（`GET /api/automation/tasks`）→ 正在执行
 *   - 在途运行数（`GET /api/automation/active-count`）→ 顶栏读数
 *   - 最近会话（宿主 sessions prop）→ 新鲜情报栏的补充，标明是替代源
 *
 * 内联对话（2026-10-23）：输入口投到 `twin:ali` 这一条会话，回复就地显示，
 * 不把用户踢去对话页。数据两条来源合并——宿主会话表里那条（本轮乐观插入
 * 与流式增量，App 的发送链路写入）+ 一次轻轮询拉到的落库历史（覆盖手机
 * 投递进来的轮次、以及 App 重启后的旧历史）。
 */
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  AlertTriangle, BookOpen, Brain, Clock3, ListChecks, Radio, Sparkles,
} from "lucide-react";

import * as api from "../api";
import type { AutomationTask, Session } from "../types";
import type { StreamErrorKind } from "../app/streamEvents";
import { PageHeader, SkeletonCard } from "./ui";
import { AliCharacter, type AliState } from "./twin/AliCharacter";
import { toneClass, type IconTone } from "../lib/categoryTone";
import { TWIN_SESSION_ID, type TwinSendOptions } from "../app/routes";
import {
  mergeTranscript,
  toTranscriptRows,
  TRANSCRIPT_CAP,
  type TranscriptRow,
} from "../lib/twinTranscript";

/** 落库历史的轮询间隔。流式期间暂停，避免和 App 的增量抢同一块显示。 */
const HISTORY_POLL_MS = 8000;

interface TwinViewProps {
  /** 宿主会话表 —— 「最近活跃」用它，避免额外一次全量拉取。 */
  sessions: Session[];
  /** 去看 `twin:ali` 的完整对话（工具时间线、审批卡片在那边的对话页里）。 */
  onOpenChat: () => void;
  /** 投递这一轮到 `twin:ali`；false = 没发出去（错误已由 App 落到 streamError）。 */
  onSend: (text: string, opts: TwinSendOptions) => Promise<boolean>;
  /** 这条会话的错误（按会话隔离，不串到别的对话）。 */
  streamError: { message: string; detail?: string; kind?: StreamErrorKind } | null;
  /** `twin:ali` 正在跑这一轮。 */
  streaming: boolean;
  /** 另一条会话正在流式：发送器是进程级单流，此时内联口不能发。 */
  anotherSessionStreaming: boolean;
}

type Mode = "watch" | "dispatch" | "summary";

const MODES: { id: Mode; label: string }[] = [
  { id: "watch", label: "盯住" },
  { id: "dispatch", label: "派活" },
  { id: "summary", label: "总结" },
];

const STATUS_TEXT: Record<string, string> = {
  ok: "上次成功",
  failed: "上次失败",
  running: "运行中",
};

/**
 * 动作模式 → 这一轮的指令。
 *
 * 三个 chip 是控件，不是装饰：不落到请求里就是假按钮。用 `systemPrompt`
 * （前置块）而不是替换人格提示 —— 模式调的是这一轮的重心，不是她是谁。
 */
const MODE_PROMPT: Record<Mode, string> = {
  watch: "这一轮以「盯住」为主：只报新变化与异常，不扩大范围，不主动起新任务。",
  dispatch: "这一轮以「派活」为主：把事派出去执行（能派子 agent 就派），完成后汇总结果与还差什么。",
  summary: "这一轮以「总结」为主：先给结论，再列依据；不展开无关细节。",
};

/**
 * 栏头：图标 + 标题 + 计数。
 *
 * 为什么必须带图标（2026-10-23）：三栏并排时，栏与栏之间只有一条发丝线，
 * 扫视全靠读文字。给每栏一个**按职能上色**的图标后，色相成为第一线索 ——
 * 「蓝色是情报（外部输入）、琥珀是执行（在跑的事）、玫瑰是定夺（要你决策）」
 * 这套对应关系在整页里保持一致。
 */
function ColHead({
  icon: Icon, tone, title, meta,
}: {
  icon: React.ElementType;
  tone: IconTone;
  title: string;
  meta?: React.ReactNode;
}) {
  return (
    <div className="twin-col__head">
      <span className="twin-col__label">
        <span className={["cat-tile cat-tile--sm", toneClass(tone, "cat-tile")].join(" ")}>
          <Icon className="icon-sm" />
        </span>
        <span className="twin-col__title">{title}</span>
      </span>
      {meta != null ? <span className="twin-col__meta">{meta}</span> : null}
    </div>
  );
}

export function TwinView({
  sessions,
  onOpenChat,
  onSend,
  streamError,
  streaming,
  anotherSessionStreaming,
}: TwinViewProps) {
  const [tasks, setTasks] = useState<AutomationTask[]>([]);
  const [activeRuns, setActiveRuns] = useState(0);
  /** 最近的工作日志（daily_memory）—— 三栏右侧的真实数据源之一。 */
  const [workDays, setWorkDays] = useState<api.DailyMemoryDay[]>([]);
  /** 落库历史（轮询拿到）。手机投递进来的轮次只有这条路能看见。 */
  const [history, setHistory] = useState<TranscriptRow[]>([]);
  /** 内联口自己的提示：没发出去的原因 / 单流约束。 */
  const [notice, setNotice] = useState<string | null>(null);

  // 流式标志进 ref：轮询 effect 只挂一次，闭包里读不到最新 prop。
  const streamingRef = useRef(streaming);
  useEffect(() => {
    streamingRef.current = streaming;
  }, [streaming]);

  // 落库历史：进入拉一次，之后低频轮询。流式期间跳过——这一轮的内容由
  // App 的增量写在宿主会话表里，轮询结果此时落后于屏幕，覆盖会造成回跳。
  useEffect(() => {
    let alive = true;
    const load = async () => {
      if (streamingRef.current) return;
      try {
        const res = await api.getSessionMessages(TWIN_SESSION_ID, { limit: 12 });
        if (!alive) return;
        setHistory(toTranscriptRows(res.messages).slice(-TRANSCRIPT_CAP));
      } catch {
        /* 保留上一份内容：抖动不该清空面板 */
      }
    };
    void load();
    const t = setInterval(() => void load(), HISTORY_POLL_MS);
    return () => {
      alive = false;
      clearInterval(t);
    };
  }, []);

  const twinSession = useMemo(
    () => sessions.find((s) => s.id === TWIN_SESSION_ID),
    [sessions],
  );
  /** 本轮在内存里的消息（乐观 user + 流式 assistant），App 的发送链路写入。 */
  const liveRows = useMemo(
    () => toTranscriptRows(twinSession?.messages ?? []),
    [twinSession?.messages],
  );
  const transcript = useMemo(
    () => mergeTranscript(history, liveRows),
    [history, liveRows],
  );

  /**
   * 等她拍板的事：未决审批 / 未答澄清。
   *
   * 内联面板不渲染审批卡片（那是对话页的完整组件），但**必须显示**——
   * 否则工具调用会在服务端空等到超时自动拒绝，用户只看到"她卡住了"。
   */
  const waitingFor = useMemo(() => {
    const msgs = twinSession?.messages ?? [];
    for (let i = msgs.length - 1; i >= 0; i--) {
      const m = msgs[i];
      if (m.role === "clarify" && !m.clarify?.answer) return "澄清";
      for (const step of m.unified_steps ?? []) {
        if (
          (step.kind === "approval_request" || step.kind === "approval_request_batch") &&
          !step.meta?.decision
        ) {
          return step.kind === "approval_request" ? "审批" : "批量审批";
        }
      }
    }
    return null;
  }, [twinSession?.messages]);

  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [level, setLevel] = useState<"default" | "full">("default");
  const [mode, setMode] = useState<Mode>("summary");
  const [draft, setDraft] = useState("");
  /** 上次成功核对的时刻（ms）。给在场位显示「多久没看过了」。 */
  const [checkedAt, setCheckedAt] = useState<number | null>(null);

  const refresh = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const [list, active, days] = await Promise.all([
        api.getAutomationTasks(),
        api.getActiveAutomationRuns(),
        api.getDailyMemoryList(14).catch(() => [] as api.DailyMemoryDay[]),
      ]);
      setTasks(list);
      setActiveRuns(active);
      setWorkDays(days);
      setCheckedAt(Date.now());
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const enabledTasks = useMemo(() => {
    const list = tasks.filter((t) => t.enabled);
    // 有在跑的排前面，其余按上次运行时间倒序（没有运行过的殿后）。
    return list.sort((a, b) => {
      const ra = a.last_status === "running" ? 1 : 0;
      const rb = b.last_status === "running" ? 1 : 0;
      if (ra !== rb) return rb - ra;
      return (b.last_run_at ?? "").localeCompare(a.last_run_at ?? "");
    });
  }, [tasks]);

  const recentSessions = useMemo(
    () =>
      // 排除她自己的会话：`twin:ali` 一旦发过消息就进了宿主会话表，混进
      // 「最近活跃」会变成"她自己的对话出现在她的近期情报里"。
      sessions
        .filter((s) => !s.id.startsWith("twin:"))
        .sort((a, b) => (b.updated_at ?? "").localeCompare(a.updated_at ?? ""))
        .slice(0, 5),
    [sessions],
  );

  const stateText = streaming
    ? "正在执行 · 你刚交代的这一轮"
    : activeRuns > 0
      ? `运行中 · ${activeRuns} 个任务在跑`
      : `静默观察 · ${enabledTasks.length} 个任务待触发`;

  // 角色姿态与"她在干什么"是同一件事的两个表达，必须同源。
  const aliState: AliState = error
    ? "alert"
    : streaming || activeRuns > 0
      ? "running"
      : loading
        ? "thinking"
        : "idle";

  const presenceLine = error
    ? "核对任务状态时出错，右侧「正在执行」里是原始报错"
    : streaming
      ? "正在电脑上跑你刚交代的这一轮，回复会直接出现在下面"
      : loading
        ? "正在核对任务状态"
        : activeRuns > 0
          ? `盯着 ${activeRuns} 个在跑的任务，异常才出声`
          : enabledTasks.length > 0
            ? `${enabledTasks.length} 个任务待触发，暂时没有要打扰你的事`
            : "还没有启用的任务，我这儿暂时没什么可盯的";

  const checkedText =
    checkedAt == null
      ? "尚未核对"
      : Date.now() - checkedAt < 60_000
        ? "刚刚核对"
        : `${Math.round((Date.now() - checkedAt) / 60_000)} 分钟前核对`;

  /** 发自这一轮。失败不吞：把原因留在输入口上方，草稿已清空可重打。 */
  const sendDraft = useCallback(async () => {
    const text = draft.trim();
    if (!text) return;
    if (anotherSessionStreaming) {
      setNotice("另一条对话正在执行——她这儿一次只跑一轮，等它结束再发");
      return;
    }
    if (streaming) {
      setNotice("她正在跑上一轮，等这次结束再发");
      return;
    }
    setDraft("");
    setNotice(null);
    const ok = await onSend(text, {
      permissionLevel: level,
      systemPrompt: MODE_PROMPT[mode],
    });
    if (!ok) setNotice("这一轮没发出去（原因见下方错误）");
  }, [draft, anotherSessionStreaming, streaming, onSend, level, mode]);

  const submit = (e: React.FormEvent) => {
    e.preventDefault();
    void sendDraft();
  };

  const sendDisabled = anotherSessionStreaming || streaming || draft.trim().length === 0;

  return (
    <div className="twin-view">
      <PageHeader
        icon={<Sparkles className="icon-lg" />}
        title="助手阿黎"
        children={
          <span className="twin-state">
            <span
              className={
                activeRuns > 0 ? "twin-dot" : "twin-dot twin-dot--idle"
              }
              aria-hidden
            />
            {stateText}
          </span>
        }
        actions={
          <>
            <div className="twin-readouts">
              <div className="twin-readout">
                <span className="twin-readout__label">在跑</span>
                <span className="twin-readout__value">{activeRuns}</span>
              </div>
              <div className="twin-readout">
                <span className="twin-readout__label">已启用</span>
                <span className="twin-readout__value">
                  {enabledTasks.length}
                  <small>个</small>
                </span>
              </div>
              <div className="twin-readout">
                <span className="twin-readout__label">待你定夺</span>
                <span className="twin-readout__value">—</span>
              </div>
            </div>
            <button
              type="button"
              className="twin-perm"
              data-level={level}
              aria-label="切换权限级别"
              onClick={() =>
                setLevel((prev) => (prev === "full" ? "default" : "full"))
              }
            >
              <span className="twin-perm__label">权限</span>
              <span className="twin-perm__value">
                {level === "full" ? "放手模式" : "默认"}
              </span>
            </button>
          </>
        }
      />

      {/* 在场位：角色姿态与文案同源，异常/思考/在跑三种状态各自有表情 */}
      {/* 在场位：形象 + 名字 + 当前姿态 + 上次核对时间。
          姿态文案（presenceLine）与形象状态（aliState）同源派生，不会出现
          「表情在笑、文案报错」。 */}
      <div className="twin-presence">
        <AliCharacter state={aliState} size={48} />
        <div className="twin-presence__text">
          <span className="twin-presence__name">阿黎</span>
          <span className="twin-presence__line">{presenceLine}</span>
        </div>
        <span className="twin-presence__checked">
          <Brain className="cat-ink cat-ink--violet icon-sm" aria-hidden />
          {checkedText}
        </span>
      </div>

      <div className="twin-board">
        {/* ── 一、新鲜情报 ── */}
        <section className="twin-col">
          <ColHead icon={Radio} tone="blue" title="新鲜情报" meta="待接通" />
          <div className="twin-col__body">
            <Pending
              title="情报源尚未接通"
              desc="接通后这里显示她替你盯到的新变化：只收「新」且与你在意的项目相关的，重复内容进笔记不再打扰。"
              items={[
                "订阅源水位线（RSS 取 last guid、页面取内容指纹）",
                "每条带一句「为什么给你看」的依据",
              ]}
            />

            <ColHead
              icon={Clock3}
              tone="cyan"
              title="最近活跃"
              meta={`${recentSessions.length} 条`}
            />
            {recentSessions.length === 0 ? (
              <p className="twin-pending__desc">还没有会话记录。</p>
            ) : (
              recentSessions.map((s) => (
                <article key={s.id} className="twin-card">
                  <div className="twin-card__top">
                    <span className="twin-card__source">会话</span>
                    <span className="twin-card__age">
                      {formatRelative(s.updated_at)}
                    </span>
                  </div>
                  <div className="twin-card__title">{s.title || "未命名会话"}</div>
                  <div className="twin-card__foot">
                    {s.project ? (
                      <span className="twin-tag">{s.project}</span>
                    ) : null}
                    {typeof s.message_count === "number" ? (
                      <span className="twin-tag">{s.message_count} 条消息</span>
                    ) : null}
                  </div>
                </article>
              ))
            )}
          </div>
        </section>

        {/* ── 二、正在执行 ── */}
        <section className="twin-col">
          <ColHead
            icon={ListChecks}
            tone="amber"
            title="正在执行"
            meta={`${activeRuns} 跑 · ${enabledTasks.length} 启用`}
          />
          <div className="twin-col__body">
            {error ? (
              <p className="twin-error">
                <AlertTriangle className="icon-md" />
                <span>{error}</span>
              </p>
            ) : null}

            {loading && tasks.length === 0 ? (
              <>
                <SkeletonCard lines={2} />
                <SkeletonCard lines={2} />
              </>
            ) : null}

            {!loading && enabledTasks.length === 0 ? (
              <Pending
                title="没有启用的自动化任务"
                desc="在「自动化」页或直接跟她说话建立任务；启用后会在这里看到运行状态、已跑次数与下次触发时间。"
              />
            ) : null}

            {enabledTasks.map((t) => {
              const ratio =
                t.max_runs && t.max_runs > 0
                  ? Math.min(1, t.run_count / t.max_runs)
                  : null;
              return (
                <article key={t.id} className="twin-task">
                  <div className="twin-task__row">
                    <span className="twin-task__name">{t.name}</span>
                    <span className="twin-task__pct">
                      {ratio != null
                        ? `${t.run_count}/${t.max_runs}`
                        : `第 ${t.run_count} 次`}
                    </span>
                  </div>
                  {ratio != null ? (
                    <div className="twin-bar">
                      <span
                        className="twin-bar__fill"
                        style={{ "--p": ratio } as React.CSSProperties}
                      />
                    </div>
                  ) : null}
                  <div className="twin-task__sub">
                    <span
                      className={`twin-dot${statusDotClass(t.last_status)}`}
                      aria-hidden
                    />
                    <span>
                      {t.last_status
                        ? (STATUS_TEXT[t.last_status] ?? t.last_status)
                        : "尚未运行"}
                    </span>
                    <code>{t.schedule_label ?? t.schedule}</code>
                    {t.next_run_at ? (
                      <span>下次 {formatRelative(t.next_run_at)}</span>
                    ) : null}
                  </div>
                  {t.last_error ? (
                    <p className="twin-card__note">{t.last_error}</p>
                  ) : null}
                </article>
              );
            })}
          </div>
        </section>

        {/* ── 三、待你定夺 ── */}
        <section className="twin-col twin-col--third">
          <ColHead icon={Sparkles} tone="rose" title="待你定夺" meta="待接通" />
          <div className="twin-col__body">
            <Pending
              title="决策队列尚未接通"
              desc="接通后，她需要你拍板的事会出现在这里，每条都附一句依据——你要能一眼看出她凭什么问这个。"
              items={[
                "「把这件事转成每天 9 点的常驻任务？」",
                "「外部 CLI 请求删除 3 个已合并分支」",
              ]}
            />

            <ColHead
              icon={BookOpen}
              tone="green"
              title="最近工作日志"
              meta={`${workDays.length} 天`}
            />
            {workDays.length === 0 ? (
              <Pending
                title="还没有工作日志"
                desc="daily_memory 里没有记录。任务跑起来后，她会在每天的目标 / 进展 / 结论里留下痕迹，这里读的就是同一份数据。"
              />
            ) : (
              <div className="twin-learn">
                {workDays.slice(0, 4).map((d) => (
                  <div key={d.date} className="twin-learn__item">
                    <span className="twin-learn__ev">{d.date.slice(5)}</span>
                    <span>
                      {firstLine(d.conclusions) ||
                        firstLine(d.progress) ||
                        firstLine(d.goals) ||
                        "这天没有留下结论"}
                    </span>
                  </div>
                ))}
              </div>
            )}

            <Pending
              title="成长档案待接通"
              desc="上面是她的工作日志。真正「学到你」的部分（偏好、判断准不准）要等 profile 与证据账本落地，现在不假装有。"
            />
          </div>
        </section>
      </div>

      {/* 内联对话：她的最近几条就地显示——输入后不用跳去对话页看她说了什么。 */}
      <div className="twin-transcript" aria-live="polite">
        {transcript.length === 0 && !streaming ? (
          <div className="twin-transcript__empty">她还没有说过话 · 在下面跟她说一句</div>
        ) : (
          transcript.map((m, i) => (
            <div key={i} className={`twin-transcript__row${m.role === "user" ? " is-user" : ""}`}>
              <span className="twin-transcript__who">{m.role === "user" ? "你" : "阿黎"}</span>
              <span className="twin-transcript__text">{m.text}</span>
            </div>
          ))
        )}
        {streaming ? (
          <div className="twin-transcript__row is-live">
            <span className="twin-transcript__who">阿黎</span>
            <span className="twin-transcript__text twin-transcript__text--live">正在执行</span>
          </div>
        ) : null}
      </div>

      {/* 等她拍板：内联面板不渲染审批/澄清卡片，但必须把人送到能处理的地方 ——
          否则工具调用在服务端空等到超时自动拒绝。 */}
      {waitingFor ? (
        <div className="twin-need" role="status">
          <span className="twin-need__text">她卡在一步需要你{waitingFor}的操作上</span>
          <button type="button" className="chip-flat" onClick={onOpenChat}>
            去对话页处理
          </button>
        </div>
      ) : null}

      {streamError || notice ? (
        <p className="twin-error twin-error--inline" role="alert">
          <AlertTriangle className="icon-md" />
          <span>{streamError?.message ?? notice}</span>
        </p>
      ) : null}

      <form className="twin-dock" onSubmit={submit}>
        <div className="twin-dock__hint">
          <span>
            {anotherSessionStreaming
              ? "另一条对话正在执行 · 她这儿一次只跑一轮"
              : streaming
                ? "这一轮在电脑上执行中 · 工具 / 工作区 / 派 agent 都在跑"
                : "在电脑上执行 · 工具 / 工作区 / 派 agent 都可用"}
          </span>
          <span>·</span>
          <kbd>Enter</kbd>
          <span>发送</span>
        </div>
        <div className="twin-dock__row">
          <span className="twin-dock__caret" aria-hidden>
            ›
          </span>
          <input
            className="twin-dock__input"
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            placeholder={
              anotherSessionStreaming ? "等另一条对话跑完再发" : "跟她说点什么，或者直接派活"
            }
            aria-label="给助手阿黎发消息"
            disabled={streaming || anotherSessionStreaming}
          />
          <div className="twin-dock__chips" role="group" aria-label="动作模式">
            {MODES.map((m) => (
              <button
                key={m.id}
                type="button"
                className="twin-chip"
                aria-pressed={mode === m.id}
                onClick={() => setMode(m.id)}
              >
                {m.label}
              </button>
            ))}
          </div>
          <button type="submit" className="twin-send" aria-label="发送" disabled={sendDisabled}>
            ↑
          </button>
        </div>
      </form>
    </div>
  );
}

/** 未接通模块：说清「将来显示什么」，不给假数据。 */
function Pending({
  title,
  desc,
  items,
}: {
  title: string;
  desc: string;
  items?: string[];
}) {
  return (
    <div className="twin-pending">
      <div className="twin-pending__title">{title}</div>
      <div className="twin-pending__desc">{desc}</div>
      {items && items.length > 0 ? (
        <ul className="twin-pending__list">
          {items.map((item) => (
            <li key={item}>{item}</li>
          ))}
        </ul>
      ) : null}
    </div>
  );
}

function statusDotClass(status: AutomationTask["last_status"]): string {
  if (status === "failed") return " twin-dot--error";
  if (status === "ok" || status === "running") return "";
  return " twin-dot--idle";
}

/** 取首行非空文本并裁到 80 字，把一天的三段日志压成一行。 */
function firstLine(text: string | undefined): string {
  if (!text) return "";
  const line =
    text
      .split(/\r?\n/)
      .map((s) => s.trim())
      .find((s) => s.length > 0) ?? "";
  if (!line) return "";
  return line.length > 80 ? `${line.slice(0, 78)}…` : line;
}

/** 相对时间。后端给的是 RFC3339（带 Z）。 */
function formatRelative(iso: string | undefined): string {
  if (!iso) return "—";
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return "—";
  const diffMin = Math.round((Date.now() - t) / 60_000);
  if (diffMin < 0) {
    const ahead = Math.abs(diffMin);
    if (ahead < 60) return `${ahead} 分钟后`;
    if (ahead < 60 * 24) return `${Math.round(ahead / 60)} 小时后`;
    return `${Math.round(ahead / (60 * 24))} 天后`;
  }
  if (diffMin < 1) return "刚刚";
  if (diffMin < 60) return `${diffMin} 分钟前`;
  if (diffMin < 60 * 24) return `${Math.round(diffMin / 60)} 小时前`;
  return `${Math.round(diffMin / (60 * 24))} 天前`;
}
