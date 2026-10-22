/**
 * 模型清单（手机端视角）。
 *
 * ## 清单从哪来
 *
 * 手机端**不存**自己的模型清单：唯一真相源在桌面（config.yaml 的
 * `model_selector` / `llm_providers`）。桌面在手机连上时把清单作为 WS
 * 密封帧推过来（`model.list` 事件，见 `core/frames.ts`），手机据此渲染
 * 选择器——不留第二份必然漂移的副本。
 *
 * ## 为什么不再走 HTTP
 *
 * 手机此前调 `GET {relay}/api/model/list`：但中继是**只路由的字节管道**，
 * 根本没有通往节点的 HTTP 通道，那条请求必然 404 —— 于是模型选择器永远是
 * 空的，桌面换了模型手机也看不见。
 *
 * 同理「切换默认模型」也不再走 `PUT /api/model/list`：那会把整份 `enabled`
 * 清单覆盖成手机这份，而手机的清单本来就不全 —— 桌面付费厂商的模型会被
 * 挤出可见范围（这是记在案的真实事故）。现在改为经 WS 发 `model_select`，
 * **只改默认值**，`enabled` 保持桌面自己那份。
 */

export interface ModelEntry {
  provider_id: string
  model_id: string
  /** 供应商展示名（自定义 provider 用 display_name，否则就是 id）。 */
  provider_name?: string
  /** 上下文窗口（能查到才有）。 */
  context_window?: number
  /** 是否支持 reasoning。 */
  supports_reasoning?: boolean
}

/** 清单条目的唯一 id：`provider:model`。 */
export function modelId(entry: ModelEntry): string {
  return `${entry.provider_id}:${entry.model_id}`
}

/** 把 `provider:model` 拆开；格式不对时返回 null。 */
export function splitModelId(id: string): { provider: string; model: string } | null {
  const i = id.indexOf(':')
  if (i <= 0 || i === id.length - 1) return null
  return { provider: id.slice(0, i), model: id.slice(i + 1) }
}

/**
 * 电脑推来的 provider 凭据——手机在**电脑离线**时直连云模型要用。
 *
 * `api_key` 缺失 = 该端点无需鉴权（不发 Authorization 头），与桌面侧
 * `auth_header` 的约定一致。
 */
export interface ProviderCredential {
  provider_id: string
  base_url?: string
  api_key?: string
  /** 端点风格。目前只有 `openai`（OpenAI 兼容的 /chat/completions）。 */
  api_style: string
}

/**
 * 电脑推来的**完整**模型配置，手机持久化它。
 *
 * 有了它，电脑关机时手机照样能本地对话——用户全程不用在手机上配模型。
 */
export interface ModelConfig {
  models: ModelEntry[]
  providers: ProviderCredential[]
  defaultModel: string | null
  /** 收到推送的时间（毫秒）。 */
  updatedAt: number
}
