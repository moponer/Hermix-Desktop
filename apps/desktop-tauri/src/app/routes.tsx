// 路由表与页面渲染分发，从 App.tsx 抽出。
// App 用 `page` 状态做页面切换（无独立路由库），此处把「page → 视图组件」
// 的映射集中为一张路由表，App 主区只需调用 renderRoutes 即可。
import type { ReactNode } from "react";
import type { NavPage, Session } from "../types";
import { AutomationView } from "../components/AutomationView";
import { AgentsView } from "../components/AgentsView";
import { AnalyticsView } from "../components/AnalyticsView";
import { CustomAppsView } from "../components/CustomAppsView";
import { WorkbenchView } from "../components/WorkbenchView";

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
}

/** 按当前 page 渲染对应视图。与 App 原条件渲染链 1:1 等价。 */
export function renderRoutes(page: NavPage, ctx: RouteContext): ReactNode {
  switch (page) {
    case "chat":
      return ctx.chatNode;
    case "automation":
      return <AutomationView />;
    case "agents":
      return <AgentsView />;
    case "experts":
      return <AgentsView initialTab="builtin" />;
    case "analytics":
      return <AnalyticsView />;
    case "customapps":
      return <CustomAppsView onStartCreateApp={ctx.startCreateAppFlow} />;
    case "workbench":
      return (
        <WorkbenchView
          chatNode={ctx.chatNode}
          onSelectSession={ctx.selectSessionOnly}
          onRegisterSessions={ctx.upsertSessions}
        />
      );
    default:
      return null;
  }
}
