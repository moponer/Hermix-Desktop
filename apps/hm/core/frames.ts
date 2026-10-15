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

export type MobileEvent =
  | { kind: 'user.echo'; content: string }
  | { kind: 'assistant.delta'; content: string }
  | { kind: 'tool.start'; tool: string }
  | { kind: 'tool.end'; tool: string }
  | { kind: 'approval.request'; requestId: string; tool: string; canPersist: boolean }
  | { kind: 'turn.done' }
  | { kind: 'turn.error'; content: string }
  | { kind: 'turn.busy' }
  | { kind: 'unknown'; raw: string }

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
      return { kind: 'turn.error', content: str(f.content) }
    case 'turn.busy':
      return { kind: 'turn.busy' }
    default:
      return { kind: 'unknown', raw }
  }
}

function str(v: unknown): string {
  return typeof v === 'string' ? v : ''
}

// ---------------------------------------------------------------------------
// 会话状态机
// ---------------------------------------------------------------------------

export interface ChatState {
  /** 正在流的 assistant 气泡 id（首帧增量时创建，后续 append 到它） */
  streamingId: string | null
  streamingText: string
  runningTools: string[]
  doneTools: string[]
  turnActive: boolean
  lastError: string | null
  /** 服务端说"上一轮还在跑"——用于提示用户别重复发 */
  busy: boolean
}

export function initialChatState(): ChatState {
  return {
    streamingId: null,
    streamingText: '',
    runningTools: [],
    doneTools: [],
    turnActive: false,
    lastError: null,
    busy: false,
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
      return { ...state, turnActive: false, busy: false, runningTools: [] }
    case 'turn.error':
      return { ...state, turnActive: false, busy: false, lastError: ev.content || '出错了' }
    case 'turn.busy':
      return { ...state, busy: true }
    case 'approval.request':
    case 'user.echo':
    case 'unknown':
    default:
      return state
  }
}
