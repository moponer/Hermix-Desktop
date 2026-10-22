/**
 * 增量帧分派：把 relay 送来的 App 事件变成 UI 能直接用的状态变化。
 *
 * 收下的事件名由 PC 侧在 **relay 边界**映射（`mobile_protocol::to_mobile_frame`），
 * 所以这里**不认** hermix 的 `ProtocolMessage` type（`tool_call` / `request_approval`
 * 那套）——那套是 stdio 通道的老协议。见 `toMobileEvent` 的对应表。
 *
 * 分派成"状态变化"而不是"直接改 UI"是为了能单测：把整段渲染逻辑摊平成
 * 纯函数，才知道每类帧到底让界面发生了什么。
 */

import type { ModelEntry, ProviderCredential } from './model'

export type MobileEvent =
  | { kind: 'user.echo'; content: string }
  | { kind: 'assistant.delta'; content: string }
  // 思考过程折叠块（与桌面端同一个 <think> 解析源）
  | { kind: 'assistant.thinking'; content: string }
  | { kind: 'tool.start'; tool: string }
  | { kind: 'tool.end'; tool: string }
  | { kind: 'approval.request'; requestId: string; tool: string; canPersist: boolean }
  | { kind: 'approval.answered'; requestId: string }
  | { kind: 'turn.done' }
  // 错误必须带**原因**：桌面侧 StreamEvent::Error 的字段是 code/message/detail，
  // 中继映射此前只透传 `content`（而 error 事件根本没有 content 字段）→ 手机
  // 只能显示兜底文案「出错了」，真实原因（LLM 401 / 参数非法 / 5xx）被吞掉。
  | { kind: 'turn.error'; content: string; code: string; detail: string }
  | { kind: 'turn.busy' }
  // 桌面推来的模型清单（手机连上即下发）——手机端唯一的模型来源
  | { kind: 'model.list'; models: ModelEntry[]; providers: ProviderCredential[]; defaultModel: string | null }
  // 上下文窗口占用（持久条）：used/max/threshold/percent
  | { kind: 'context.usage'; used: number; max: number; threshold: number; percent: number }
  // 单轮 token 用量（cache 命中率等）：prompt/completion/cacheRead/cacheCreation/reasoning
  | {
      kind: 'usage.update'
      prompt: number
      completion: number
      cacheRead: number
      cacheCreation: number
      reasoning: number
    }
  // 待办列表（对方 To-dos 卡片）：snapshot 全量覆盖，upsert/removed 增量合并
  | { kind: 'todo.snapshot'; items: TodoItem[] }
  | { kind: 'todo.item_upserted'; item: TodoItem }
  | { kind: 'todo.item_removed'; id: string }
  | { kind: 'todo.group_started'; groupId: string; groupTitle: string | null }
  | { kind: 'unknown'; raw: string }

export interface TodoItem {
  id: string
  title: string
  status: string
  priority?: string
  description?: string
  last_result?: string
  execution_count?: number
  group_id?: string
  group_title?: string
}

/** 解析一帧。脏数据返回 `unknown` 而不是抛——一帧坏数据不该打断整条会话。 */
export function dispatch(raw: string): MobileEvent {
  let f: any
  try {
    f = JSON.parse(raw)
  } catch {
    return { kind: 'unknown', raw }
  }
  const type = f?.type
  if (typeof type !== 'string') return { kind: 'unknown', raw }

  switch (type) {
    case 'user.message':
      return { kind: 'user.echo', content: str(f.content) }
    case 'assistant.message':
      return { kind: 'assistant.delta', content: str(f.content) }
    case 'assistant.thinking':
      return { kind: 'assistant.thinking', content: str(f.content) }
    case 'tool.start':
      return { kind: 'tool.start', tool: str(f.tool) }
    case 'tool.end':
      return { kind: 'tool.end', tool: str(f.tool) }
    case 'approval.request':
      return {
        kind: 'approval.request',
        requestId: str(f.request_id),
        tool: str(f.tool),
        // 缺这个字段时按"不可用"处理：能力位矩阵对配对设备无条件 Denied，
        // 缺字段就当"能永久放行"是危险的默认值。
        canPersist: f.can_persist === true,
      }
    case 'turn.done':
      return { kind: 'turn.done' }
    case 'turn.error':
      return {
        kind: 'turn.error',
        content: str(f.message) || str(f.content),
        code: str(f.code),
        detail: str(f.detail),
      }
    case 'turn.busy':
      return { kind: 'turn.busy' }
    case 'model.list':
      return {
        kind: 'model.list',
        models: Array.isArray(f.models) ? f.models.map(toModelEntry) : [],
        providers: Array.isArray(f.providers) ? f.providers.map(toProvider) : [],
        defaultModel: f.default_model == null ? null : str(f.default_model),
      }
    case 'context.usage':
      return {
        kind: 'context.usage',
        used: num(f.used_tokens),
        max: num(f.max_tokens),
        threshold: num(f.threshold_tokens),
        percent: num(f.percent),
      }
    case 'usage.update':
      return {
        kind: 'usage.update',
        prompt: num(f.prompt_tokens),
        completion: num(f.completion_tokens),
        cacheRead: num(f.cache_read_tokens),
        cacheCreation: num(f.cache_creation_tokens),
        reasoning: num(f.reasoning_tokens),
      }
    case 'todo.snapshot':
      return { kind: 'todo.snapshot', items: toTodos(f.items) }
    case 'todo.item_upserted':
      return { kind: 'todo.item_upserted', item: toTodo(f.item) }
    case 'todo.item_removed':
      return { kind: 'todo.item_removed', id: str(f.id) }
    case 'todo.group_started':
      return {
        kind: 'todo.group_started',
        groupId: str(f.group_id),
        groupTitle: f.group_title == null ? null : str(f.group_title),
      }
    default:
      return { kind: 'unknown', raw }
  }
}

function str(v: unknown): string {
  return typeof v === 'string' ? v : ''
}
function num(v: unknown): number {
  return typeof v === 'number' && isFinite(v) ? v : 0
}
function toTodo(raw: any): TodoItem {
  return {
    id: str(raw?.id),
    title: str(raw?.title),
    status: str(raw?.status) || 'pending',
    priority: raw?.priority == null ? undefined : str(raw.priority),
    description: raw?.description == null ? undefined : str(raw.description),
    last_result: raw?.last_result == null ? undefined : str(raw.last_result),
    execution_count:
      typeof raw?.execution_count === 'number' ? raw.execution_count : undefined,
    group_id: raw?.group_id == null ? undefined : str(raw.group_id),
    group_title: raw?.group_title == null ? undefined : str(raw.group_title),
  }
}
function toTodos(raw: any): TodoItem[] {
  return Array.isArray(raw) ? raw.map(toTodo) : []
}
/** 桌面推来的模型条目（与桌面 `/api/model/list` 的 `models` 同形）。 */
function toModelEntry(raw: any): ModelEntry {
  return {
    provider_id: str(raw?.provider_id),
    model_id: str(raw?.model_id),
    provider_name: raw?.provider_name == null ? undefined : str(raw.provider_name),
    context_window:
      typeof raw?.context_window === 'number' ? raw.context_window : undefined,
    supports_reasoning:
      typeof raw?.supports_reasoning === 'boolean' ? raw.supports_reasoning : undefined,
  }
}
/** 桌面推来的 provider 凭据（手机离线时直连云模型用）。 */
function toProvider(raw: any): ProviderCredential {
  return {
    provider_id: str(raw?.provider_id),
    base_url: raw?.base_url == null ? undefined : str(raw.base_url),
    api_key: raw?.api_key == null ? undefined : str(raw.api_key),
    api_style: str(raw?.api_style) || 'openai',
  }
}
/** todo.item_upserted：按 id 合并（已存在则覆盖，否则追加） */
function mergeTodo(list: TodoItem[], item: TodoItem): TodoItem[] {
  const i = list.findIndex((t) => t.id === item.id)
  if (i < 0) return [...list, item]
  const next = list.slice()
  next[i] = item
  return next
}

// ---------------------------------------------------------------------------
// 会话状态机
// ---------------------------------------------------------------------------

export interface ChatState {
  /** 正在流的 assistant 气泡 id（首帧增量时创建，后续 append 到它） */
  streamingId: string | null
  streamingText: string
  /** 思考过程（<think> 折叠块），按轮累积，turn 落定时随气泡带走并清空 */
  thinking: string
  runningTools: string[]
  doneTools: string[]
  turnActive: boolean
  lastError: string | null
  /** 服务端说"上一轮还在跑"——用于提示用户别重复发 */
  busy: boolean
  /** 上下文窗口占用（对方「上下文仪表」）：used/max/threshold/percent */
  context: { used: number; max: number; threshold: number; percent: number } | null
  /** 单轮 token 用量（对方助手操作条上的 cache 命中率等） */
  usage: {
    prompt: number
    completion: number
    cacheRead: number
    cacheCreation: number
    reasoning: number
  } | null
  /** 待办列表（对方 To-dos 卡片） */
  todos: TodoItem[]
  /**
   * 桌面推来的模型清单。手机端**不自己存**清单——唯一真相源在桌面，
   * 手机连上时由桌面经 WS 推下来（`model.list` 事件）。
   */
  models: ModelEntry[]
  /** 桌面当前的默认模型（`provider:model`）。 */
  defaultModel: string | null
  /**
   * 电脑推来的 provider 凭据（手机离线时直连云模型用）。
   * 电脑关机后，这份就是唯一能用的凭据——所以会被持久化到本地。
   */
  providers: ProviderCredential[]
  /**
   * 正在等的工具审批。审批是**对话里的一条消息**：PC 发 `approval.request`
   * 停在这里等人点，点了发 `approval_response` 回 PC，agent 才继续。
   *
   * 之所以放进会话状态而不是让页面另开一条轮询：审批和回答、工具轨迹一样，
   * 都属于「这一轮正在发生什么」。分成两套状态机就会出现「对话里看不到、
   * 另一个页面才有」的割裂。
   */
  pendingApproval: PendingApproval | null
}

export interface PendingApproval {
  requestId: string
  tool: string
  /** 配对设备恒为 false（永久放行只能在电脑端操作）。 */
  canPersist: boolean
}

export function initialChatState(): ChatState {
  return {
    streamingId: null,
    streamingText: '',
    thinking: '',
    runningTools: [],
    doneTools: [],
    turnActive: false,
    lastError: null,
    busy: false,
    context: null,
    usage: null,
    todos: [],
    models: [],
    defaultModel: null,
    providers: [],
    pendingApproval: null,
  }
}

let seq = 0
const nextId = () => `a${++seq}`

/**
 * 吃一帧，返回**新状态**（不可变更新）。
 *
 * 工具轨迹的语义抄 IM 端 `ToolTrace`：工具**开始**才进列表，**结束**只是标记
 * 完成；正文又开始流时，已跑完的工具从"在跑"里撤掉——否则一行五个 ✅ 会把
 * 真正的回答挤下去。
 */
export function reduce(state: ChatState, ev: MobileEvent): ChatState {
  switch (ev.kind) {
    case 'assistant.delta': {
      if (!ev.content) return state
      return {
        ...state,
        streamingId: state.streamingId ?? nextId(),
        streamingText: state.streamingText + ev.content,
        turnActive: true,
        // 回答又在流了 → 收工的工具从"在跑"里撤掉
        runningTools: state.runningTools.filter((t) => !state.doneTools.includes(t)),
      }
    }
    case 'assistant.thinking': {
      if (!ev.content) return state
      return {
        ...state,
        thinking: state.thinking + ev.content,
        turnActive: true,
      }
    }
    case 'tool.start':
      return {
        ...state,
        turnActive: true,
        runningTools: state.runningTools.includes(ev.tool)
          ? state.runningTools
          : [...state.runningTools, ev.tool],
      }
    case 'tool.end':
      return {
        ...state,
        runningTools: state.runningTools.filter((t) => t !== ev.tool),
        doneTools: state.doneTools.includes(ev.tool)
          ? state.doneTools
          : [...state.doneTools, ev.tool],
      }
    case 'turn.done':
      return {
        ...state,
        turnActive: false,
        busy: false,
        runningTools: [],
        // 一轮落定：思考过程已被气泡带走（页面在 doneAtTurnEnd 取走），清掉防串台
        thinking: '',
        pendingApproval: null,
      }
    case 'turn.error':
      return {
        ...state,
        turnActive: false,
        busy: false,
        // 真实原因优先；都没有才退到兜底文案。
        lastError: ev.content || ev.code || '出错了',
        thinking: '',
        // 这一轮已经结束，还挂着的审批没有人会等它了
        pendingApproval: null,
      }
    case 'model.list':
      return {
        ...state,
        models: ev.models,
        providers: ev.providers,
        defaultModel: ev.defaultModel,
      }
    case 'turn.busy':
      return { ...state, busy: true }
    case 'context.usage':
      return {
        ...state,
        context: {
          used: ev.used,
          max: ev.max,
          threshold: ev.threshold,
          percent: ev.percent,
        },
      }
    case 'usage.update':
      return { ...state, usage: ev }
    case 'todo.snapshot':
      return { ...state, todos: ev.items }
    case 'todo.item_upserted':
      return {
        ...state,
        todos: mergeTodo(state.todos, ev.item),
      }
    case 'todo.item_removed':
      return {
        ...state,
        todos: state.todos.filter((t) => t.id !== ev.id),
      }
    case 'todo.group_started': {
      // 新计划组：把组标题刷到同组 item 上，UI 据此分组渲染
      if (!ev.groupTitle) return state
      return {
        ...state,
        todos: state.todos.map((t) =>
          t.group_id === ev.groupId && !t.group_title ? { ...t, group_title: ev.groupTitle! } : t,
        ),
      }
    }
    case 'approval.request':
      // 停在对话里等人点。`turnActive` 置真：agent 正卡在等这个回答。
      return {
        ...state,
        turnActive: true,
        pendingApproval: {
          requestId: ev.requestId,
          tool: ev.tool,
          canPersist: ev.canPersist,
        },
      }
    case 'approval.answered':
      // 只清自己那条：期间可能已经来了下一条（批量审批），别误清。
      return state.pendingApproval?.requestId === ev.requestId
        ? { ...state, pendingApproval: null }
        : state
    case 'user.echo':
    case 'unknown':
    default:
      return state
  }
}
