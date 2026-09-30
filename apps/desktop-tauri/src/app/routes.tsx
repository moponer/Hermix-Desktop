// 路由表与页面渲染分发，从 App.tsx 抽出。
// App 用 `page` 状态做页面切换（无独立路由库），此处把「page → 视图组件」
// 的映射集中为一张路由表，App 主区只需调用 renderRoutes 即可。
import { lazy, Suspense, type ReactNode } from "react";
import type { NavPage, Session } from "../types";

// 重视图（ECharts / 大表单 / 全量列表）懒加载：首次进入才求值模块并挂载，
// 且切走即卸载，不占常驻内存。chat 页面是首屏主路径，保持静态导入。
const AutomationView = lazy(() => import("../components/AutomationView").then((m) => ({ default: m.AutomationView })));
const AgentsView = lazy(() => import("../components/AgentsView").then((m) => ({ default: m.AgentsView })));
const AnalyticsView = lazy(() => import("../components/AnalyticsView").then((m) => ({ default: m.AnalyticsView })));
const CustomAppsView = lazy(() => import("../components/CustomAppsView").then((m) => ({ default: m.CustomAppsView })));
const WorkbenchView = lazy(() => import("../components/WorkbenchView").then((m) => ({ default: m.WorkbenchView })));

/** 渲染各非 chat 页面所需的 App 级依赖。chat 页面 props 复杂，
 *  由 App 预先渲染为 `chatNode` 元素传入，保持逻辑零改写。 */
export interface RouteContext {
  sessions: Session[];
  /** 切会话并跳回对话页 —— 侧栏、搜索结果用这个。 */
  handleSelectSession: (id: string) => void;
  /** 只切会话，不动当前页面 —— 工作台页面用这个。 */
  selectSessionOnly: (id: string) => void;
  /**
   * 把会话登记进宿主的会话表，已存在的跳过。
   *
   * 工作台的对话不在主对话列表里（列表按归属过滤过），但对话栏复用
   * 的是宿主那份 ChatView，它只认宿主会话表里的对象 —— 不登记就渲染
   * 不出来。登记只补不覆盖，避免把已加载好的消息冲成空。
   */
  upsertSessions: (list: Session[]) => void;
  startCreateAppFlow: () => void;
  chatNode: ReactNode;
  /** 当前激活会话 id —— 工作台浮窗历史下拉高亮当前对话线。 */
  activeSessionId: string | null;
}

/** 按当前 page 渲染对应视图。与 App 原条件渲染链 1:1 等价。 */
export function renderRoutes(page: NavPage, ctx: RouteContext): ReactNode {
  switch (page) {
    case "chat":
      return ctx.chatNode;
    case "automation":
      return <Suspense fallback={<RouteLoading />}><AutomationView /></Suspense>;
    case "agents":
      return <Suspense fallback={<RouteLoading />}><AgentsView /></Suspense>;
    case "experts":
      return <Suspense fallback={<RouteLoading />}><AgentsView initialTab="builtin" /></Suspense>;
    case "analytics":
      return <Suspense fallback={<RouteLoading />}><AnalyticsView /></Suspense>;
    case "customapps":
      return <Suspense fallback={<RouteLoading />}><CustomAppsView onStartCreateApp={ctx.startCreateAppFlow} /></Suspense>;
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
  return (
    <div className="flex-1 flex items-center justify-center text-[color:var(--color-text-muted)] text-sm">
      加载中…
    </div>
  );
}
