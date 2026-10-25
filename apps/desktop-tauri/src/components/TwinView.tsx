/**
 * 助手阿黎 —— 值守大屏。
 *
 * 版面是「中轴 + 两翼 + 底条」：
 *   左翼  她替你看着什么（任务 / 指标 / 最近会话 / 工作日志）
 *   中轴  双态面板：对话 ↔ 数据。默认对话；出现待审批时自动切到数据，
 *        点「回到对话」或点输入框切回（手动切回后 30s 内不再抢屏）
 *   右翼  现在要你做什么（待审批 / 刚发生的事 / 权限）
 *   底条  会话与快捷键
 *
 * 数据纪律：**只渲染真实数据**。情报源、决策队列、成长档案都还没有后端，
 * 对应位置显示「待接通」并写明将来会显示什么，不用示例数据填空 —— 用户
 * 看到的数据必须能当真。
 *
 * 真实数据源：
 *   - 自动化任务（`GET /api/automation/tasks`）→ 左翼任务条 + 中轴环
 *   - 在途运行数（`GET /api/automation/active-count`）
 *   - 最近会话（宿主 sessions prop）
 *   - 工作日志（`GET /api/memory/daily`）
 *   - 内联对话与审批（`twin` 这个独立发送器实例，会话 `twin:ali`）
 */
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { MarkdownBody } from "./MarkdownBody";
import { ThinkingDots } from "./ThinkingBubble";
import { MissionBoardView } from "./MissionBoardView";
import { AlertTriangle, X } from "lucide-react";

import * as api from "../api";
import type { AutomationTask, Session } from "../types";
import { AliCharacter, type AliState } from "./twin/AliCharacter";
import { TWIN_SESSION_ID, type TwinInlineApi } from "../app/routes";
import {
  mergeTranscript,
  toTranscriptRows,
  TRANSCRIPT_CAP,
  type TranscriptRow,
} from "../lib/twinTranscript";

/** 落库历史的轮询间隔。她这一轮跑着时暂停，避免轮询结果落后于屏幕造成回跳。 */
const HISTORY_POLL_MS = 8000;

/** 手动切回对话后，这段时间内不再被自动切走（防抢屏）。 */
const AUTO_SWITCH_COOLDOWN_MS = 30_000;

interface TwinViewProps {
  /** 宿主会话表 —— 「最近会话」用它，避免额外一次全量拉取。 */
  sessions: Session[];
  /** 去看 `twin:ali` 的完整对话（工具时间线等完整形态在那个页面）。 */
  onOpenChat: () => void;
  /**
   * 打开「最近会话」里的某一条（切过去并回到对话页）。
   *
   * 列表只摆标题不可点是 2026-10-25 用户实报的问题：值守页看到的会话
   * 必须能一键到达正文。
   */
  onOpenSession: (id: string) => void;
  /** 删除某条会话（宿主负责乐观更新会话表与激活态回退）。 */
  onDeleteSession: (id: string) => void;
  /**
   * 她自己的发送器（App 顶层的第二个 `useStreamChat` 实例）。
   *
   * 独立实例 = 与任何对话并行、互不禁用；本轮文本、未决审批、澄清都在
   * 这里就地渲染，不占宿主会话表。
   */
  twin: TwinInlineApi;
}

type Mode = "watch" | "dispatch" | "summary";

/** 中轴两态。数据态不是装饰：它承载「现在这一刻最该看的那个数」。 */
type CenterMode = "chat" | "data" | "tasks";

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
 * 小面板标题。大屏版不用图标：数据本身已经够密，再加图标只会和数字抢眼。
 * 一条 3px 的主色竖条 + 一行标签，是这套版面里唯一的「标题语法」。
 */
function BoxHead({ title, meta }: { title: string; meta?: React.ReactNode }) {
  return (
    <div className="twin-box__head">
      <span className="twin-box__bar" aria-hidden />
      <span className="twin-box__title">{title}</span>
      <span className="twin-box__spacer" />
      {meta != null ? <span className="twin-box__meta">{meta}</span> : null}
    </div>
  );
}

export function TwinView({ sessions, onOpenChat, onOpenSession, onDeleteSession, twin }: TwinViewProps) {
  const [tasks, setTasks] = useState<AutomationTask[]>([]);
  const [activeRuns, setActiveRuns] = useState(0);
  /** 最近的工作日志（daily_memory）—— 左翼与右翼的真实数据源之一。 */
  const [workDays, setWorkDays] = useState<api.DailyMemoryDay[]>([]);
  /** 落库历史（轮询拿到）。手机投递进来的轮次、App 重启前的旧轮次都在这。 */
  const [history, setHistory] = useState<TranscriptRow[]>([]);
  /** 内联口自己的提示：没发出去的原因。 */
  const [notice, setNotice] = useState<string | null>(null);
  /** 无选项澄清的就地输入（服务端要求自由文本时用）。 */
  const [clarifyDraft, setClarifyDraft] = useState<Record<string, string>>({});

  // 流式标志进 ref：轮询 effect 只挂一次，闭包里读不到最新值。
  const streamingRef = useRef(twin.streaming);
  useEffect(() => {
    streamingRef.current = twin.streaming;
  }, [twin.streaming]);

  // 落库历史：进入拉一次，之后低频轮询。她这一轮跑着时跳过——本轮内容在
  // `twin.turn` 里，轮询结果此刻落后于屏幕，覆盖会造成回跳。
  const mountedRef = useRef(true);
  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);
  const loadHistory = useCallback(async () => {
    if (streamingRef.current) return;
    try {
      const res = await api.getSessionMessages(TWIN_SESSION_ID, { limit: 12 });
      if (!mountedRef.current) return;
      setHistory(toTranscriptRows(res.messages).slice(-TRANSCRIPT_CAP));
    } catch {
      /* 保留上一份内容：抖动不该清空面板 */
    }
  }, []);

  useEffect(() => {
    void loadHistory();
    const t = setInterval(() => void loadHistory(), HISTORY_POLL_MS);
    return () => clearInterval(t);
  }, [loadHistory]);

  // 这一轮刚结束：立刻对齐一次落库历史，别让用户等下一个 8s 轮询
  // （那段时间里屏幕上还是内存里的两行，历史一到会闪一下）。
  const prevStreamingRef = useRef(twin.streaming);
  useEffect(() => {
    if (prevStreamingRef.current && !twin.streaming) void loadHistory();
    prevStreamingRef.current = twin.streaming;
  }, [twin.streaming, loadHistory]);

  /** 本轮在内存里的两行（她自己的发送器持有，不经过宿主会话表）。 */
  const liveRows = useMemo<TranscriptRow[]>(() => {
    if (!twin.turn) return [];
    const rows: TranscriptRow[] = [{ role: "user", text: twin.turn.user }];
    if (twin.turn.reply.trim()) rows.push({ role: "assistant", text: twin.turn.reply });
    return rows;
  }, [twin.turn]);

  const transcript = useMemo(
    () => mergeTranscript(history, liveRows),
    [history, liveRows],
  );

  /** 待你拍板的那件事（可能同时有审批 + 澄清，按到达顺序各占一行）。 */
  const pending = twin.pending;

  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [level, setLevel] = useState<"default" | "full">("default");
  const [mode, setMode] = useState<Mode>("summary");
  const [draft, setDraft] = useState("");
  /** 上次成功核对的时刻（ms）。给在场位显示「多久没看过了」。 */
  const [checkedAt, setCheckedAt] = useState<number | null>(null);
  /** 中轴两态。 */
  const [center, setCenter] = useState<CenterMode>("chat");
  /** 用户手动切回对话的时刻 —— 冷却期内不被自动切换抢屏。 */
  const manualAtRef = useRef(0);

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
      // 值守口径：daily_memory 的行首是 `[sessionId] …`。只有阿黎工作域
      // 的会话（她的对话 twin:ali / 自动化 automati* / 成员工单 member::）
      // 才进「最近工作日志」与「刚才发生」——普通主对话的用户消息流水
      // 会把面板刷成聊天记录（实测 smoke 测试会话泄漏进来）。
      const dutyScope = (d: api.DailyMemoryDay) =>
        /^\[(twin:|automati|member::)/m.test(
          `${d.goals}
${d.progress}
${d.conclusions}`,
        );
      setWorkDays(days.filter(dutyScope));
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
      // 值守口径（2026-10-25 用户验收）：这里只显示**阿黎工作域**的会话
      // ——她的对话（twin:）、成员工单（member::）、自动化触发的会话。
      // 全部主对话的 New chat 流水与值守总览无关，混进来就是噪音。
      sessions
        .filter(
          (s) =>
            s.id.startsWith("twin:") ||
            s.id.startsWith("member::") ||
            s.id.startsWith("automati"),
        )
        .sort((a, b) => (b.updated_at ?? "").localeCompare(a.updated_at ?? ""))
        .slice(0, 5),
    [sessions],
  );

  const stateText = twin.streaming
    ? "正在执行 · 你刚交代的这一轮"
    : activeRuns > 0
      ? `运行中 · ${activeRuns} 个任务在跑`
      : `静默观察 · ${enabledTasks.length} 个任务待触发`;

  // 角色姿态与"她在干什么"是同一件事的两个表达，必须同源。
  const aliState: AliState = error
    ? "alert"
    : twin.streaming || activeRuns > 0
      ? "running"
      : loading
        ? "thinking"
        : "idle";

  const presenceLine = error
    ? "核对任务状态时出错，右翼「刚才发生」上方是原始报错"
    : twin.streaming
      ? twin.turn?.activity
        ? `正在用 ${twin.turn.activity} 做事，回复会直接出现在中间`
        : "正在电脑上跑你刚交代的这一轮，回复会直接出现在中间"
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

  /**
   * 自动切到数据态：只在「出现新的待审批」时切一次。
   * 澄清（要你回答的问题）不切 —— 回答要用输入框，那是对话态的事。
   * 手动切回后 30s 内不抢屏，否则你在打字时会被反复拽走。
   */
  useEffect(() => {
    if (pending.length === 0) return;
    if (!pending.some((p) => p.kind === "approval")) return;
    if (Date.now() - manualAtRef.current < AUTO_SWITCH_COOLDOWN_MS) return;
    setCenter("data");
  }, [pending]);

  const goChat = useCallback(() => {
    manualAtRef.current = Date.now();
    setCenter("chat");
  }, []);

  /** 中轴环读的都是真实量：有 max_runs 才有百分比，否则只报次数。 */
  const runningTask = useMemo(
    () => enabledTasks.find((t) => t.last_status === "running") ?? null,
    [enabledTasks],
  );
  const runningRatio =
    runningTask?.max_runs && runningTask.max_runs > 0
      ? Math.min(1, runningTask.run_count / runningTask.max_runs)
      : null;
  const ringR = 130;
  const ringC = 2 * Math.PI * ringR;

  /** 发自这一轮。失败不吞：把原因留在输入口上方，草稿已清空可重打。 */
  const sendDraft = useCallback(async () => {
    const text = draft.trim();
    if (!text) return;
    if (twin.streaming) {
      setNotice("她正在跑上一轮，等这次结束再发");
      return;
    }
    setDraft("");
    setNotice(null);
    const ok = await twin.send(text, {
      permissionLevel: level,
      systemPrompt: MODE_PROMPT[mode],
    });
    if (!ok) setNotice("这一轮没发出去（原因见下方错误）");
  }, [draft, twin, level, mode]);

  const submit = (e: React.FormEvent) => {
    e.preventDefault();
    void sendDraft();
  };

  const sendDisabled = twin.streaming || draft.trim().length === 0;

  return (
    <div className="twin-screen">
      {/* ── 顶栏 ───────────────────────────────────────────── */}
      <header className="twin-head">
        <div className="twin-head__main">
          <h1 className="twin-head__title">阿黎</h1>
          <span
            className={`twin-head__state${activeRuns > 0 ? " is-live" : ""}`}
            title={stateText}
          >
            <i aria-hidden />
            {activeRuns > 0 ? "值守中" : "静默"}
          </span>
          <span className="twin-head__sub">{stateText}</span>
        </div>
        <div className="twin-head__side twin-head__side--r">
          <span className="twin-head__kv">
            在跑 <b>{activeRuns}</b>
          </span>
          <span className="twin-head__kv">
            已启用 <b>{enabledTasks.length}</b>
          </span>
          <span className="twin-head__kv">
            待批 <b className={pending.length > 0 ? "is-hot" : ""}>{pending.length}</b>
          </span>
          <button
            type="button"
            className="twin-head__perm"
            data-level={level}
            aria-label="切换权限级别"
            onClick={() => setLevel((prev) => (prev === "full" ? "default" : "full"))}
          >
            {level === "full" ? "放手模式" : "默认权限"}
          </button>
        </div>
      </header>

      <div className="twin-main">
        {/* ── 左翼：她替你看着什么 ─────────────────────────── */}
        <aside className="twin-wing">
          <section className="twin-box">
            <BoxHead title="值守任务" meta={`${enabledTasks.length} 个`} />
            {error ? (
              <p className="twin-error">
                <AlertTriangle className="icon-md" />
                <span>{error}</span>
              </p>
            ) : null}
            {loading && tasks.length === 0 ? (
              <p className="twin-muted">正在核对任务状态…</p>
            ) : null}
            {!loading && enabledTasks.length === 0 ? (
              <p className="twin-muted">
                还没有启用的任务。在「自动化」页或直接跟她说一句就能建。
              </p>
            ) : null}
            <div className="twin-jobs">
              {enabledTasks.map((t) => {
                const ratio =
                  t.max_runs && t.max_runs > 0
                    ? Math.min(1, t.run_count / t.max_runs)
                    : null;
                return (
                  <div key={t.id} className="twin-job">
                    <div className="twin-job__line">
                      <span className="twin-job__name">{t.name}</span>
                      <span
                        className={`twin-job__st${statusClass(t.last_status)}`}
                      >
                        {t.last_status ? (STATUS_TEXT[t.last_status] ?? t.last_status) : "未运行"}
                      </span>
                    </div>
                    <div className="twin-track">
                      <span
                        className="twin-track__fill"
                        style={{ "--p": ratio ?? 0 } as React.CSSProperties}
                      />
                    </div>
                    <div className="twin-job__meta">
                      <span className="twin-job__pct">
                        {ratio != null ? `${Math.round(ratio * 100)}%` : "—"}
                      </span>
                      <span>
                        {ratio != null ? `${t.run_count}/${t.max_runs}` : `第 ${t.run_count} 次`}
                      </span>
                      <span className="twin-spacer" />
                      <code>{t.schedule_label ?? t.schedule}</code>
                    </div>
                    {t.last_error ? (
                      <p className="twin-job__err">{t.last_error}</p>
                    ) : null}
                  </div>
                );
              })}
            </div>
          </section>

          <section className="twin-box">
            <BoxHead title="此刻" meta={checkedText} />
            <div className="twin-grid">
              <div>
                <div
                  className={`twin-grid__v${activeRuns > 0 ? " is-live" : " is-zero"}`}
                >
                  {activeRuns}
                </div>
                <div className="twin-grid__k">在跑</div>
              </div>
              <div>
                <div
                  className={`twin-grid__v${enabledTasks.length > 0 ? "" : " is-zero"}`}
                >
                  {enabledTasks.length}
                </div>
                <div className="twin-grid__k">已启用</div>
              </div>
              <div>
                <div className={`twin-grid__v${pending.length > 0 ? " is-hot" : " is-zero"}`}>
                  {pending.length}
                </div>
                <div className="twin-grid__k">待批</div>
              </div>
              <div>
                <div
                  className={`twin-grid__v${tasks.length > 0 ? "" : " is-zero"}`}
                >
                  {tasks.length}
                </div>
                <div className="twin-grid__k">任务总数</div>
              </div>
            </div>
          </section>

          <section className="twin-box twin-box--grow twin-box--quiet">
            <BoxHead title="最近会话" meta={`${recentSessions.length} 条`} />
            <div className="twin-rows">
              {recentSessions.length === 0 ? (
                <p className="twin-muted">还没有会话记录。</p>
              ) : (
                recentSessions.map((s) => (
                  <div key={s.id} className="twin-row twin-row--act">
                    <button
                      type="button"
                      className="twin-row__open"
                      title="打开这个会话"
                      onClick={() => onOpenSession(s.id)}
                    >
                      <span className="twin-row__t">{formatRelative(s.updated_at)}</span>
                      <span className="twin-row__m">{s.title || "未命名会话"}</span>
                      <span className="twin-row__k">
                        {typeof s.message_count === "number" ? `${s.message_count} 条` : "—"}
                      </span>
                    </button>
                    <button
                      type="button"
                      className="twin-row__del"
                      aria-label={`删除会话 ${s.title || s.id}`}
                      title="删除这个会话"
                      onClick={() => onDeleteSession(s.id)}
                    >
                      <X />
                    </button>
                  </div>
                ))
              )}
            </div>
          </section>

          <section className="twin-box twin-box--quiet">
            <BoxHead title="最近工作日志" meta={`${workDays.length} 条`} />
            <div className="twin-rows">
              {workDays.length === 0 ? (
                <p className="twin-muted">还没有值守日志。</p>
              ) : (
                workDays.slice(0, 3).map((d) => (
                  <div key={d.date} className="twin-row">
                    <span className="twin-row__t">{d.date.slice(5)}</span>
                    <span className="twin-row__m">
                      {firstLine(d.conclusions) ||
                        firstLine(d.progress) ||
                        firstLine(d.goals) ||
                        "这天没有留下结论"}
                    </span>
                    <span className="twin-row__k">日志</span>
                  </div>
                ))
              )}
            </div>
          </section>
        </aside>

        {/* ── 中轴：对话 ↔ 数据 ───────────────────────────── */}
        <section className="twin-center" data-mode={center}>
          <div className="twin-ctabs">
            <button
              type="button"
              className="twin-ctab"
              aria-pressed={center === "chat"}
              onClick={goChat}
            >
              对话
            </button>
            <button
              type="button"
              className="twin-ctab"
              aria-pressed={center === "data"}
              onClick={() => setCenter("data")}
            >
              数据
            </button>
            <button
              type="button"
              className="twin-ctab"
              aria-pressed={center === "tasks"}
              onClick={() => setCenter("tasks")}
            >
              任务
            </button>
            <span className="twin-ctabs__spacer" />
            {pending.length > 0 ? (
              center === "data" ? (
                <span className="twin-ctabs__note">
                  <i aria-hidden />
                  有 {pending.length} 件待办，已切到数据
                </span>
              ) : (
                <span className="twin-ctabs__note">
                  <i aria-hidden />
                  有 {pending.length} 件待办，点「数据」看详情
                </span>
              )
            ) : (
              <span className="twin-ctabs__note twin-ctabs__note--quiet">
                {presenceLine}
              </span>
            )}
            {center !== "chat" ? (
              <button type="button" className="twin-ctab twin-ctab--back" onClick={goChat}>
                回到对话
              </button>
            ) : null}
          </div>

          {center === "chat" ? (
            <div className="twin-chat">
              <div className="twin-msgs" aria-live="polite">
                {transcript.length === 0 && !twin.streaming ? (
                  <p className="twin-muted twin-msgs__empty">
                    她还没有说过话 · 在下面跟她说一句
                  </p>
                ) : null}
                {transcript.map((m, i) => (
                  <div
                    key={i}
                    className={`twin-msg${m.role === "user" ? " is-user" : ""}`}
                  >
                    {m.kind === "activity" ? (
                      <p className="twin-msg__act">{m.text}</p>
                    ) : (
                      <>
                        <span className="twin-msg__who">
                          {m.role === "user" ? "你" : "阿黎"}
                        </span>
                        {m.role === "user" ? (
                          <p className="twin-msg__tx">{m.text}</p>
                        ) : (
                          <MarkdownBody content={m.text} />
                        )}
                      </>
                    )}
                  </div>
                ))}
                {twin.streaming ? (
                  <div className="twin-msg is-live">
                    <ThinkingDots
                      label={
                        twin.turn?.activity ? `正在用 ${twin.turn.activity}` : "正在执行"
                      }
                    />
                  </div>
                ) : null}

                {/* 就地拍板：她这一轮在服务端停住等这个决定，
                    不答就等于 600s 后自动拒绝 —— 按钮必须在视线里。 */}
                {pending.map((p) =>
                  p.kind === "approval" ? (
                    <div key={p.id} className="twin-need" role="group" aria-label="待审批操作">
                      <span className="twin-need__tag">需要授权</span>
                      <span className="twin-need__text">
                        {p.tool}
                        {p.reason ? ` · ${p.reason}` : ""}
                      </span>
                      <span className="twin-need__actions">
                        <button
                          type="button"
                          className="chip-flat"
                          onClick={() => twin.resolve(p.id, "denied")}
                        >
                          拒绝
                        </button>
                        <button
                          type="button"
                          className="chip-flat chip-flat--on"
                          onClick={() => twin.resolve(p.id, "approved")}
                        >
                          允许
                        </button>
                      </span>
                    </div>
                  ) : (
                    <div key={p.id} className="twin-need" role="group" aria-label="待回答的问题">
                      <span className="twin-need__tag">需要你回答</span>
                      <span className="twin-need__text">{p.question}</span>
                      <span className="twin-need__actions">
                        {p.choices && p.choices.length > 0 ? (
                          p.choices.map((c) => (
                            <button
                              key={c}
                              type="button"
                              className="chip-flat"
                              onClick={() => twin.resolve(p.id, "approved", c)}
                            >
                              {c}
                            </button>
                          ))
                        ) : (
                          <>
                            <input
                              className="twin-need__input"
                              value={clarifyDraft[p.id] ?? ""}
                              onChange={(e) =>
                                setClarifyDraft((prev) => ({ ...prev, [p.id]: e.target.value }))
                              }
                              aria-label="回答她的问题"
                              placeholder="回答她"
                            />
                            <button
                              type="button"
                              className="chip-flat chip-flat--on"
                              onClick={() => twin.resolve(p.id, "approved", clarifyDraft[p.id] ?? "")}
                            >
                              答复
                            </button>
                          </>
                        )}
                      </span>
                    </div>
                  ),
                )}
              </div>

              {twin.turn?.error || notice ? (
                <p className="twin-error twin-error--inline" role="alert">
                  <AlertTriangle className="icon-md" />
                  <span>{twin.turn?.error ?? notice}</span>
                </p>
              ) : null}

              <form className="twin-cinput" onSubmit={submit}>
                <div className="twin-cfield">
                  <input
                    className="twin-cfield__input"
                    value={draft}
                    onChange={(e) => setDraft(e.target.value)}
                    onFocus={() => {
                      manualAtRef.current = Date.now();
                    }}
                    placeholder="跟她说点什么，或者直接派活"
                    aria-label="给助手阿黎发消息"
                    disabled={twin.streaming}
                  />
                  <div className="twin-modes" role="group" aria-label="动作模式">
                    {MODES.map((m) => (
                      <button
                        key={m.id}
                        type="button"
                        className="twin-mode"
                        aria-pressed={mode === m.id}
                        onClick={() => setMode(m.id)}
                      >
                        {m.label}
                      </button>
                    ))}
                  </div>
                  <button
                    type="submit"
                    className="twin-cfield__send"
                    aria-label="发送"
                    disabled={sendDisabled}
                  >
                    发送
                  </button>
                </div>
              </form>
            </div>
          ) : center === "tasks" ? (
            <MissionBoardView embedded />
          ) : (
            <div className="twin-data">
              <div className="twin-ring">
                <svg viewBox="0 0 320 320" aria-hidden>
                  <circle
                    cx="160"
                    cy="160"
                    r="152"
                    fill="none"
                    stroke="var(--color-border-primary)"
                    strokeWidth="1"
                  />
                  <circle
                    cx="160"
                    cy="160"
                    r={ringR}
                    fill="none"
                    stroke="var(--color-bg-tertiary)"
                    strokeWidth="14"
                  />
                  <circle
                    cx="160"
                    cy="160"
                    r={ringR}
                    fill="none"
                    stroke="var(--color-accent)"
                    strokeWidth="14"
                    strokeLinecap="round"
                    strokeDasharray={`${ringC * (runningRatio ?? 0)} ${ringC}`}
                    transform="rotate(-90 160 160)"
                  />
                  <circle
                    cx="160"
                    cy="160"
                    r="97"
                    fill="none"
                    stroke="var(--color-border-primary)"
                    strokeWidth="1"
                    strokeDasharray="2 10"
                  />
                </svg>
                <div className="twin-ring__ctr">
                  <div className="twin-ring__big">
                    {runningRatio != null ? Math.round(runningRatio * 100) : activeRuns}
                    {runningRatio != null ? <span>%</span> : null}
                  </div>
                  <div className="twin-ring__sub">
                    {runningRatio != null ? "本轮进度" : "个任务在跑"}
                  </div>
                  <div className="twin-ring__fig">
                    {runningTask
                      ? `${runningTask.name} · 第 ${runningTask.run_count} 次`
                      : "没有正在跑的任务"}
                  </div>
                </div>
              </div>

              <div className="twin-data__cap">
                <b>{twin.streaming ? "你刚交代的这一轮在跑" : stateText}</b>
                <span>{presenceLine}</span>
              </div>

              <div className="twin-strip">
                <div>
                  <div className="twin-strip__v">{activeRuns}</div>
                  <div className="twin-strip__k">在跑</div>
                </div>
                <div>
                  <div className="twin-strip__v">{enabledTasks.length}</div>
                  <div className="twin-strip__k">已启用</div>
                </div>
                <div>
                  <div className={`twin-strip__v${pending.length > 0 ? " is-hot" : ""}`}>
                    {pending.length}
                  </div>
                  <div className="twin-strip__k">待批</div>
                </div>
                <div>
                  <div className="twin-strip__v">{workDays.length}</div>
                  <div className="twin-strip__k">日志天</div>
                </div>
              </div>
            </div>
          )}
        </section>

        {/* ── 右翼：现在要你做什么 ─────────────────────────── */}
        <aside className="twin-wing twin-wing--r">
          <section className={`twin-box${pending.length > 0 ? " twin-box--attn" : ""}`}>
            <BoxHead title="等你决定" meta={`${pending.length} 件`} />
            {pending.length === 0 ? (
              <p className="twin-muted">队列是空的。有事她会直接在这儿问她。</p>
            ) : (
              <div className="twin-att">
                {pending.map((p) => (
                  <div key={p.id} className="twin-att__item">
                    <span className="twin-att__tag">
                      {p.kind === "approval" ? "需要授权" : "需要回答"}
                    </span>
                    <span className="twin-att__text">
                      {p.kind === "approval"
                        ? `${p.tool}${p.reason ? ` · ${p.reason}` : ""}`
                        : p.question}
                    </span>
                    <span className="twin-att__actions">
                      {p.kind === "approval" ? (
                        <>
                          <button
                            type="button"
                            className="chip-flat"
                            onClick={() => twin.resolve(p.id, "denied")}
                          >
                            拒绝
                          </button>
                          <button
                            type="button"
                            className="chip-flat chip-flat--on"
                            onClick={() => twin.resolve(p.id, "approved")}
                          >
                            允许
                          </button>
                        </>
                      ) : (
                        <button type="button" className="chip-flat" onClick={goChat}>
                          去回答
                        </button>
                      )}
                    </span>
                  </div>
                ))}
              </div>
            )}
          </section>

          <section className="twin-box twin-box--grow twin-box--quiet">
            <BoxHead title="刚才发生" meta={`${workDays.length} 条`} />
            <div className="twin-rows">
              {workDays.length === 0 ? (
                <p className="twin-muted">
                  她还没有值守记录。派成员、跑自动化或跟她聊过后，这里会出现工作日志。
                </p>
              ) : (
                workDays.slice(0, 8).map((d) => (
                  <div key={d.date} className="twin-row">
                    <span className="twin-row__t">{d.date.slice(5)}</span>
                    <span className="twin-row__m">
                      {firstLine(d.progress) ||
                        firstLine(d.conclusions) ||
                        firstLine(d.goals) ||
                        "这天没有留下记录"}
                    </span>
                    <span className="twin-row__k">日志</span>
                  </div>
                ))
              )}
            </div>
          </section>

          <section className="twin-box">
            <BoxHead title="她" />
            <div className="twin-who">
              <AliCharacter state={aliState} size={36} />
              <div className="twin-who__text">
                <span className="twin-who__name">阿黎</span>
                <span className="twin-who__line">{checkedText}</span>
              </div>
            </div>
            <div className="twin-who__acts">
              <button type="button" className="chip-flat" onClick={onOpenChat}>
                完整对话
              </button>
              {twin.streaming ? (
                <button type="button" className="chip-flat" onClick={twin.stop}>
                  停止这一轮
                </button>
              ) : null}
            </div>
          </section>
        </aside>
      </div>

      {/* ── 底条 ─────────────────────────────────────────── */}
      <footer className="twin-bottom">
        <span className="twin-bottom__live">
          <i aria-hidden />
          {twin.streaming ? "这一轮在电脑上执行中" : "在电脑上执行"}
        </span>
        <span>会话 {TWIN_SESSION_ID}</span>
        <span className="twin-spacer" />
        <span>
          <kbd>Enter</kbd> 发送
        </span>
        <span>{pending.length > 0 ? `有 ${pending.length} 件待办` : "没有待办"}</span>
      </footer>
    </div>
  );
}

function statusClass(status: AutomationTask["last_status"]): string {
  if (status === "failed") return " is-error";
  if (status === "running") return " is-run";
  if (status === "ok") return " is-ok";
  return " is-idle";
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
