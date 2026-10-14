/**
 * 待审批列表 + 审批决定 API（手机端遥控器视角）。
 *
 * ## 后端端点
 *   GET  /v1/sessions/{sid}/approvals/pending → 按会话批量拉取
 *   POST /v1/sessions/{sid}/approval           → 提交审批决定
 *
 * ## 手机端审批列表的特殊性
 *   `GET /v1/sessions/{sid}/approvals/pending` 需要指定 sid，而手机在 relay
 *   模式下只有本设备那一个会话。我们不知道它何时有审批——轮询 tasks 接口
 *   可以拿到 sid，再对那个 sid 拉 approvals。
 *
 *   **如果只有一个设备级端点**（`GET /v1/approvals`），则取消这里的 sid 参数
 *   逻辑，改为直接调用该端点。当前实现是：如果传了 sid 就按 sid 拉，
 *   不传 sid 就用设备级接口（fallback）。
 *
 * ## HTTP 认证（走 relay HTTP 代理）
 *   Authorization: Bearer <deviceToken>  — hermix-server 验证设备身份
 *   X-Hermix-Signature: …              — 请求完整性签名
 *   Authorization: Bearer <relayToken>  — relay 代理层认证
 *
 * ## 两键约束（security.rs 强制）
 *   `Origin::PairedDevice` 对 `Capability::ApproveAlways` 是无条件 Denied，
 *   服务端会下发 `can_persist: false`，手机端必须把这个状态显示出来——
 *   永久放行只能在桌面端操作，手机端只处理「允许一次 / 拒绝」。
 *
 *   **can_persist 只在 WS approval.request 帧里**，REST 响应不携带它。
 *   这里的 `can_persist` 字段暂时写死 `false`——桌面端若在 WS 里重传
 *   approval.request 帧，手机端在收到时更新本地状态。
 */
import { request } from './http'
import { relayBaseUrl } from './clock'
import { signatureHeader } from './signature'
import { deviceToken, relayToken } from './pairing'

/** 一条待审批记录。 */
export interface ApprovalItem {
  /** 服务端生成的请求 id。 */
  request_id: string
  /** 正在申请权限的工具名。 */
  tool: string
  /** 工具调用的关键参数（截取前 200 字）。 */
  detail: string
  /** 剩余秒数（由服务端下发）。 */
  remain_secs: number
  /** 是否已过期（remain_secs 已到 0）。 */
  expired: boolean
  /** 这次审批是否能「永久放行」；REST 不返回该字段，暂写死 false。 */
  can_persist: false
  /** 该请求关联的会话 sid。 */
  sid: string
}

/** GET /v1/sessions/{sid}/approvals/pending 的响应形状。 */
interface PendingApprovalsResponse {
  approvals: Array<{
    request_id: string
    tool: string
    arguments: unknown
    expires_at: string | null
  }>
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

/** GET /v1/sessions/{sid}/approvals/pending — 按 sid 拉待审批列表。
 *
 * @param sid 会话 id（来自 tasks 列表里的 TaskItem.sid）
 */
export async function getApprovalsForSession(
  sid: string,
): Promise<ApprovalItem[]> {
  const base = apiBase()
  if (!base) return []
  const path = `/v1/sessions/${encodeURIComponent(sid)}/approvals/pending`
  const headers = await authHeaders('GET', path)
  const res = await request({
    url: `${base}${path}`,
    method: 'GET',
    header: headers,
    timeout: 10000,
  })
  if (!res.reachable) return []
  if (res.statusCode === 404) return [] // 该会话无审批是正常现象
  if (res.statusCode !== 200) return []
  const data = res.data as PendingApprovalsResponse
  return (data.approvals ?? []).map((a) => {
    // 服务端给的是绝对过期时间；本地换算剩余秒数，缺省（null）才退回 600。
    const remain = a.expires_at
      ? Math.max(0, Math.floor((Date.parse(a.expires_at) - Date.now()) / 1000))
      : 600
    return {
      request_id: a.request_id,
      tool: a.tool,
      // arguments 是完整 JSON，截取前 200 字符显示
      detail: JSON.stringify(a.arguments).slice(0, 200),
      remain_secs: remain,
      expired: remain <= 0,
      can_persist: false as const,
      sid,
    }
  })
}

/** 按 sid 数组批量拉取（tasks 页先用已知 sid 轮询）。 */
export async function getAllApprovals(sids: string[]): Promise<ApprovalItem[]> {
  const results = await Promise.all(sids.map((sid) => getApprovalsForSession(sid)))
  return results.flat()
}

export type ApprovalDecision = 'approved' | 'denied'

/** 提交决定后的结果。 */
export interface ApprovalResult {
  ok: boolean
  reason?: string
}

/**
 * POST /v1/sessions/{sid}/approval — 提交审批决定。
 * @param sid       会话 id
 * @param requestId 请求 id
 * @param decision  approved | denied
 */
export async function submitDecision(
  sid: string,
  requestId: string,
  decision: ApprovalDecision,
): Promise<ApprovalResult> {
  const base = apiBase()
  if (!base) return { ok: false, reason: '未配对' }
  const path = `/v1/sessions/${encodeURIComponent(sid)}/approval`
  const headers = await authHeaders('POST', path)
  headers['Content-Type'] = 'application/json'
  const res = await request({
    url: `${base}${path}`,
    method: 'POST',
    header: headers,
    data: { request_id: requestId, decision },
    timeout: 10000,
  })
  if (!res.reachable) return { ok: false, reason: '网络不可达' }
  if (res.statusCode === 200 || res.statusCode === 202 || res.statusCode === 204) {
    return { ok: true }
  }
  return { ok: false, reason: `服务端返回 ${res.statusCode}` }
}
