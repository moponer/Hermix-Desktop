/**
 * 模型配置的**本地持久化**。
 *
 * 电脑连上时把配置推过来（`model.list` 事件），手机存一份；电脑关机后这份
 * 就是唯一可用的配置——所以它必须落盘，不能只活在内存里。
 *
 * ## 安全边界
 *
 * 存的是**真凭据**（含 API Key）。这台手机已经是配对设备、能驱动电脑的
 * 工具执行，多一把 LLM 密钥不改变信任等级，但**泄漏面变大**——所以：
 * - 只写本机存储，不上传、不写日志、不进任何导出；
 * - 撤销配对时（设置里移除这台电脑）应一并清掉（见 `clearModelConfig`）。
 */
import type { ModelConfig } from './model'

const KEY = 'hmxd_model_config'

/** 保存一份来自电脑的配置（覆盖旧的——电脑改了，手机跟着变）。 */
export function saveModelConfig(cfg: Omit<ModelConfig, 'updatedAt'>): void {
  try {
    uni.setStorageSync(KEY, { ...cfg, updatedAt: Date.now() })
  } catch {
    // 存储满了/被禁：本地直连就没了凭据，但不影响遥控电脑那条路。
  }
}

/** 读回上次从电脑收到的配置；没有则 null。 */
export function loadModelConfig(): ModelConfig | null {
  try {
    const raw = uni.getStorageSync(KEY)
    if (!raw || typeof raw !== 'object') return null
    const cfg = raw as ModelConfig
    if (!Array.isArray(cfg.models)) return null
    return cfg
  } catch {
    return null
  }
}

/** 清掉本地配置（撤销配对 / 用户主动清理时用）。 */
export function clearModelConfig(): void {
  try {
    uni.removeStorageSync(KEY)
  } catch {
    // 忽略：清不掉也不能因此阻断撤销流程。
  }
}

/**
 * 改**本机直聊**用的默认模型（只改这份本地快照的 defaultModel）。
 *
 * 不经电脑：电脑下次推送会把整份快照覆盖回来——那本来就是唯一真相源，
 * 本地改的只服务「电脑不在线时手机自己挑模型」这一件事。
 */
export function setLocalDefaultModel(model: string): void {
  const cfg = loadModelConfig()
  if (!cfg || cfg.defaultModel === model) return
  try {
    uni.setStorageSync(KEY, { ...cfg, defaultModel: model })
  } catch {
    // 存不进去 = 下次还是旧默认，不影响当轮对话。
  }
}
