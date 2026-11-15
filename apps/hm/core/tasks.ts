/**
 * 任务列表 + 中断 API（手机端遥控器视角）。
 *
 * ## 后端端点
 *   GET  /v1/tasks                      → TaskListResponse
 *   POST /v1/sessions/{sid}/interrupt   → 中断指定会话
 *
 * ## HTTP 认证（走 relay HTTP 代理）
 *   Authorization: Bearer <deviceToken>  — hermix-server 验证设备身份
 *   X-Hermix-Signature: …              — 请求完整性签名（与 WS 帧同算法）
 *   Authorization: Bearer <relayToken>  — relay 代理层认证
 *
 * ## 安全要点
 *   `GET /v1/tasks` 服务端已按 `origin.device_id()` 过滤（只回本设备的 sid），
 *   App 侧不做二次过滤——把过滤逻辑写在两端等于两份，漂移了谁都不知道。
 */
import { request } from './http'
import { relayBaseUrl } from './clock'
import { signatureHeader } from './signature'
import { deviceToken, relayToken } from './pairing'

/** 一个运行中或排队中的会话任务。 */
export interface TaskItem {
  sid: string
  state: 'running' | 'queued'
  /** 首条用户消息的截取（用于在列表里显示任务内容）。 */
  first_line: string
  /** 排队位次；running 时为 null。 */
  position: number | null
}

export interface TaskListResponse {
  tasks: TaskItem[]
}

/** 中断结果。 */
export interface InterruptResult {
  interrupted: boolean
  reason?: string
}

function apiBase(): string | null {
  return relayBaseUrl()
}

async function authHeaders(method: 'GET' | 'POST', path: string): Promise<Record<string, string>> {
  const dt = deviceToken()
  const rt = relayToken()
  const sig = await signatureHeader(dt, method, path)
  return {
    'Authorization': `Bearer ${rt || dt}`,
    'X-Device-Token': dt,
    'X-Hermix-Signature': sig,
  }
}

/** GET /v1/tasks — 取当前设备的全部任务。失败返空数组。 */
export async function getTasks(): Promise<TaskItem[]> {
  const base = apiBase()
  if (!base) return []
  const path = '/v1/tasks'
  const headers = await authHeaders('GET', path)
  const res = await request({
    url: `${base}${path}`,
    method: 'GET',
    header: headers,
    timeout: 10000,
  })
  if (!res.reachable || res.statusCode !== 200) return []
  const data = res.data as TaskListResponse
  return data.tasks ?? []
}

/**
 * POST /v1/sessions/{sid}/interrupt — 中断指定会话。
 *
 * `sid` 可为 null（当前还没有会话 / 配对流程的顺手清理）：没有会话就没有
 * 可中断的对象，如实返回，不发那次注定无效的请求。
 */
export async function interruptSession(sid: string | null): Promise<InterruptResult> {
  if (!sid) return { interrupted: false, reason: '当前没有会话' }
  const base = apiBase()
  if (!base) return { interrupted: false, reason: '未配对' }
  const path = `/v1/sessions/${encodeURIComponent(sid)}/interrupt`
  const headers = await authHeaders('POST', path)
  const res = await request({
    url: `${base}${path}`,
    method: 'POST',
    header: headers,
    timeout: 10000,
  })
  if (!res.reachable) return { interrupted: false, reason: '网络不可达' }
  if (res.statusCode === 200 || res.statusCode === 202 || res.statusCode === 204) {
    // 响应体 { interrupted: bool } — 会话没在跑时为 false，此时不应给「已中断」toast
    const data = res.data as { interrupted?: boolean } | null
    if (data && typeof data.interrupted === 'boolean') {
      return {
        interrupted: data.interrupted,
        reason: data.interrupted ? undefined : '会话不在运行中',
      }
    }
    // 旧版响应体无 interrupted 字段，视同成功（向后兼容）
    return { interrupted: true }
  }
  return { interrupted: false, reason: `服务端返回 ${res.statusCode}` }
}
