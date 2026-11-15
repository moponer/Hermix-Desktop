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
import { usePolling } from "../hooks/usePolling";
import { useCurrentModel } from "../hooks/useCurrentModel";
import { usePermissionLevel, type PermissionLevel } from "../hooks/useExpert";
import { friendlyModelName } from "./ModelSelector";
import type { AutomationTask, Session } from "../types";
import { AliCharacter, type AliState } from "./twin/AliCharacter";
import { TwinReportCards } from "./twin/TwinReportCards";
import { TwinTaskQueue } from "./twin/TwinTaskQueue";
import { TWIN_SESSION_ID, type TwinInlineApi } from "../app/routes";
import {
  mergeTranscript,
  toTranscriptRows,
  TRANSCRIPT_CAP,
  type TranscriptRow,
} from "../lib/twinTranscript";
import { clockTime, dailyLine, pad2, relativeCompact, sessionRowLabel } from "../lib/readout";
import { verdictLines } from "../lib/twinQueue";
import type { PatrolCode, PatrolFinding } from "../lib/twinPatrol";

/** 落库历史的轮询间隔。她这一轮跑着时暂停，避免轮询结果落后于屏幕造成回跳。 */
const HISTORY_POLL_MS = 8000;

/** 面板/历史请求的超时预算。后端被长任务（如整盘扫描）拖住时，这些请求会
 *  一直 pending —— 无限转圈的"加载中"比一条超时报错更伤：用户分不清是
 *  在工作还是挂了。到点如实报错，轮询路径下一轮自会重试。 */
const BOARD_TIMEOUT_MS = 10_000;

async function withTimeout<T>(p: Promise<T>, ms: number, label: string): Promise<T> {
  let timer: number | undefined;
  try {
    return await Promise.race([
      p,
      new Promise<never>((_, reject) => {
        timer = window.setTimeout(
          () => reject(new Error(`${label} 超时（${Math.round(ms / 1000)}s 无响应）`)),
          ms,
        );
      }),
    ]);
  } finally {
    if (timer !== undefined) window.clearTimeout(timer);
  }
}

/** 手动切回对话后，这段时间内不再被自动切走（防抢屏）。 */
const AUTO_SWITCH_COOLDOWN_MS = 30_000;

/**
 * 团队执行通道（派单 / 跟进 / 验收）—— 阿黎真正"建团队"的动作。
 *
 * 2026-11-06 与取数通道拆开：旧表把 `delegate_task`（同步子代理）和
 * `dispatch_member`（长任务执行单元）混在同一行，用户看到的是「发布到
 * 团队：delegate_task」—— 明明是子代理在跑，却写着"团队"，判断无从做。
 * 现在两类各自一行，名字不同、含义不同。
 */
const TEAM_TOOL_NAMES = new Set([
  "dispatch_member",
  "assign_task",
  "task_create",
  "task_update",
  "review_member",
  "review_result",
  "get_member_result",
  "stop_member",
]);

/** 取数通道（一次性读/分析，回传结论）—— 不是团队，不产交付物。 */
const FETCH_TOOL_NAMES = new Set(["delegate_task", "get_subagent_result", "stop_subagent"]);

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
   * 主对话是否有轮次在跑。「在跑」的口径必须把三方都数进来：自动化在途
   * run + 她自己的轮次（`twin.streaming`）+ 主对话的轮次 —— 少了后两者，
   * 顶栏「正在执行」与面板「0 个任务在跑」就是两个不相干的世界（实测脱节）。
   */
  mainStreaming?: boolean;
  /**
   * 她自己的发送器（App 顶层的第二个 `useStreamChat` 实例）。
   *
   * 独立实例 = 与任何对话并行、互不禁用；本轮文本、未决审批、澄清都在
   * 这里就地渲染，不占宿主会话表。
   */
  twin: TwinInlineApi;
}

/**
 * 中轴四态（2026-11-06 重构）。
 *
 * 面板把「收需求」与「看执行」拆开：`queue` 是待拉取的需求（可排序），
 * `team` 是团队任务看板（成员工单），`data` 是状态读数（本轮整合了
 * 哪些需求、发布到团队了什么）。此前只有「对话 / 数据」两态，需求与
 * 执行混在一条对话流里，用户没法表达优先级，也看不出哪条做完了。
 */
type CenterMode = "chat" | "queue" | "team" | "data";

const STATUS_TEXT: Record<string, string> = {
  ok: "上次成功",
  failed: "上次失败",
  running: "运行中",
};

/**
 * 面板标题 —— 仪表台语法（2026-10-28）。
 *
 * 上一版是「3px 主色竖条 + 一行标签」，读起来是网页仪表盘：竖条是装饰，
 * 编号缺失，标签与读数之间没有刻度关系。仪表台靠三件事立住：
 *   ① 通道编号（`01`…）—— 仪器面板都有槽位号，用户能指着说「02 那块」；
 *   ② 一条带刻度的基线 —— 面板之间的分隔由刻度承担，不用描边；
 *   ③ 右对齐读数 —— mono、等宽、与数字列对齐。
 */
function BoxHead({
  code,
  title,
  meta,
}: {
  code: string;
  title: string;
  meta?: React.ReactNode;
}) {
  return (
    <div className="twin-box__head">
      <span className="twin-box__bar" aria-hidden />
      <span className="twin-box__code" aria-hidden>
        {code}
      </span>
      <span className="twin-box__title">{title}</span>
      <span className="twin-box__spacer" />
      {meta != null ? <span className="twin-box__meta">{meta}</span> : null}
    </div>
  );
}

export function TwinView({
  sessions,
  onOpenChat,
  onOpenSession,
  onDeleteSession,
  twin,
  mainStreaming = false,
}: TwinViewProps) {
  const [tasks, setTasks] = useState<AutomationTask[]>([]);
  const [activeRuns, setActiveRuns] = useState(0);
  /** 最近的工作日志（daily_memory）—— 左翼与右翼的真实数据源之一。 */
  const [workDays, setWorkDays] = useState<api.DailyMemoryDay[]>([]);
  /** 落库历史（轮询拿到）。手机投递进来的轮次、App 重启前的旧轮次都在这。 */
  const [history, setHistory] = useState<TranscriptRow[]>([]);
  /** 面板队列表头的一次性提示（例如未入队的重复需求）。 */
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
      const res = await withTimeout(
        api.getSessionMessages(TWIN_SESSION_ID, { limit: 12 }),
        BOARD_TIMEOUT_MS,
        "值守历史",
      );
      if (!mountedRef.current) return;
      setHistory(toTranscriptRows(res.messages).slice(-TRANSCRIPT_CAP));
    } catch {
      /* 保留上一份内容：抖动不该清空面板 */
    }
  }, []);

  // 8s 历史轮询（P2-4）：隐藏页不发请求，恢复可见立即补一次。
  usePolling(loadHistory, HISTORY_POLL_MS);

  // 这一轮刚结束：立刻对齐落库历史，别让用户等下一个 8s 轮询
  // （那段时间里屏幕上还是内存里的两行，历史一到会闪一下）。
  // 面板补刷在 loadBoard 声明之后的另一个 effect 里（日志摘要那时才落库）。
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
  /**
   * 权限模式 —— 与主对话共用**一个**全局设置（`usePermissionLevel` 落
   * storage）。此前这一页是本地 `useState`：在这里选了放手，主对话还在问，
   * 换个页回来又变回默认 —— 同一个策略两个家，用户没法判断当前到底哪种。
   */
  const [level, setLevel] = usePermissionLevel();
  /** 切到放手模式的二次确认（放手是不可逆后果的一侧，必须两下）。 */
  const [permArmed, setPermArmed] = useState(false);
  /**
   * 她没有独立模型设置：`twin.send` 不下发 model，后端按
   * `overrides.model → config 默认模型` 解析，所以她跑的就是全局默认模型。
   * 这里把它显式显示出来 —— 此前这一页完全不提模型，用户无法判断她在用什么。
   */
  const { defaultModel } = useCurrentModel();
  const modelName = defaultModel
    ? friendlyModelName(defaultModel.slice(defaultModel.indexOf(":") + 1))
    : "未配置";
  const [draft, setDraft] = useState("");
  /** 上次成功核对的时刻（ms）。给在场位显示「多久没看过了」。 */
  const [checkedAt, setCheckedAt] = useState<number | null>(null);
  /** 中轴两态。 */
  const [center, setCenter] = useState<CenterMode>("chat");
  /** 用户手动切回对话的时刻 —— 冷却期内不被自动切换抢屏。 */
  const manualAtRef = useRef(0);

  /**
   * 面板数据（任务 / 在跑数 / 工作日志）。分两档：
   * `withLoading` 只在进页首刷用（显示核对中）；轮询与轮次结束后的补刷
   * 静默进行 —— 这块数据与对话流无关，streaming 期间照刷（历史轮询才跳过）。
   */
  const loadBoard = useCallback(async (withLoading: boolean) => {
    if (withLoading) {
      setLoading(true);
      setError(null);
    }
    try {
      const [list, active, days] = await withTimeout(
        Promise.all([
          api.getAutomationTasks(),
          api.getActiveAutomationRuns(),
          api.getDailyMemoryList(14).catch(() => [] as api.DailyMemoryDay[]),
        ]),
        BOARD_TIMEOUT_MS,
        "值守面板",
      );
      setTasks(list);
      setActiveRuns(active);
      // 值守口径：daily_memory 摘要行的会话 id 可能带 `[twin:…]` 方括号
      //（回合结束时确定性落库的行是 `- HH:MM [sid] 摘要`，`[` 在行中），
      // 也可能是 LLM 自己用 daily_memory 工具写的自由文本。锚行首会把
      // 前者全部漏掉 —— 面板日志恒 0 的直接原因（2026-10-27 修复）。
      // 只有阿黎工作域的会话（twin: / automati* / member::）才进面板，
      // 普通主对话的用户消息流水不混进来。
      const dutyScope = (d: api.DailyMemoryDay) =>
        /\[(twin:|automati|member::)/.test(
          `${d.goals}
${d.progress}
${d.conclusions}`,
        );
      setWorkDays(days.filter(dutyScope));
      setCheckedAt(Date.now());
    } catch (e) {
      if (withLoading) setError(String(e));
    } finally {
      if (withLoading) setLoading(false);
    }
  }, []);

  const refresh = useCallback(() => loadBoard(true), [loadBoard]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  // 面板轮询：她这一轮跑着、自动化到点触发、别的会话在跑，数字都要动。
  // 轮询间隔与落库历史一致（8s），一次 tick 三项一起对齐。
  usePolling(() => loadBoard(false), HISTORY_POLL_MS);

  // 这一轮刚结束：面板补刷一次 —— 工作日志摘要在会话结束那一刻落库，
  // 不补刷「刚才发生 / 日志天」要干等下一个轮询周期才动。
  const boardEndRef = useRef(twin.streaming);
  useEffect(() => {
    if (boardEndRef.current && !twin.streaming) void loadBoard(false);
    boardEndRef.current = twin.streaming;
  }, [twin.streaming, loadBoard]);

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

  /**
   * 「刚才发生」的行 = 这一轮的工具流水，**唯一来源**。
   *
   * 此前这里还拼了工作日志的 8 天摘要，三个后果：① 同一批日志在左翼
   * 「最近工作日志」和右翼各印一遍，而两处取的字段顺序还不一样（左翼
   * conclusions 优先、右翼 progress 优先）→ 同一天能显示两段不同的话；
   * ② 面板头的「3 条」数的是日志条数，屏幕上是 9 行；③ 日志是历史，
   * 与「刚才发生」这个标题本来就不是一件事。日志归左翼，这里只放动作。
   */
  const happenedRows = useMemo(() => {
    if (!twin.turn) return [];
    const rows: { t: string; text: string; tag: string }[] = [];
    if (twin.streaming) {
      rows.push({
        t: "此刻",
        text: twin.turn.activity
          ? `正在用 ${twin.turn.activity}`
          : "正在跑你交代的这一轮",
        tag: "这一轮",
      });
    }
    for (const e of (twin.turn.events ?? []).slice(-6).reverse()) {
      rows.push({
        // 跑完之后这些是既成事实：写「刚才 / 上一轮」。此前一律写
        // 「刚刚 / 这一轮」，静默时看起来像还在跑（用户实报截图）。
        t: twin.streaming ? "刚刚" : "刚才",
        text: e,
        tag: twin.streaming ? "这一轮" : "上一轮",
      });
    }
    return rows;
  }, [twin.turn, twin.streaming]);

  /**
   * 「发布到团队」= 本轮流水里与派单/工单相关的动作。
   *
   * 这是"需求真的派出去了"唯一可核对的信号：她有没有调 dispatch_member /
   * assign_task、工单有没有更新，全在流水里。不做二次加工 —— 面板读的
   * 就是这一步真实发生的工具名 + 参数摘要。
   */
  const teamPublish = useMemo(
    () =>
      (twin.turn?.events ?? []).filter((e) => TEAM_TOOL_NAMES.has(e.split(":")[0].trim())),
    [twin.turn],
  );

  /**
   * 「取数」= 本轮调了同步子代理。
   *
   * 单独一行的理由：`delegate_task` 会把这一轮卡住直到返回，且**不产出
   * 交付物**。她该用它读大文件/多文件检索，不该用它跑任务（那是
   * dispatch_member 的活）。分成两行，用户一眼能看出"这轮是在取数还是在
   * 派人"，也看得出她把哪一类搞混了。
   */
  const fetchPublish = useMemo(
    () =>
      (twin.turn?.events ?? []).filter((e) => FETCH_TOOL_NAMES.has(e.split(":")[0].trim())),
    [twin.turn],
  );

  /**
   * 她对这批需求的归档判断 —— **原文**，来自她回复里的 `【归档】…` 行。
   *
   * 前端不再按关键词猜"这条属于哪类"：两条需求是不是同一件事，只有她知道
   * 自己手上在做什么。没写就如实显示"没写"，不替她补一个分类。
   */
  const archive = useMemo(() => verdictLines(twin.turn?.reply ?? ""), [twin.turn]);

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

  /**
   * 「在跑」的统一口径（2026-10-27）：自动化在途 run + 她的轮次 +
   * 主对话的轮次。此前只有第一项，顶栏明明写着「正在执行」、面板却是
   * 0 —— 两个数字不同源，用户看到的就是「那边在跑，这边不动」。
   */
  const turnBusy = (twin.streaming ? 1 : 0) + (mainStreaming ? 1 : 0);
  const busy = activeRuns + turnBusy;

  /**
   * 「她现在什么状态」的唯一出处（顶栏与中轴都用它）。
   *
   * 待办排在最前（2026-10-28 修正）：此前只看 `busy`，于是同一屏上
   * 顶栏写「静默观察 · 0 个任务待触发」、右翼写「等你决定 1 件 · 需要授权
   * terminal」—— 她明明停在原地等人拍板，页面却说没事可做。待办是**她
   * 停下来的原因**，比"在不在跑"更该先说。
   */
  const stateText =
    pending.length > 0
      ? `等你决定 · ${pending.length} 件待办`
      : twin.streaming
        ? `正在执行 · 本轮 ${twin.batch?.tasks.length ?? 0} 条需求`
        : twin.queue.length > 0
          ? `待拉取 · ${twin.queue.length} 条需求排队`
          : busy > 0
            ? `运行中 · ${busy} 个任务在跑`
            : `静默观察 · ${enabledTasks.length} 个任务待触发`;

  // 角色姿态与"她在干什么"是同一件事的两个表达，必须同源。
  const aliState: AliState = error
    ? "alert"
    : busy > 0
      ? "running"
      : loading
        ? "thinking"
        : "idle";

  const presenceLine = error
    ? "核对任务状态时出错，右翼「刚才发生」上方是原始报错"
    : pending.length > 0
      ? `有 ${pending.length} 件等你拍板，她停在这儿等`
      : twin.streaming
      ? twin.turn?.activity
        ? `正在用 ${twin.turn.activity} 做事，回复会直接出现在中间`
        : "正在执行面板拉下来的这一批需求"
      : twin.queue.length > 0
        ? `${twin.queue.length} 条需求在面板里排队，她下一轮就拉`
        : loading
          ? "正在核对任务状态"
          : busy > 0
            ? `盯着 ${busy} 个在跑的任务，异常才出声`
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

  /**
   * 权限模式切换。
   *
   * 放手模式是不可逆后果的一侧（她不再问就直接跑命令、删文件），所以
   * 「进入」要两下：第一下把开关变成一句风险说明 + 确认钮，第二下才生效；
   * 「退出」回到安全侧，一下即可。确认态 6s 没动作自动收回 —— 留在
   * "待确认"会让下一次无意点击变成意外提交。
   */
  useEffect(() => {
    if (!permArmed) return;
    const t = window.setTimeout(() => setPermArmed(false), 6000);
    return () => window.clearTimeout(t);
  }, [permArmed]);

  /**
   * 落档 = 本地偏好 + **立刻通知后端**。
   *
   * 只写 localStorage 时，后端要到**下一条消息**才从 `permission` 字段得知
   * 用户已经放手（档位按轮捕获）。后果 2026-11-06 实报：在值守页点完
   * 「确认放手」，正在跑的这一轮仍然一张张弹审批卡，用户得出"放手模式无效"
   * 的结论。`POST /v1/sessions/{id}/permission` 是热更加放行已挂起请求的
   * 入口（主对话 2026-10-31 起就走它），这里补齐同一条。
   */
  const applyLevel = useCallback(
    (next: PermissionLevel) => {
      setLevel(next);
      void api.setSessionPermission(TWIN_SESSION_ID, next);
    },
    [setLevel],
  );

  const pickPermission = useCallback(
    (next: PermissionLevel) => {
      if (next === level) return;
      if (next === "full") {
        setPermArmed(true);
        return;
      }
      setPermArmed(false);
      applyLevel("default");
    },
    [level, applyLevel],
  );

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

  /**
   * 下达一条需求 —— **只入队，不直接开跑**，而且永远不因为"她在跑"而拒绝。
   *
   * 两处旧缺陷一起在这里收口：
   * ① 输出未结束时输入框被禁用，人只能干等（2026-11-06 实报）；
   * ② 发不出去时只写一句「原因见下方错误」，而下方根本没有错误文本
   *    （截图实报）—— 现在入队是纯本地操作，不产生"发送失败"这种状态；
   *    真正下发失败时，原因挂在「任务面板」里那一批上，写的是原始错误。
   */
  const submit = (e: React.FormEvent) => {
    e.preventDefault();
    const text = draft.trim();
    if (!text) return;
    // 重复需求不会入队（面板里已有一条原文一样的）——如实说一句，别假装收下了。
    if (twin.queue.some((t) => t.text === text)) {
      setNotice("这条已经在面板里了");
      return;
    }
    twin.enqueueTask(text);
    setDraft("");
    setNotice(null);
  };

  const sendDisabled = draft.trim().length === 0;

  /** 「补充」：把待补充的需求取回草稿（并从面板移除），改完重新下达。 */
  const reviseFromPanel = useCallback(
    (id: string) => {
      const text = twin.reviseTask(id);
      if (text == null) return;
      setDraft(text);
      setCenter("chat");
    },
    [twin],
  );

  /**
   * 巡查建议的"下一步"。每条建议都对应一个真实操作 —— 只说"可能卡住了"
   * 而不给按钮，等于把问题丢回给用户自己找入口。
   */
  const patrolAction = useCallback(
    (code: PatrolCode): { label: string; run: () => void } | null => {
      switch (code) {
        case "stalled":
          return { label: "停止这一轮", run: twin.stop };
        case "blocked":
        case "batch_failed":
        case "queue_waiting":
          return { label: "去任务面板", run: () => setCenter("queue") };
        case "tickets_failed":
          return { label: "看团队看板", run: () => setCenter("team") };
        default:
          return null;
      }
    },
    [twin],
  );

  const findings: PatrolFinding[] = twin.patrol?.findings ?? [];

  return (
    <div className="twin-screen" data-mode={aliState}>
      {/* ── 顶栏 ───────────────────────────────────────────── */}
      <header className="twin-head">
        <div className="twin-head__main">
          <h1 className="twin-head__title">阿黎</h1>
          {/* 状态灯的三态与 stateText 同源：有待办 = 她在等你（警告色），
              在跑 = 值守中（强调色），都不成立才是静默。此前只有两态，
              于是「静默」与右翼「等你决定」同时成立。 */}
          <span
            className={`twin-head__state${pending.length > 0 ? " is-hot" : busy > 0 ? " is-live" : ""}`}
            title={stateText}
          >
            <i aria-hidden />
            {pending.length > 0
              ? "待你决定"
              : busy > 0
                ? "值守中"
                : twin.queue.length > 0
                  ? "待拉取"
                  : "静默"}
          </span>
          <span className="twin-head__sub">{stateText}</span>
        </div>
        <div className="twin-head__side twin-head__side--r">
          {/* 她跟随全局默认模型（无独立设置）：把模型摆出来，用户才知道
              这一路对话是谁在答。 */}
          <span className="twin-head__kv" title="跟随全局默认模型（设置 → 模型）">
            模型 <b className="twin-head__model">{modelName}</b>
          </span>
          {/* 读数用 `key={值}` 触发重挂：元素重建一次 → CSS 的 twin-tick
              动画重跑一次。8s 轮询里值没变就不重建，所以"跳动"只在数字真的
              变时发生，不会成为周期性抖动。数字不可聚焦，重建不丢焦点。 */}
          <span className="twin-head__kv">
            在跑 <b key={busy}>{pad2(busy)}</b>
          </span>
          <span className="twin-head__kv" title="任务面板里还没被拉走的需求">
            待拉取 <b key={twin.queue.length}>{pad2(twin.queue.length)}</b>
          </span>
          <span className="twin-head__kv">
            已启用 <b key={enabledTasks.length}>{pad2(enabledTasks.length)}</b>
          </span>
          <span className="twin-head__kv">
            待批 <b key={pending.length} className={pending.length > 0 ? "is-hot" : ""}>
              {pad2(pending.length)}
            </b>
          </span>
          {/* 权限模式：两个选项都摆在面上（分段控件），当前那段实底上色。
              旧版是一个会换字的按钮、默认态无色 —— 用户看不出「现在哪种」
              「另一种是什么」。默认=成功档（有护栏），放手=警告档（无护栏）。 */}
          <div className="twin-perm" data-level={level}>
            {permArmed ? (
              <>
                <span className="twin-perm__ask">
                  <AlertTriangle className="icon-sm" aria-hidden />
                  放手后不再申请权限
                </span>
                <button
                  type="button"
                  className="twin-perm__opt twin-perm__opt--danger"
                  onClick={() => {
                    applyLevel("full");
                    setPermArmed(false);
                  }}
                >
                  确认放手
                </button>
                <button
                  type="button"
                  className="twin-perm__opt"
                  onClick={() => setPermArmed(false)}
                >
                  取消
                </button>
              </>
            ) : (
              <>
                <button
                  type="button"
                  className={`twin-perm__opt${level === "default" ? " is-on" : ""}`}
                  aria-pressed={level === "default"}
                  title="默认模式：命令、删除、写配置等高危操作先弹卡问你"
                  onClick={() => pickPermission("default")}
                >
                  默认 · 需授权
                </button>
                <button
                  type="button"
                  className={`twin-perm__opt twin-perm__opt--risk${level === "full" ? " is-on" : ""}`}
                  aria-pressed={level === "full"}
                  title="放手模式：她不再申请权限，命令与删除直接执行，风险由你承担"
                  onClick={() => pickPermission("full")}
                >
                  放手 · 无审批
                </button>
              </>
            )}
          </div>
        </div>
      </header>

      {/* 放手模式常驻风险条：模式一旦生效，"不问就直接跑"这件事必须在视线里，
          不能只藏在按钮的一个词里。切回默认即消失。 */}
      {level === "full" ? (
        <div className="twin-risk" role="status">
          <AlertTriangle className="icon-sm" aria-hidden />
          <b>放手模式</b>
          <span>她不再申请权限：命令、删除、写配置都会直接执行，出错由你承担。</span>
          <button type="button" className="chip-flat" onClick={() => applyLevel("default")}>
            切回默认
          </button>
        </div>
      ) : null}

      <div className="twin-main">
        {/* ── 左翼：她替你看着什么 ─────────────────────────── */}
        <aside className="twin-wing">
          <section className="twin-box">
            {/* 读数写「启用 N / 共 M」而不是光秃秃的「N 个」：同一屏的 02 里有
                「任务总数 07」，只写「0 个」会被读成"一条任务都没有"（实际是
                7 条全停用）。 */}
            <BoxHead
              code="01"
              title="值守任务"
              meta={`启用 ${enabledTasks.length} / ${tasks.length}`}
            />
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
            <BoxHead code="02" title="此刻" meta={checkedText} />
            <div className="twin-grid">
              <div>
                <div
                  key={busy}
                  className={`twin-grid__v${busy > 0 ? " is-live" : " is-zero"}`}
                >
                  {pad2(busy)}
                </div>
                <div className="twin-grid__k">在跑</div>
              </div>
              <div>
                <div
                  key={enabledTasks.length}
                  className={`twin-grid__v${enabledTasks.length > 0 ? "" : " is-zero"}`}
                >
                  {pad2(enabledTasks.length)}
                </div>
                <div className="twin-grid__k">已启用</div>
              </div>
              <div>
                <div
                  key={pending.length}
                  className={`twin-grid__v${pending.length > 0 ? " is-hot" : " is-zero"}`}
                >
                  {pad2(pending.length)}
                </div>
                <div className="twin-grid__k">待批</div>
              </div>
              <div>
                <div
                  key={tasks.length}
                  className={`twin-grid__v${tasks.length > 0 ? "" : " is-zero"}`}
                >
                  {pad2(tasks.length)}
                </div>
                <div className="twin-grid__k">任务总数</div>
              </div>
            </div>
          </section>

          <section className="twin-box twin-box--grow">
            <BoxHead code="03" title="最近会话" meta={`${recentSessions.length} 条`} />
            <div className="twin-rows">
              {recentSessions.length === 0 ? (
                <p className="twin-muted">还没有会话记录。</p>
              ) : (
                recentSessions.map((s) => {
                  // 阿黎工作域的会话**永远**拿不到标题（自动起名只走主对话），
                  // 库里是 NULL → 接口回落成 `New chat`。两行一样的「New chat」
                  // 等于没写（2026-11-06 实报）：按 id 归类成
                  // 「阿黎的对话 / 自动化 · 任务名 / 成员工单」。
                  const label = sessionRowLabel(s.id, s.title, (autoId) =>
                    tasks.find((t) => t.id === autoId)?.name,
                  );
                  return (
                    <div key={s.id} className="twin-row twin-row--act">
                      {/* 一行一个会话：标题是正文（主墨 13px），时间是元信息（弱墨，
                          右对齐）。条数不进行内 —— 它和面板头的「N 条」同单位却
                          不同含义（会话数 / 消息数），摆在一行里必被读错；
                          需要时挂在 title 上。 */}
                      <button
                        type="button"
                        className="twin-row__open"
                        title={`${formatRelative(s.updated_at)}${
                          typeof s.message_count === "number"
                            ? ` · 共 ${s.message_count} 条消息`
                            : ""
                        }`}
                        onClick={() => onOpenSession(s.id)}
                      >
                        <span className="twin-row__m">{label}</span>
                        <span className="twin-row__t">{relativeCompact(s.updated_at)}</span>
                      </button>
                      <button
                        type="button"
                        className="twin-row__del"
                        aria-label={`删除会话 ${label}`}
                        title="删除这个会话"
                        onClick={() => onDeleteSession(s.id)}
                      >
                        <X />
                      </button>
                    </div>
                  );
                })
              )}
            </div>
          </section>

          <section className="twin-box">
            <BoxHead code="04" title="最近工作日志" meta={`${workDays.length} 条`} />
            <div className="twin-rows">
              {workDays.length === 0 ? (
                <p className="twin-muted">还没有值守日志。</p>
              ) : (
                workDays.slice(0, 3).map((d) => (
                  <div key={d.date} className="twin-row">
                    <span className="twin-row__t">{d.date.slice(5)}</span>
                    {/* dailyLine 剥掉落库格式的前缀（`- 14:32 [automati] USER:`）
                        —— 日期已经在行首列，时间与截断的会话 id 都是噪音。 */}
                    <span className="twin-row__m">
                      {dailyLine(d.conclusions) ||
                        dailyLine(d.progress) ||
                        dailyLine(d.goals) ||
                        "这天没有留下结论"}
                    </span>
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
              aria-pressed={center === "queue"}
              onClick={() => setCenter("queue")}
            >
              任务面板
            </button>
            <button
              type="button"
              className="twin-ctab"
              aria-pressed={center === "team"}
              onClick={() => setCenter("team")}
            >
              团队看板
            </button>
            <button
              type="button"
              className="twin-ctab"
              aria-pressed={center === "data"}
              onClick={() => setCenter("data")}
            >
              状态
            </button>
            <span className="twin-ctabs__spacer" />
            {pending.length > 0 ? (
              center === "data" ? (
                <span className="twin-ctabs__note">
                  <i aria-hidden />
                  有 {pending.length} 件待办，已切到状态
                </span>
              ) : (
                <span className="twin-ctabs__note">
                  <i aria-hidden />
                  有 {pending.length} 件待办，点「状态」看详情
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
                {/* 空态：一句灰字是整页最后一个"网页"角落（2026-10-28）。
                    换成观测台的瞄准环：圆环 + 十字 + 呼吸的中心点，
                    下面两行读数（通道/状态 + 一句怎么做）。 */}
                {transcript.length === 0 && !twin.streaming ? (
                  <div className="twin-idle">
                    <span className="twin-idle__scope" aria-hidden />
                    <p className="twin-idle__line">CH ALI-01 · 待下达</p>
                    <p className="twin-idle__hint">
                      还没有接到需求 —— 在下面下达一条，她从任务面板拉走后在这台机器上执行
                    </p>
                  </div>
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

                {/* 部署与汇报（2026-11-06）：她交付的是工单结果，不是文件。
                    本批开工之后的工单直接摆进对话流 —— 派了谁、做到第几轮、
                    验收判定、交付说明正文都能就地读到。数据取自
                    `GET /api/members`，不解析她的回复去猜状态。 */}
                <TwinReportCards
                  since={twin.batch?.startedAt ?? null}
                  onRequestReport={(m) =>
                    twin.enqueueTask(
                      `【跟进】工单 ${m.member_id}（${m.role}）已结束：请验收并把结论汇报给我，` +
                        `说明达成了什么、证据是什么、还差什么。`,
                    )
                  }
                />

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

              {/* 错误只印真错误：`turn.error` 由发送器写入（含原始原因）。
                  notice 是本地提示（重复需求），单独一行弱化显示 —— 此前
                  两类混在一句「原因见下方错误」里，而下方什么都没有。 */}
              {twin.turn?.error ? (
                <p className="twin-error twin-error--inline" role="alert">
                  <AlertTriangle className="icon-md" />
                  <span>{twin.turn.error}</span>
                </p>
              ) : null}
              {notice ? <p className="twin-queue__hint twin-queue__notice">{notice}</p> : null}

              <form className="twin-cinput" onSubmit={submit}>
                <div className="twin-cfield">
                  <input
                    className="twin-cfield__input"
                    value={draft}
                    onChange={(e) => setDraft(e.target.value)}
                    onFocus={() => {
                      manualAtRef.current = Date.now();
                    }}
                    placeholder={
                      twin.queue.length > 0
                        ? `下达需求（面板里还有 ${twin.queue.length} 条待拉取）`
                        : "下达一条需求，她拉取后在这里执行"
                    }
                    aria-label="下达一条需求给助手阿黎"
                    // 不再随 streaming 禁用：她在跑的时候人照样可以排需求
                    // （2026-11-06 实报「输出没结束就输入不了」）。
                  />
                  <button
                    type="submit"
                    className="twin-cfield__send"
                    aria-label="下达需求"
                    disabled={sendDisabled}
                  >
                    下达
                  </button>
                  {/* 停止与下达并存：两个动作互不替代（旧版把发送键换成停止，
                      于是跑着的时候既发不了也不能"先排后发"）。 */}
                  {twin.streaming ? (
                    <button
                      type="button"
                      className="twin-cfield__send twin-cfield__send--stop"
                      aria-label="停止这一轮"
                      onClick={twin.stop}
                    >
                      停止
                    </button>
                  ) : null}
                </div>
              </form>
            </div>
          ) : center === "queue" ? (
            <TwinTaskQueue
              queue={twin.queue}
              batch={twin.batch}
              streaming={twin.streaming}
              onMove={twin.moveTask}
              onRemove={twin.removeTask}
              onRevise={reviseFromPanel}
              onRequeue={twin.requeueBatch}
            />
          ) : center === "team" ? (
            // 团队看板按「本批开始时刻」筛工单：这一批需求派出去的活才在
            // 视线里，历史工单不混进来（否则「该需求的完成情况」没法读）。
            <MissionBoardView embedded since={twin.batch?.startedAt ?? null} />
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
                  {/* 进度弧只在真有进度时画。零长度 dash 配 `strokeLinecap="round"`
                      会在起点留下一个圆帽 —— 读数写着 00，环上 12 点方向却亮着
                      一点，看起来像"已经在跑"（2026-10-28 截图核对）。 */}
                  {runningRatio != null && runningRatio > 0 ? (
                    <circle
                      cx="160"
                      cy="160"
                      r={ringR}
                      fill="none"
                      stroke="var(--color-accent)"
                      strokeWidth="14"
                      strokeLinecap="round"
                      strokeDasharray={`${ringC * runningRatio} ${ringC}`}
                      transform="rotate(-90 160 160)"
                    />
                  ) : null}
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
                {/* 雷达慢扫：环上唯一常驻的动效，静默时也在转（14s 一圈），
                    在跑时提速到 8s —— 转速本身就是状态，不需要额外指示灯。 */}
                <span className="twin-ring__sweep" aria-hidden />
                <div className="twin-ring__ctr">
                  <div
                    key={runningRatio != null ? Math.round(runningRatio * 100) : busy}
                    className="twin-ring__big"
                  >
                    {runningRatio != null ? Math.round(runningRatio * 100) : pad2(busy)}
                    {runningRatio != null ? <span>%</span> : null}
                  </div>
                  <div className="twin-ring__sub">
                    {runningRatio != null ? "本轮进度" : "个任务在跑"}
                  </div>
                  {/* 「正在用 X」必须以 streaming 为前提：`twin.turn.activity`
                      是上一轮留下的内存态，跑完了也不会归零（流在工具中途
                      断掉时更是永远停在最后一个工具名上）。先判 streaming
                      才不会出现「静默 / 0 个在跑」旁边写着「她正在用 task_create」。 */}
                  <div className="twin-ring__fig">
                    {runningTask
                      ? `${runningTask.name} · 第 ${runningTask.run_count} 次`
                      : twin.streaming
                        ? twin.turn?.activity
                          ? `她正在用 ${twin.turn.activity}`
                          : "这一批需求在跑"
                        : pending.length > 0
                          ? "她停下等你的决定"
                          : mainStreaming
                            ? "另一条对话在跑"
                            : "没有正在跑的任务"}
                  </div>
                </div>
              </div>

              {/* 一句话只归一个地方：状态读数（stateText）的家长是顶栏，
                  这里只说"她此刻在做什么"。此前两处都印 stateText，同一屏
                  上下两个一模一样的「静默观察 · N 个任务待触发」。 */}
              <div className="twin-data__cap">
                <b>{presenceLine}</b>
              </div>

              <div className="twin-strip">
                <div>
                  <div key={busy} className="twin-strip__v">
                    {pad2(busy)}
                  </div>
                  <div className="twin-strip__k">在跑</div>
                </div>
                <div>
                  <div key={enabledTasks.length} className="twin-strip__v">
                    {pad2(enabledTasks.length)}
                  </div>
                  <div className="twin-strip__k">已启用</div>
                </div>
                <div>
                  <div
                    key={pending.length}
                    className={`twin-strip__v${pending.length > 0 ? " is-hot" : ""}`}
                  >
                    {pad2(pending.length)}
                  </div>
                  <div className="twin-strip__k">待批</div>
                </div>
                <div>
                  <div key={workDays.length} className="twin-strip__v">
                    {pad2(workDays.length)}
                  </div>
                  <div className="twin-strip__k">日志天</div>
                </div>
                <div>
                  <div
                    key={twin.queue.length}
                    className={`twin-strip__v${twin.queue.length > 0 ? " is-hot" : ""}`}
                  >
                    {pad2(twin.queue.length)}
                  </div>
                  <div className="twin-strip__k">待拉取</div>
                </div>
              </div>

              {/* 需求整合 + 发布到团队（2026-11-06）：状态界面要能回答
                  「这一轮做的是哪几条需求」「派出去了什么」——两件事都以
                  真实数据为源（批次 = 面板拉走的那几条；派单 = 工具流水）。 */}
              <div className="twin-batchpanel">
                <div className="twin-batchpanel__head">
                  <span>本轮需求</span>
                  <span className="twin-batchpanel__meta">
                    {twin.batch
                      ? `${twin.batch.tasks.length} 条 · ${
                          twin.batch.status === "running"
                            ? "执行中"
                            : twin.batch.status === "done"
                              ? "已完成"
                              : "已失败"
                        }`
                      : "还没拉取过"}
                  </span>
                </div>
                {twin.batch ? (
                  <div className="twin-rows">
                    {twin.batch.tasks.map((t, i) => (
                      <div key={t.id} className="twin-row">
                        <span className="twin-row__t">{pad2(i + 1)}</span>
                        <span className="twin-row__m" title={t.text}>
                          {t.text}
                        </span>
                      </div>
                    ))}
                  </div>
                ) : (
                  <p className="twin-muted">面板里有需求时她会自动拉取一批。</p>
                )}
              </div>

              {/* 归档判断：她逐条判断「并入哪条任务线 / 新开哪条」，原文照登。
                  这条是她自己写的，不是前端分类 —— 界面不替她做语义判断。 */}
              <div className="twin-batchpanel">
                <div className="twin-batchpanel__head">
                  <span>她的归档判断</span>
                  <span className="twin-batchpanel__meta">
                    {archive.length > 0 ? `${archive.length} 条` : "没写"}
                  </span>
                </div>
                {archive.length > 0 ? (
                  <div className="twin-rows">
                    {archive.map((a, i) => (
                      <div key={`ar-${i}`} className="twin-row">
                        <span className="twin-row__m" title={a}>
                          {a}
                        </span>
                      </div>
                    ))}
                  </div>
                ) : (
                  <p className="twin-muted">
                    这一轮她没有写归档行（`【归档】… → 并入/新开 …`）——并入了哪条任务线，以她正文为准。
                  </p>
                )}
              </div>

              <div className="twin-batchpanel">
                <div className="twin-batchpanel__head">
                  <span>发布到团队</span>
                  <span className="twin-batchpanel__meta">
                    {twin.streaming
                      ? `本轮 ${teamPublish.length} 次`
                      : teamPublish.length > 0
                        ? `上一轮 ${teamPublish.length} 次`
                        : "没有派单"}
                  </span>
                </div>
                {teamPublish.length > 0 ? (
                  <div className="twin-rows">
                    {teamPublish
                      .slice(-5)
                      .reverse()
                      .map((e, i) => (
                        <div key={`tp-${i}`} className="twin-row">
                          <span className="twin-row__m" title={e}>
                            {e}
                          </span>
                          <span className="twin-row__k">{twin.streaming ? "本轮" : "上轮"}</span>
                        </div>
                      ))}
                  </div>
                ) : (
                  <p className="twin-muted">
                    这一轮她没有派人。需要人手时她会建团队工单，派了什么会出现在这里。
                  </p>
                )}
              </div>

              {/* 取数通道单独一行：`delegate_task` 是同步子代理，只回结论、
                  不产出交付物。它与"建团队"混在一行时，用户无法判断这一轮
                  到底是在取数还是在派人（2026-11-06 实报）。 */}
              {fetchPublish.length > 0 ? (
                <div className="twin-batchpanel">
                  <div className="twin-batchpanel__head">
                    <span>取数</span>
                    <span className="twin-batchpanel__meta">{fetchPublish.length} 次</span>
                  </div>
                  <div className="twin-rows">
                    {fetchPublish
                      .slice(-3)
                      .reverse()
                      .map((e, i) => (
                        <div key={`fp-${i}`} className="twin-row">
                          <span className="twin-row__m" title={e}>
                            {e}
                          </span>
                          <span className="twin-row__k">{twin.streaming ? "本轮" : "上轮"}</span>
                        </div>
                      ))}
                  </div>
                  <p className="twin-muted">
                    子代理只回结论、不产出交付物；会卡住这一轮直到返回。跑任务该走团队。
                  </p>
                </div>
              ) : null}

              {/* 心跳巡查（2 分钟一次，零 LLM）：显示的是**上一次真的算出来的
                  结果**，不是"服务正常"这类套话。异常时右翼「等你决定」里
                  还会多一条带按钮的建议。 */}
              <div className="twin-batchpanel">
                <div className="twin-batchpanel__head">
                  <span>巡查</span>
                  <span className="twin-batchpanel__meta">
                    {twin.patrol ? `上次 ${clockTime(twin.patrol.at)}` : "正在排第一次"}
                  </span>
                </div>
                {twin.patrol ? (
                  <>
                    <p className="twin-muted">
                      每 2 分钟一次，只看机械信号：队列 {twin.patrol.queue} · 待补充{" "}
                      {twin.patrol.blocked} · 工单{" "}
                      {twin.patrol.tickets
                        ? `${twin.patrol.tickets.running} 在跑 / ${twin.patrol.tickets.done} 完成 / ${twin.patrol.tickets.failed} 失败`
                        : "查不到"}
                    </p>
                    {twin.patrol.findings.length > 0 ? (
                      <div className="twin-rows">
                        {twin.patrol.findings.map((f, i) => (
                          <div key={`${f.code}-${i}`} className="twin-row">
                            <span
                              className={`twin-row__k${f.level === "alert" ? " is-hot" : ""}`}
                            >
                              {f.level === "alert" ? "异常" : "提醒"}
                            </span>
                            <span className="twin-row__m" title={f.text}>
                              {f.text}
                            </span>
                          </div>
                        ))}
                      </div>
                    ) : (
                      <p className="twin-muted">没有异常。</p>
                    )}
                  </>
                ) : (
                  <p className="twin-muted">启动后 5 秒跑第一次，之后每 2 分钟一次。</p>
                )}
                {/* 唯一会让她"没人叫也动手"的开关。默认关，说明写在按钮上，
                    不藏在设置页深处 —— 打开它等于把处置权交给她。 */}
                <button
                  type="button"
                  className={`chip-flat${twin.autoIntervene ? " chip-flat--on" : ""}`}
                  aria-pressed={twin.autoIntervene}
                  title="只在她空闲、没有待拉取需求、且距上次 ≥10 分钟时触发一次"
                  onClick={() => twin.setAutoIntervene(!twin.autoIntervene)}
                >
                  {twin.autoIntervene
                    ? "异常时她自己处理：已开"
                    : "异常时让她自己处理（默认关）"}
                </button>
              </div>
            </div>
          )}
        </section>

        {/* ── 右翼：现在要你做什么 ─────────────────────────── */}
        <aside className="twin-wing twin-wing--r">
          <section
            className={`twin-box${
              pending.length > 0 || findings.some((f) => f.level === "alert")
                ? " twin-box--attn"
                : ""
            }`}
          >
            <BoxHead
              code="01"
              title="等你决定"
              meta={
                findings.length > 0
                  ? `${pending.length} 待办 · ${findings.length} 建议`
                  : `${pending.length} 件`
              }
            />
            {pending.length === 0 && findings.length === 0 ? (
              <p className="twin-muted">队列是空的。</p>
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
                {/* 巡查建议：来自 2 分钟一次的心跳（零 LLM、纯机械判据）。
                    每条都给一个真实入口，不做"请注意一下"这类空提醒。 */}
                {findings.map((f, i) => {
                  const action = patrolAction(f.code);
                  return (
                    <div key={`pf-${f.code}-${i}`} className="twin-att__item">
                      <span className="twin-att__tag">建议</span>
                      <span className="twin-att__text">{f.text}</span>
                      <span className="twin-att__actions">
                        {action ? (
                          <button type="button" className="chip-flat" onClick={action.run}>
                            {action.label}
                          </button>
                        ) : null}
                      </span>
                    </div>
                  );
                })}
              </div>
            )}
          </section>

          <section className="twin-box twin-box--grow">
            {/* 计数与行数同源：`happenedRows` 既是渲染源也是读数源，
                不再出现「3 条」配 9 行。 */}
            <BoxHead
              code="02"
              title="刚才发生"
              meta={happenedRows.length > 0 ? `${happenedRows.length} 条` : undefined}
            />
            <div className="twin-rows">
              {happenedRows.length === 0 ? (
                <p className="twin-muted">
                  这会儿没有动作。她动手时，工具流水会实时出现在这里；做完的事进左翼「最近工作日志」。
                </p>
              ) : (
                <div aria-live="polite">
                  {happenedRows.map((r, i) => (
                    <div key={`ev-${i}`} className="twin-row">
                      <span className="twin-row__t">{r.t}</span>
                      <span className="twin-row__m">{r.text}</span>
                      <span className="twin-row__k">{r.tag}</span>
                    </div>
                  ))}
                </div>
              )}
            </div>
          </section>

          <section className="twin-box">
            <BoxHead code="03" title="她" />
            <div className="twin-who">
              <AliCharacter state={aliState} size={36} />
              <div className="twin-who__text">
                <span className="twin-who__name">阿黎</span>
                <span className="twin-who__line">{checkedText}</span>
                {/* 她这一轮在做哪一批需求 —— 与「任务面板」同一份批次，
                    不做第二份计数（两处各算一次必然对不上）。 */}
                <span className="twin-who__line">
                  {twin.batch
                    ? `本批 ${twin.batch.tasks.length} 条 · ${
                        twin.batch.status === "running"
                          ? "执行中"
                          : twin.batch.status === "done"
                            ? "已完成"
                            : "已失败"
                      }`
                    : "还没拉取需求"}
                </span>
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
      <footer className={`twin-bottom${twin.streaming ? " is-live" : ""}`}>
        <span className="twin-bottom__live">
          <i aria-hidden />
          {twin.streaming ? "这一轮在电脑上执行中" : "在电脑上执行"}
        </span>
        {/* 遥测条：仪表台的「帧头」—— 会话 / 周期 / 在跑 / 队列，全部是真值，
            随 8s 轮询联动；末尾光标块只表示「机器在线」。
            2026-10-28 删掉原先的 `CH ALI-01`：那是个写死的字符串，页面上没有
            「通道」这个概念，纯装饰冒充读数 —— 仪表台的每一格都必须能对上真值。 */}
        <span className="twin-bottom__tele">
          <span>
            SID <b>{TWIN_SESSION_ID}</b>
          </span>
          <span>
            TICK <b>{HISTORY_POLL_MS / 1000}s</b>
          </span>
          <span>
            RUN <b>{pad2(busy)}</b>
          </span>
          <span>
            Q <b>{pad2(pending.length)}</b>
          </span>
          <i className="twin-caret" aria-hidden />
        </span>
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
