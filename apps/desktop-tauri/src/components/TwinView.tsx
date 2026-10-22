/**
 * 助手阿黎 —— 常驻助理的主界面。
 *
 * 这一页不是对话页：三栏信息板（新鲜情报 / 正在执行 / 待你定夺）+ 底部
 * 单行输入口。设计依据见
 * docs/notes/proposed/feature/2026-10-21-digital-twin-assistant.md。
 *
 * 数据纪律：**只渲染真实数据**。阿黎的情报源与成长档案还没有后端，这两块
 * 一律显示「待接通」并说明将来会显示什么，不用示例数据填空——用户看到的
 * 数据必须能当真。
 *
 * 当前真实数据源：
 *   - 自动化任务（`GET /api/automation/tasks`）→ 正在执行 + 正在盯
 *   - 在途运行数（`GET /api/automation/active-count`）→ 顶栏读数
 *   - 最近会话（宿主 sessions prop）→ 新鲜情报栏的补充，标明是替代源
 *   - 挂起审批（`GET /v1/sessions/{id}/approvals/pending`）→ 待你定夺
 *   - 工作日志（`GET /api/memory/daily`）→ 最近工作日志
 *   - 会话通道（`POST /api/twin/message`）→ 底部输入口
 */
import { useCallback, useEffect, useMemo, useState } from "react";
import { AlertTriangle, Sparkles } from "lucide-react";

import * as api from "../api";
import type { AutomationTask, Session } from "../types";
import { PageHeader, SkeletonCard } from "./ui";
import { AliCharacter, type AliState } from "./twin/AliCharacter";

interface TwinViewProps {
  /** 宿主会话表 —— 「最近活跃」用它，避免额外一次全量拉取。 */
  sessions: Session[];
  /** 会话通道接通前的降级：把用户送去对话页。 */
  onOpenChat: () => void;
  /** 把阿黎的专属会话登记进宿主表（工作台同款机制）——不登记 ChatView 渲染不出来。 */
  upsertSessions: (list: Session[]) => void;
  /** 只切激活会话、不动当前页面：点「打开对话」时用。 */
  selectSessionOnly: (id: string) => void;
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

/** `watch` 工具建的任务名前缀（见 tools/watch.rs 的 default_name）。 */
const WATCH_NAME_PREFIX = "守望：";

/** 阿黎专属会话的标题。提交时按标题找到或创建（服务端生成 id）。 */
const ALI_SESSION_TITLE = "助手阿黎";

type TwinPhase = "idle" | "sending" | "running" | "done" | "error";

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

export function TwinView({
  sessions,
  onOpenChat,
  upsertSessions,
  selectSessionOnly,
}: TwinViewProps) {
  const [tasks, setTasks] = useState<AutomationTask[]>([]);
  const [activeRuns, setActiveRuns] = useState(0);
  /** 最近的工作日志（daily_memory）—— 三栏右侧的真实数据源之一。 */
  const [workDays, setWorkDays] = useState<api.DailyMemoryDay[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [level, setLevel] = useState<"default" | "full">("default");
  const [mode, setMode] = useState<Mode>("summary");
  const [draft, setDraft] = useState("");
  /** 上次成功核对的时刻（ms）。给在场位显示「多久没看过了」。 */
  const [checkedAt, setCheckedAt] = useState<number | null>(null);

  // ── 会话通道状态 ──
  const [twinPhase, setTwinPhase] = useState<TwinPhase>("idle");
  const [twinReply, setTwinReply] = useState<string | null>(null);
  const [twinError, setTwinError] = useState<string | null>(null);
  const [aliSession, setAliSession] = useState<Session | null>(null);
  /** 她的轮次里挂起的审批 —— 「待你定夺」的真实数据源。 */
  const [pendingApprovals, setPendingApprovals] = useState<
    api.PendingApprovalItem[]
  >([]);

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

  // 阿黎会话存在后，持续盯她的挂起审批：她跑的轮次若遇到需批准的操作，
  // 审批卡挂在她自己的会话里 —— 用户在阿黎页看不到，轮次就会干等到超时。
  // 裁决仍走会话内的审批卡（WS 通道），这里只负责「让你知道有东西要拍板」。
  useEffect(() => {
    if (!aliSession) return;
    let alive = true;
    const load = async () => {
      const items = await api.getPendingApprovals(aliSession.id);
      if (alive) setPendingApprovals(items);
    };
    void load();
    const timer = setInterval(load, 10_000);
    return () => {
      alive = false;
      clearInterval(timer);
    };
  }, [aliSession]);

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
      [...sessions]
        .sort((a, b) => (b.updated_at ?? "").localeCompare(a.updated_at ?? ""))
        .slice(0, 5),
    [sessions],
  );

  const stateText =
    activeRuns > 0
      ? `运行中 · ${activeRuns} 个任务在跑`
      : `静默观察 · ${enabledTasks.length} 个任务待触发`;

  // 角色姿态与"她在干什么"是同一件事的两个表达，必须同源。
  // 角色姿态：会话通道的状态优先（她在处理时是"思考"，出错了是警觉）。
  const aliState: AliState = twinPhase === "error"
    ? "alert"
    : twinPhase === "sending" || twinPhase === "running"
      ? "thinking"
      : twinPhase === "done"
        ? "done"
        : error
          ? "alert"
          : loading
            ? "thinking"
            : activeRuns > 0
              ? "running"
              : "idle";

  const presenceLine = twinPhase === "sending"
    ? "正在把话带给她…"
    : twinPhase === "running"
      ? "她在处理，回复写进「助手阿黎」会话"
      : twinPhase === "done" && twinReply
        ? `她：${twinReply}`
        : twinPhase === "error"
          ? (twinError ?? "出了点问题")
          : error
            ? "核对任务状态时出错，右侧「正在执行」里是原始报错"
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

  // 守望任务与普通定时任务的区分（v1 约定）：`watch` 工具建的任务名
  // 一律以「守望：」开头。等 `AutomationTask` 有 kind 字段后换成结构化
  // 判定，这里的字符串匹配随之删除。
  const watchTasks = useMemo(
    () => enabledTasks.filter((t) => t.name.startsWith(WATCH_NAME_PREFIX)),
    [enabledTasks],
  );
  const plainTasks = useMemo(
    () => enabledTasks.filter((t) => !t.name.startsWith(WATCH_NAME_PREFIX)),
    [enabledTasks],
  );

  const removeWatch = useCallback(
    async (id: string) => {
      const ok = await api.deleteAutomationTask(id);
      if (ok) {
        void refresh();
      }
    },
    [refresh],
  );

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    const text = draft.trim();
    if (!text || twinPhase === "sending" || twinPhase === "running") return;
    setDraft("");
    setTwinError(null);
    setTwinReply(null);
    setTwinPhase("sending");
    try {
      // 找到或创建阿黎的专属会话（标题匹配；服务端生成 id）。
      const all = await api.getSessions();
      let ali = all.find((s) => s.title === ALI_SESSION_TITLE);
      if (!ali) ali = await api.createSession(ALI_SESSION_TITLE);
      setAliSession(ali);
      upsertSessions([ali]);

      await api.sendTwinMessage(ali.id, text);
      setTwinPhase("running");
      void pollReply(ali.id);
    } catch (err) {
      setTwinError(err instanceof Error ? err.message : String(err));
      setTwinPhase("error");
    }
  };

  /** 轮询等她的回复落库。3s 一次、至多 3 分钟 —— 不做 WebSocket 订阅，
   *  因为这里唯一要等的就是「那一条 assistant 消息」。 */
  const pollReply = useCallback(async (sessionId: string) => {
    let baseTotal = 0;
    try {
      const base = await api.getSessionMessages(sessionId, { limit: 5 });
      baseTotal = base.totalMessages ?? base.messages.length;
    } catch {
      // 读不到基线就只看「最后一条是不是 assistant」。
    }
    for (let i = 0; i < 60; i++) {
      await sleep(3_000);
      try {
        const res = await api.getSessionMessages(sessionId, { limit: 5 });
        const total = res.totalMessages ?? res.messages.length;
        const last = res.messages[res.messages.length - 1];
        if ((total > baseTotal || baseTotal === 0) && last?.role === "assistant") {
          setTwinReply(firstLine(last.content) || "（她回了，但内容是空的）");
          setTwinPhase("done");
          return;
        }
      } catch {
        // 网络抖动：下一轮再试。
      }
    }
    setTwinError("等了很久还没有回复。去对话页看看——可能后台出问题了。");
    setTwinPhase("error");
  }, []);

  /** 打开阿黎的会话（登记 → 切激活 → 跳对话页），后续轮次直接在对话里聊。 */
  const openAliChat = () => {
    if (!aliSession) return;
    upsertSessions([aliSession]);
    selectSessionOnly(aliSession.id);
    onOpenChat();
  };

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
                <span className="twin-readout__value">
                  {pendingApprovals.length}
                </span>
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
      <div className="twin-presence">
        <AliCharacter state={aliState} size={48} />
        <div className="twin-presence__text">
          <span className="twin-presence__name">阿黎</span>
          <span className="twin-presence__line">{presenceLine}</span>
        </div>
        {aliSession ? (
          <button type="button" className="twin-btn" onClick={openAliChat}>
            打开对话
          </button>
        ) : null}
        <span className="twin-presence__checked">{checkedText}</span>
      </div>

      <div className="twin-board">
        {/* ── 一、新鲜情报 ── */}
        <section className="twin-col">
          <div className="twin-col__head">
            <span className="twin-col__title">新鲜情报</span>
            <span className="twin-col__meta">待接通</span>
          </div>
          <div className="twin-col__body">
            <Pending
              title="情报源尚未接通"
              desc="接通后这里显示她替你盯到的新变化：只收「新」且与你在意的项目相关的，重复内容进笔记不再打扰。"
              items={[
                "订阅源水位线（RSS 取 last guid、页面取内容指纹）",
                "每条带一句「为什么给你看」的依据",
              ]}
            />

            <div className="twin-col__head">
              <span className="twin-col__title">最近活跃</span>
              <span className="twin-col__meta">{recentSessions.length} 条</span>
            </div>
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
          <div className="twin-col__head">
            <span className="twin-col__title">正在执行</span>
            <span className="twin-col__meta">
              {activeRuns} 跑 · {enabledTasks.length} 启用
            </span>
          </div>
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

            {plainTasks.map((t) => {
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

            {/* 守望单独一节：它们不产出东西，只负责「该不该打扰你」的判断，
                和在跑的任务混在一起会让人以为它们也在干活。 */}
            {watchTasks.length > 0 ? (
              <>
                <div className="twin-subhead">
                  <span className="twin-col__title">正在盯</span>
                  <span className="twin-col__meta">{watchTasks.length}</span>
                </div>
                {watchTasks.map((t) => (
                  <article key={t.id} className="twin-task">
                    <div className="twin-task__row">
                      <span className="twin-task__name">
                        {t.name.replace(WATCH_NAME_PREFIX, "")}
                      </span>
                      <span className="twin-task__pct">
                        {t.last_status === "failed" ? "有异常" : "在盯"}
                      </span>
                    </div>
                    <div className="twin-task__sub">
                      <span
                        className={`twin-dot${statusDotClass(t.last_status)}`}
                        aria-hidden
                      />
                      <span>
                        {t.last_status
                          ? (STATUS_TEXT[t.last_status] ?? t.last_status)
                          : "还没查过第一轮"}
                      </span>
                      <code>{t.schedule_label ?? t.schedule}</code>
                      {t.next_run_at ? (
                        <span>下次 {formatRelative(t.next_run_at)}</span>
                      ) : null}
                    </div>
                    <div className="twin-card__foot">
                      <button
                        type="button"
                        className="twin-btn"
                        onClick={() => void removeWatch(t.id)}
                      >
                        不盯了
                      </button>
                      <span className="twin-card__note">
                        撤掉后她就不再检查这件事
                      </span>
                    </div>
                  </article>
                ))}
              </>
            ) : null}
          </div>
        </section>

        {/* ── 三、待你定夺 ── */}
        <section className="twin-col twin-col--third">
          <div className="twin-col__head">
            <span className="twin-col__title">待你定夺</span>
            <span className="twin-col__meta">
              {pendingApprovals.length > 0
                ? `${pendingApprovals.length} 件待批`
                : "无"}
            </span>
          </div>
          <div className="twin-col__body">
            {pendingApprovals.length === 0 ? (
              <Pending
                title="没有等你拍板的事"
                desc="她的轮次若遇到需要批准的操作，审批会出现在这里——你在对话页看不到时，至少这里不会漏。"
              />
            ) : (
              pendingApprovals.map((a) => (
                <article key={a.request_id} className="twin-ask">
                  <div className="twin-ask__q">{a.tool}</div>
                  <div className="twin-ask__why">
                    {summarizeArgs(a.arguments)}
                  </div>
                  <div className="twin-ask__acts">
                    <button
                      type="button"
                      className="twin-btn twin-btn--primary"
                      onClick={openAliChat}
                    >
                      去处理
                    </button>
                  </div>
                </article>
              ))
            )}

            <div className="twin-col__head">
              <span className="twin-col__title">最近工作日志</span>
              <span className="twin-col__meta">{workDays.length} 天</span>
            </div>
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

      <form className="twin-dock" onSubmit={submit}>
        <div className="twin-dock__hint">
          <span>回车发送</span>
          <kbd>Shift</kbd>
          <span>+</span>
          <kbd>Enter</kbd>
          <span>换行</span>
          <span>·</span>
          <span>回复写进「助手阿黎」会话，随时可打开继续聊</span>
        </div>
        <div className="twin-dock__row">
          <span className="twin-dock__caret" aria-hidden>
            ›
          </span>
          <input
            className="twin-dock__input"
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            placeholder="跟她说点什么，或者直接派活"
            aria-label="给助手阿黎发消息"
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
          <button type="submit" className="twin-send" aria-label="发送">
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

/** 把审批参数压成一行可读摘要（上限 120 字）。 */
function summarizeArgs(args: unknown): string {
  if (args == null) return "（无参数）";
  let text: string;
  try {
    text = typeof args === "string" ? args : JSON.stringify(args);
  } catch {
    return "（参数无法序列化）";
  }
  const line = text.replace(/\s+/g, " ").trim();
  return line.length > 120 ? `${line.slice(0, 118)}…` : line || "（空参数）";
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
