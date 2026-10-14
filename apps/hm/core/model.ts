/**
 * 模型选择 API（手机端遥控器视角）。
 *
 * 桌面端有完整的模型配置 UI（ModelListTab + ModelAddDialog），手机端只做
 * 两件事：列出可用模型、切换默认模型。
 *
 * ## 后端端点
 *   GET  /api/model/list  → ModelListResponse
 *   PUT  /api/model/list  → 写入新的 model_selector
 *
 * ## HTTP 认证（走 relay HTTP 代理）
 *   Authorization: Bearer <deviceToken>   — hermix-server 验证设备身份
 *   X-Hermix-Signature: t=<t>,n=<n>,s=<s> — 请求完整性签名（与 WS 帧同算法）
 *   Authorization: Bearer <relayToken>     — relay 代理层认证
 */
import { request } from './http'
import { relayBaseUrl } from './clock'
import { signatureHeader } from './signature'
import { deviceToken, relayToken } from './pairing'

export interface ModelEntry {
  provider_id: string
  model_id: string
}

export interface ModelListResponse {
  models: ModelEntry[]
  default_model: string | null
}

export interface ModelInfo {
  provider: string
  model: string
  context_window: number | null
  supports_reasoning: boolean
}

function apiBase(): string | null {
  return relayBaseUrl()
}

/**
 * 带完整认证头的 HTTP 请求选项。
 * - Authorization: Bearer <deviceToken>  → hermix-server 验设备
 * - X-Hermix-Signature: …               → 防篡改（与 WS 帧同算法）
 * - uni.request 的 header.Authorization → relay 代理层（hrel_*）
 */
async function authHeaders(method: 'GET' | 'PUT', path: string): Promise<Record<string, string>> {
  const dt = deviceToken()
  const rt = relayToken()
  const sig = await signatureHeader(dt, method, path)
  const out: Record<string, string> = {
    'Authorization': `Bearer ${rt || dt}`,
    'X-Device-Token': dt,
    'X-Hermix-Signature': sig,
  }
  return out
}

/** GET /api/model/list — 取模型清单。失败返回 null。 */
export async function getModelList(): Promise<ModelListResponse | null> {
  const base = apiBase()
  if (!base) return null
  const path = '/api/model/list'
  const headers = await authHeaders('GET', path)
  const res = await request({
    url: `${base}${path}`,
    method: 'GET',
    header: headers,
    timeout: 10000,
  })
  if (!res.reachable || res.statusCode !== 200) return null
  return res.data as ModelListResponse
}

/** GET /api/model/info — 取当前模型信息。失败返回 null。 */
export async function getModelInfo(): Promise<ModelInfo | null> {
  const base = apiBase()
  if (!base) return null
  const path = '/api/model/info'
  const headers = await authHeaders('GET', path)
  const res = await request({
    url: `${base}${path}`,
    method: 'GET',
    header: headers,
    timeout: 10000,
  })
  if (!res.reachable || res.statusCode !== 200) return null
  return res.data as ModelInfo
}

/**
 * PUT /api/model/list — 切换默认模型。
 * @param models 完整清单（必须含 default_model）
 * @param defaultModel "provider:model" 格式
 */
export async function setDefaultModel(
  models: ModelEntry[],
  defaultModel: string,
): Promise<boolean> {
  const base = apiBase()
  if (!base) return false
  const path = '/api/model/list'
  const headers = await authHeaders('PUT', path)
  headers['Content-Type'] = 'application/json'
  const res = await request({
    url: `${base}${path}`,
    method: 'PUT',
    header: headers,
    data: { models, default_model: defaultModel },
    timeout: 10000,
  })
  return res.reachable && res.statusCode === 200
}
