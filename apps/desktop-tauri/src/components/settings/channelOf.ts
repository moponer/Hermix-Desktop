/**
 * 渠道判据（跨渠道一张 `paired_devices` 表的**呈现**切分）。
 *
 * ## 为什么单独立一个文件
 *
 * `DevicesTab` 已经 import 了 `ImChannelsCard`（它渲染上半部分的渠道配置），
 * 反过来再让 `ImChannelsCard` 从 `DevicesTab` 拿这个函数就是**循环依赖**。
 * 判据本身又是两边都要用的（一边列已连设备，一边决定要不要出码），
 * 所以放到两个都碰不到的叶子上。
 *
 * ## 判据用名字形态而非加字段
 *
 * `^(qq|wecom):` 前缀是 IM 账号（由 channels 侧写入，见 `channels/session.rs`
 * 里 `qq:<openid>` 的写法），其余是走扫码配对进来的手机（名字由手机自报）。
 * 加字段就得迁移存量行，而判据本身没有歧义——不值得。
 */
export type ChannelId = 'qq' | 'wecom' | 'mobile'

/** 这一行设备属于哪个渠道。 */
export function channelOf(deviceName: string): ChannelId {
  const m = /^(qq|wecom):/.exec(deviceName)
  if (m) return m[1] as ChannelId
  return 'mobile'
}

/** `GET /api/pairing/devices` 的一行（`pairing.rs::PairedDevice`）。 */
export interface PairedDevice {
  device_id: string
  device_name: string
  scopes: string
  issued_at: number
  last_seen_at: number | null
  revoked_at: number | null
}

/**
 * `DELETE /api/pairing/devices/{id}` 的返回。
 *
 * **撤销是两件事**，必须分开报：本地设备令牌失效（= 发不出帧）永远成功；
 * relay 侧断开连接则可能失败（relay 不可达、没配地址、绑都没绑过）。
 * 把两者合成一句「已撤销」就是在骗用户——那台手机可能还在收广播帧。
 */
export interface RevokeOutcome {
  device_id: string
  local_revoked: boolean
  /** 这台设备配对时是否报过 relay 凭据哈希。没报过就没有 relay 侧可撤。 */
  relay_bound: boolean
  relay_revoked: boolean
  relay_error: string | null
}