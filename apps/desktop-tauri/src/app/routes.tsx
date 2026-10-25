// 路由表与页面渲染分发，从 App.tsx 抽出。
// App 用 `page` 状态做页面切换（无独立路由库），此处把「page → 视图组件」
// 的映射集中为一张路由表，App 主区只需调用 renderRoutes 即可。
import { lazy, Suspense, type ReactNode } from "react";
import type { NavPage, Session } from "../types";
import { Skeleton, SkeletonCard } from "../components/ui";

// 重视图（ECharts / 大表单 / 全量列表）懒加载：首次进入才求值模块并挂载，
// 且切走即卸载，不占常驻内存。chat 页面是首屏主路径，保持静态导入。
const AutomationView = lazy(() => import("../components/AutomationView").then((m) => ({ default: m.AutomationView })));
const MissionBoardView = lazy(() => import("../components/MissionBoardView").then((m) => ({ default: m.MissionBoardView })));
const AgentsView = lazy(() => import("../components/AgentsView").then((m) => ({ default: m.AgentsView })));
const AnalyticsView = lazy(() => import("../components/AnalyticsView").then((m) => ({ default: m.AnalyticsView })));
const CustomAppsView = lazy(() => import("../components/CustomAppsView").then((m) => ({ default: m.CustomAppsView })));
const WorkbenchView = lazy(() => import("../components/WorkbenchView").then((m) => ({ default: m.WorkbenchView })));
const VaultPanel = lazy(() => import("../components/VaultPanel").then((m) => ({ default: m.VaultPanel })));
const TwinView = lazy(() => import("../components/TwinView").then((m) => ({ default: m.TwinView })));

/**
 * 阿黎（Twin）的会话 id。
 *
 * - 桌面：她的页面读写这一条；侧栏按 `twin:` 前缀把它排除在对话列表之外。
 * - 手机：将来经设备通道带 `target` 打到同一条上（同一份历史）。
 * - PC：这一轮的 `session_id` 用它，历史落库与两边视图同源。
 */
export const TWIN_SESSION_ID = "twin:ali";

/** 内联对话随这一轮下发的两项设置（都来自她那页的可见控件）。 */
export interface TwinSendOptions {
  /** 顶部「权限」开关：默认会为高风险操作弹审批。 */
  permissionLevel: "default" | "full";
  /** 底部动作模式（盯住 / 派活 / 总结）对应的这一轮指令。 */
  systemPrompt?: string;
}

/** 她这一轮在干什么 —— 她那页用它就地渲染，不依赖宿主会话表。 */
export interface TwinTurn {
  user: string;
  reply: string;
  /** 当前正在跑的工具名（工具调用期间有值），给一行动态提示用。 */
  activity: string | null;
  error: string | null;
}

/** 等她拍板的一件事：审批（工具）或澄清（问题）。 */
export interface TwinPendingDecision {
  id: string;
  kind: "approval" | "clarify";
  tool?: string;
  reason?: string | null;
  question?: string;
  choices?: string[];
}

/**
 * 「助手阿黎」页的独立发送器。
 *
 * 她有自己的 `useStreamChat` 实例（挂在 App 顶层，切页不断流），因此她的
 * 一轮执行与任何对话的一轮执行**完全并行**：两边各自一条 `stream:<sid>`
 * 通道、各自一套发送状态，互不禁用、互不串台。
 */
export interface TwinInlineApi {
  /** 投递这一轮；false = 没发出去（错误在 `turn.error`）。 */
  send: (text: string, opts: TwinSendOptions) => Promise<boolean>;
  /** 停止当前这一轮。 */
  stop: () => void;
  streaming: boolean;
  /** 本轮（内存态）—— 与落库历史分开，避免流式期间被轮询覆盖。 */
  turn: TwinTurn | null;
  /** 未决的审批 / 澄清，按到达顺序。 */
  pending: TwinPendingDecision[];
  /** 结算一件待决事项：审批给 decision，澄清把选项放进 answer。 */
  resolve: (id: string, decision: "approved" | "denied", answer?: string) => void;
}

/** 渲染各非 chat 页面所需的 App 级依赖。chat 页面 props 复杂，
 *  由 App 预先渲染为 `chatNode` 元素传入，保持逻辑零改写。 */
export interface RouteContext {
  sessions: Session[];
  /** 切会话并跳回对话页 —— 侧栏、搜索结果用这个。 */
  handleSelectSession: (id: string) => void;
  /** 只切会话，不动当前页面 —— 工作台页面用这个。 */
  selectSessionOnly: (id: string) => void;
  /** 「助手阿黎」页的内联对话（独立发送器，见 `TwinInlineApi`）。 */
  twin: TwinInlineApi;
  /**
   * 把会话登记进宿主的会话表，已存在的跳过。
   *
   * 工作台的对话不在主对话列表里（列表按归属过滤过），但对话栏复用
   * 的是宿主那份 ChatView，它只认宿主会话表里的对象 —— 不登记就渲染
   * 不出来。登记只补不覆盖，避免把已加载好的消息冲成空。
   */
  upsertSessions: (list: Session[]) => void;
  startCreateAppFlow: () => void;
  /** 工作流页「通过对话创建」：进入新对话并预填工作流设计指令。 */
  startCreateWorkflowFlow: () => void;
  chatNode: ReactNode;
  /** 当前激活会话 id —— 工作台浮窗历史下拉高亮当前对话线。 */
  activeSessionId: string | null;
  /** 工作流运行后的跳转：切回对话页（新对话空屏），不落在旧激活会话上。 */
  openChat: () => void;
}

/** 按当前 page 渲染对应视图。与 App 原条件渲染链 1:1 等价。 */
export function renderRoutes(page: NavPage, ctx: RouteContext): ReactNode {
  switch (page) {
    case "chat":
      return ctx.chatNode;
    case "twin":
      // 阿黎吃宿主已有的会话表（避免再拉一次全量）；对话走她自己的发送器。
      // `twin:ali` 在服务端是独立会话，这里也有独立的流 —— 她与任何对话并行，
      // 回复就地显示，审批/澄清也在她自己页面处理。
      return (
        <Suspense fallback={<RouteLoading />}>
          <TwinView
            sessions={ctx.sessions}
            onOpenChat={() => ctx.handleSelectSession(TWIN_SESSION_ID)}
            twin={ctx.twin}
          />
        </Suspense>
      );
    case "mission":
      return <Suspense fallback={<RouteLoading />}><MissionBoardView /></Suspense>;
    case "automation":
      return <Suspense fallback={<RouteLoading />}><AutomationView openChat={ctx.openChat} onCreateWorkflow={ctx.startCreateWorkflowFlow} /></Suspense>;
    case "experts":
      return <Suspense fallback={<RouteLoading />}><AgentsView initialTab="builtin" /></Suspense>;
    case "analytics":
      return <Suspense fallback={<RouteLoading />}><AnalyticsView /></Suspense>;
    case "customapps":
      return <Suspense fallback={<RouteLoading />}><CustomAppsView onStartCreateApp={ctx.startCreateAppFlow} /></Suspense>;
    case "vault":
      return <Suspense fallback={<RouteLoading />}><VaultPanel /></Suspense>;
    case "workbench":
      return (
        <Suspense fallback={<RouteLoading />}>
          <WorkbenchView
            chatNode={ctx.chatNode}
            onSelectSession={ctx.selectSessionOnly}
            onRegisterSessions={ctx.upsertSessions}
            activeSessionId={ctx.activeSessionId}
          />
        </Suspense>
      );
    default:
      return null;
  }
}

function RouteLoading() {
  // 骨架屏而非「加载中…」文字——ui/Skeleton 的规范（感知更快、零布局跳动）。
  //
  // 2026-10-23 修复「进页面时闪一个竖长灰胶囊」：
  // 旧实现是 `flex flex-col items-center` 里塞一张 SkeletonCard。
  // items-center 让子项按**内容宽度**收缩，而 SkeletonCard 内部最宽只有
  // w-1/3 的文字条 —— 整张卡被压成约 30px 宽，p-4 又撑出 ~110px 高，
  // 屏幕上于是出现一个带描边的竖胶囊（用户报的那个）。
  //
  // 现在改成**与真实页面同构**的骨架：页头（图标 + 标题 + 副标题）
  // + 双列卡片网格。结构对上之后，模块加载完成时不会跳版 ——
  // 这才是「零布局跳动」本来的意思，而不是"随便放一个灰块"。
  return (
    <div className="flex-1 overflow-hidden p-6" aria-busy="true">
      <div className="mb-6 flex items-start gap-3">
        <Skeleton className="h-9 w-9 rounded-[var(--radius-md)]" />
        <div className="flex flex-col gap-2 pt-0.5">
          <Skeleton className="h-4 w-28" />
          <Skeleton className="h-3 w-56" />
        </div>
      </div>
      <div className="grid grid-cols-1 gap-2 md:grid-cols-2">
        {[0, 1, 2, 3].map((i) => (
          <SkeletonCard key={i} lines={2} />
        ))}
      </div>
    </div>
  );
}
