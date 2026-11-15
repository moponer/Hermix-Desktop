/**
 * 语音侧的不变量。真正有安全含义的只有两处：**高危动词检测**与
 * **三态轮次**；provider 本身只是 HTTP 搬运，测它等于测 fetch。
 */
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { isRiskyInstruction, riskOf, NullAsr, NullTts, TurnTracker, type VoiceTurn } from '../core/voice'

// --------------------------------------------------------------- 高危动词

test('不可逆动作一律要发前确认', () => {
  const must = [
    '把 target 删掉',
    '清空这个目录',
    '清理一下缓存',
    '覆盖配置文件',
    '格式化磁盘',
    '重装一下',
    '推送到 main',
    '发布上线',
    '删',
    'rm -rf build',
    'DROP TABLE users',
    'kill 掉 1234',
  ]
  for (const t of must) {
    assert.equal(isRiskyInstruction(t), true, `「${t}」应判为需确认`)
  }
})

test('读写查类不误报（否则每次都要多点一次，久了就不看了）', () => {
  const safe = [
    '帮我看看这个文件',
    '跑一下测试',
    '读取配置',
    '搜索所有 TODO',
    '把这个函数重构一下',
    '写个脚本',
    'git log 看一下',
  ]
  for (const t of safe) {
    assert.equal(isRiskyInstruction(t), false, `「${t}」不该判为需确认`)
  }
})

test('否定句同样命中 —— 宁可多报', () => {
  // 漏报的代价是执行了不可逆动作；多报的代价是多一次确认。刻意不对称。
  assert.equal(isRiskyInstruction('不要删除任何东西'), true)
  assert.equal(isRiskyInstruction('千万别清理'), true)
})

test('riskOf 与 isRisky 一致', () => {
  assert.equal(riskOf('删掉它'), 'confirm')
  assert.equal(riskOf('读一下'), 'none')
})

// --------------------------------------------------------------- 空 provider

test('没配 key 时是空转而不是假装成功', async () => {
  const asr = new NullAsr()
  assert.equal(asr.enabled, false)
  await asr.start()
  await asr.push(new ArrayBuffer(8))
  const r = await asr.stop()
  // final 必须是 null（"没听清"），不能是 ''（会被当成"他说的是空话"而照发）
  assert.equal(r.final, null)

  const tts = new NullTts()
  assert.equal(tts.enabled, false)
  await tts.speak('不会真的出声') // 不得抛
  await tts.flush()
})

// --------------------------------------------------------------- 三态轮次

test('三态只报变化，不刷屏', () => {
  const seen: VoiceTurn[] = []
  const tracker = new TurnTracker()
  tracker.onChange((t) => seen.push(t))
  assert.deepEqual(seen, ['silence'], '订阅时先给当前态')

  tracker.youSpeaking()
  tracker.youSpeaking() // 重复
  tracker.agentSpeaking()
  tracker.silent()
  assert.deepEqual(seen, ['silence', 'you', 'agent', 'silence'])
  assert.equal(tracker.current, 'silence')
})
