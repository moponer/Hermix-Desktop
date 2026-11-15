/**
 * 帧分派与会话状态机。
 *
 * 这里刻意做成纯函数：把「一帧进来后界面变成什么样」摊平了才知道每类帧的
 * 行为，而嵌在 Vue 组件里的 reducer 改坏了不会有任何测试报警。
 */
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { dispatch, reduce, initialChatState, type ChatState } from '../core/frames'

function feed(state: ChatState, ...frames: string[]): ChatState {
  return frames.reduce((s, raw) => reduce(s, dispatch(raw)), state)
}

// ------------------------------------------------------------------ dispatch

test('事件名对齐 PC 侧映射（不认 hermix 那套 type）', () => {
  assert.equal(dispatch('{"type":"assistant.message","content":"hi"}').kind, 'assistant.delta')
  assert.equal(dispatch('{"type":"tool.start","tool":"bash"}').kind, 'tool.start')
  assert.equal(dispatch('{"type":"turn.done"}').kind, 'turn.done')
  assert.equal(dispatch('{"type":"turn.busy"}').kind, 'turn.busy')
  // **映射前**的 type 不该被当成 App 帧。映射由 PC 在 relay 边界完成
  // （mobile_protocol::to_mobile_frame），所以 App 永远见不到 `done` /
  // `tool_call` / `request_approval` 这套——见到就说明边界漏了。
  for (const pre of ['done', 'error', 'tool_call', 'tool_result', 'request_approval', 'assistant_message', 'user_message']) {
    assert.equal(
      dispatch(`{"type":"${pre}"}`).kind,
      'unknown',
      `App 不该认得 hermix 的 "${pre}" —— 那是映射前的 type`,
    )
  }
})

test('脏数据不抛 —— 一帧坏数据不该打断整条会话', () => {
  assert.equal(dispatch('not json').kind, 'unknown')
  assert.equal(dispatch('{}').kind, 'unknown')
  assert.equal(dispatch('{"type":42}').kind, 'unknown')
  assert.equal(dispatch('[]').kind, 'unknown')
})

test('审批帧缺 can_persist 时按不可用处理', () => {
  // 缺字段就当"能永久放行"是危险默认值：能力位矩阵对配对设备无条件 Denied
  const a = dispatch('{"type":"approval.request","request_id":"r1","tool":"terminal"}')
  assert.equal(a.kind, 'approval.request')
  assert.equal((a as any).canPersist, false)
  const b = dispatch('{"type":"approval.request","request_id":"r1","tool":"terminal","can_persist":false}')
  assert.equal((b as any).canPersist, false)
})

// ------------------------------------------------------------------- reduce

test('assistant 增量串成一个气泡，不每帧新建', () => {
  let s = feed(
    initialChatState(),
    '{"type":"assistant.message","content":"先看"}',
    '{"type":"assistant.message","content":"dispatch"}',
  )
  assert.equal(s.streamingText, '先看dispatch')
  const id = s.streamingId
  s = feed(s, '{"type":"assistant.message","content":"。"}')
  assert.equal(s.streamingId, id, '同一轮必须是同一个气泡')
  assert.equal(s.streamingText, '先看dispatch。')
})

test('工具开始进"在跑"、结束只标记完成', () => {
  let s = feed(
    initialChatState(),
    '{"type":"tool.start","tool":"read_file"}',
    '{"type":"tool.start","tool":"bash"}',
  )
  assert.deepEqual(s.runningTools, ['read_file', 'bash'])

  s = feed(s, '{"type":"tool.end","tool":"bash"}')
  assert.deepEqual(s.runningTools, ['read_file'])
  assert.deepEqual(s.doneTools, ['bash'])
})

test('重复 start 不会把同一个工具列两次', () => {
  let s = feed(
    initialChatState(),
    '{"type":"tool.start","tool":"grep"}',
    '{"type":"tool.start","tool":"grep"}',
  )
  assert.deepEqual(s.runningTools, ['grep'])
  s = feed(s, '{"type":"tool.end","tool":"grep"}', '{"type":"tool.end","tool":"grep"}')
  assert.deepEqual(s.doneTools, ['grep'])
})

test('正文又开始流时，已跑完的工具从"在跑"里撤掉', () => {
  // 抄 IM 端 ToolTrace 的语义：一行五个 ✅ 会把真正的回答挤下去
  let s = feed(
    initialChatState(),
    '{"type":"tool.start","tool":"read_file"}',
    '{"type":"tool.end","tool":"read_file"}',
    '{"type":"tool.start","tool":"bash"}',
  )
  assert.deepEqual(s.runningTools, ['bash'])
  s = feed(s, '{"type":"assistant.message","content":"结论是…"}')
  assert.deepEqual(s.runningTools, ['bash'], '仍在跑的工具要保留')
  s = feed(s, '{"type":"tool.end","tool":"bash"}')
  s = feed(s, '{"type":"assistant.message","content":"…"}')
  assert.deepEqual(s.runningTools, [], '跑完的撤掉')
})

test('turn.done 收尾并清掉在跑工具', () => {
  let s = feed(
    initialChatState(),
    '{"type":"tool.start","tool":"bash"}',
    '{"type":"assistant.message","content":"好了"}',
    '{"type":"turn.done"}',
  )
  assert.equal(s.turnActive, false)
  assert.deepEqual(s.runningTools, [])
  // streamingText 要留给 UI 落成最终气泡，不能被 done 清掉
  assert.equal(s.streamingText, '好了')
})

test('turn.busy 提示"上一轮还在跑"，turn.done 解除', () => {
  let s = feed(initialChatState(), '{"type":"turn.busy"}')
  assert.equal(s.busy, true)
  s = feed(s, '{"type":"turn.done"}')
  assert.equal(s.busy, false)
})

test('turn.error 记下原因并收尾', () => {
  let s = feed(
    initialChatState(),
    '{"type":"assistant.message","content":"x"}',
    '{"type":"turn.error","content":"后端 503"}',
  )
  assert.equal(s.turnActive, false)
  assert.equal(s.lastError, '后端 503')
})

test('turn.error 优先显示桌面给的真实原因，而不是兜底文案', () => {
  // 桌面侧 StreamEvent::Error 的字段是 code/message/detail（没有 content）。
  // 映射不透传它们时，手机端只能显示"出错了"，真实原因被整个吞掉。
  const s = feed(
    initialChatState(),
    '{"type":"turn.error","code":"llm_error","message":"401 invalid api_key"}',
  )
  assert.equal(s.lastError, '401 invalid api_key')
})

test('turn.error 没有 message 时退回 code', () => {
  const s = feed(initialChatState(), '{"type":"turn.error","code":"model_not_found"}')
  assert.equal(s.lastError, 'model_not_found')
})

// ------------------------------------------------------------- 模型清单

test('model.list 把桌面推来的清单装进会话状态', () => {
  const s = feed(
    initialChatState(),
    '{"type":"model.list","models":[{"provider_id":"deepseek","model_id":"chat","provider_name":"DeepSeek"}],"default_model":"deepseek:chat"}',
  )
  assert.equal(s.models.length, 1)
  assert.equal(s.models[0].provider_id, 'deepseek')
  assert.equal(s.models[0].model_id, 'chat')
  assert.equal(s.defaultModel, 'deepseek:chat')
})

test('model.list 空清单也合法（桌面没配模型时）', () => {
  const s = feed(initialChatState(), '{"type":"model.list","models":[],"default_model":null}')
  assert.deepEqual(s.models, [])
  assert.equal(s.defaultModel, null)
})

test('model.list 带上 provider 凭据（电脑离线时手机直连云模型要用）', () => {
  const s = feed(
    initialChatState(),
    '{"type":"model.list","models":[{"provider_id":"deepseek","model_id":"chat"}],"providers":[{"provider_id":"deepseek","base_url":"https://api.deepseek.com/v1","api_key":"sk-x","api_style":"openai"}],"default_model":"deepseek:chat"}',
  )
  assert.equal(s.providers.length, 1)
  assert.equal(s.providers[0].provider_id, 'deepseek')
  assert.equal(s.providers[0].base_url, 'https://api.deepseek.com/v1')
  assert.equal(s.providers[0].api_key, 'sk-x')
})

test('空增量不新建气泡', () => {
  const s = feed(initialChatState(), '{"type":"assistant.message","content":""}')
  assert.equal(s.streamingId, null, '空内容不该凭空开一个气泡')
  assert.equal(s.streamingText, '')
})

// ------------------------------------------------------------- 工具审批

test('审批帧停在会话状态里等人点', () => {
  const s = feed(
    initialChatState(),
    '{"type":"approval.request","request_id":"r1","tool":"terminal","can_persist":false}',
  )
  assert.deepEqual(s.pendingApproval, {
    requestId: 'r1',
    tool: 'terminal',
    canPersist: false,
  })
  // PC 正卡在等这个回答，所以这一轮仍然是「进行中」
  assert.equal(s.turnActive, true)
})

test('缺 can_persist 时按不可永久放行处理', () => {
  const s = feed(
    initialChatState(),
    '{"type":"approval.request","request_id":"r1","tool":"terminal"}',
  )
  assert.equal(s.pendingApproval?.canPersist, false, '危险默认值不能是「能永久放行」')
})

test('答复后只清掉自己那条', () => {
  const s = feed(
    initialChatState(),
    '{"type":"approval.request","request_id":"r1","tool":"terminal","can_persist":false}',
  )
  const cleared = reduce(s, { kind: 'approval.answered', requestId: 'r1' })
  assert.equal(cleared.pendingApproval, null)

  // 期间来了下一条（批量审批），回答旧的不能把新的清掉
  const s2 = reduce(s, { kind: 'approval.answered', requestId: 'other' })
  assert.equal(s2.pendingApproval?.requestId, 'r1')
})

test('一轮结束时不留悬空审批', () => {
  const s = feed(
    initialChatState(),
    '{"type":"approval.request","request_id":"r1","tool":"terminal","can_persist":false}',
    '{"type":"turn.done"}',
  )
  assert.equal(s.pendingApproval, null, '这一轮都结束了，没有人还会等它')
})
