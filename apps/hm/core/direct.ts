/**
 * 配对 —— 扫桌面端的码，**经 relay 中转**完成绑定。
 *
 * ## 2026-10-10 起这是唯一路径
 *
 * 拍板：实现复杂度不转嫁给用户。旧「局域网直连 + 证书指纹比对」退役——
 * 它要求手机和电脑同网、要求桌面端暴露本地 TLS 端口、要求用户肉眼比对
 * 64 位指纹，三样都是把实现侧的复杂度摊到用户头上。现在：
 *
 *   桌面出码（c=码 & t=手机relay凭据 & k=PC公钥，凭据当场向 relay 签发）
 *   → 手机扫码 → 凭据连 relay → **密封**配对帧（relay 拒收明文 route 帧）
 *   → PC 判码 → **密封**回 device_token（relay 读不到）→ 绑定完成。
 *
 * 信任锚 = 桌面屏幕上的二维码本身：手机凭据由 PC 当场签发、只走
 * 「屏幕 → 相机」这条信道，能持它连上 relay 并路由到这台 PC 的，
 * 就是扫码的那台手机。配对码的 TTL、一次性、按凭据分桶的错误预算
 * 全部保留（PC 侧 `pairing.rs::handle_pairing_frame`）。
 *
 * ## 帧 AAD 与 PC 侧逐字节对齐
 *
 * Rust 的 `Envelope::aad()` 是 `format!("v={};tag={:?};sid={};nid={}", …)`，
 * `{:?}` 打印枚举变体名（`Route`）。这里手工拼同样形态的串——错一个字节
 * 表现是 AES-GCM 认证失败，极难排查，所以单独提成函数并有测试钉住。
 */
import {
  b64,
  b64decode,
  importPublicKey,
  openBody,
  sealBody,
  type Identity,
} from './crypto'
import {
  encodeEnvelope,
  parseEnvelope,
  PROTO_VERSION,
  type Envelope,
} from './envelope'
import { relayWsUrl } from './relayClient'

/** 配对码位数。服务端 `mint_code` 固定 6 位。 */
const CODE_LEN = 6

/** 配对专用会话。与 Rust 侧 `pairing::PAIRING_SESSION_ID` 必须同串。 */
export const PAIRING_SESSION_ID = 'im:pairing'

/** 配对帧的 AAD。nid 恒为空串：配对阶段双方都还不知道对方身份。 */
export function pairingFrameHeader(sid: string = PAIRING_SESSION_ID): Uint8Array {
  return new TextEncoder().encode(`v=${PROTO_VERSION};tag=Route;sid=${sid};nid=`)
}

export interface PairingTarget {
  /** relay 根地址，如 `https://relay.example.com` */
  relayBase: string
  code: string
  /** 桌面端当场签发的手机 relay 凭据 `hrel_*`（只存在于二维码里） */
  phoneToken: string
  /** 桌面端 E2EE 静态公钥（base64），配对请求封给它 */
  pcPublicKey: string
}

export interface PairOverRelayResult {
  deviceId: string
  deviceToken: string
}

/**
 * 解析桌面端二维码内容。
 *
 * 解析失败返回 `null`，由调用方给出「扫到的不是 hermix 配对码」的提示。
 * 旧格式（带 `fp=` 的局域网直连码）不再接受——桌面端已不再生成。
 */
export function parsePairingUrl(raw: string): PairingTarget | null {
  let u: URL
  try {
    u = new URL(raw.trim())
  } catch {
    return null
  }
  if (u.protocol !== 'https:' && u.protocol !== 'wss:') return null
  if (!/\/pair\/?$/.test(u.pathname)) return null

  const code = (u.searchParams.get('c') ?? '').replace(/\D/g, '')
  if (code.length !== CODE_LEN) return null

  const t = (u.searchParams.get('t') ?? '').trim()
  if (!t.startsWith('hrel_') || t.length > 128) return null

  const k = (u.searchParams.get('k') ?? '').trim()
  if (!/^[A-Za-z0-9+/]+={0,2}$/.test(k)) return null

  return { relayBase: u.origin, code, phoneToken: t, pcPublicKey: k }
}

/**
 * 经 relay 完成配对。失败 throw，文案按阶段分档——
 * 连不上 relay、码被拒、回信超时是三个排查方向完全不同的问题。
 *
 * 全程 30s 上限：配对是一次交互内的往返，卡更久就是真的有问题。
 */
export function pairOverRelay(opts: {
  target: PairingTarget
  deviceName: string
  identity: Identity
}): Promise<PairOverRelayResult> {
  const { target, deviceName, identity } = opts
  return new Promise((resolve, reject) => {
    let task: UniApp.SocketTask | null = null
    const request_id = Array.from(globalThis.crypto.getRandomValues(new Uint8Array(8)))
      .map((b) => b.toString(16).padStart(2, '0'))
      .join('')

    let settled = false
    const cleanup = () => {
      if (settled) return
      settled = true
      clearTimeout(timer)
      task?.close({})
      task = null
    }
    const fail = (msg: string) => {
      cleanup()
      reject(new Error(msg))
    }
    const timer = setTimeout(() => {
      fail(
        '配对没有回音（30s 超时）。确认电脑端 hermix 正在运行且已连上中转；' +
          '若反复如此，中转服务版本可能过旧（2026-10-11 前的 relay 会丢帧），需与桌面端一起升级。',
      )
    }, 30000)

    const boot = async () => {
      // ① 构造密封配对请求。收件人是二维码里那把 PC 公钥——relay 转发时
      //    看得到帧、解不开内容。
      const pcPublic = await importPublicKey(b64decode(target.pcPublicKey))
      const frameId = globalThis.crypto.getRandomValues(new Uint8Array(16))
      const { sealed, ephemeralPublic } = await sealBody(
        new TextEncoder().encode(
          JSON.stringify({
            type: 'pairing.request',
            request_id,
            pairing_code: target.code,
            device_name: deviceName,
            // 本机 E2EE 公钥：PC 用它密封回信（里面装着 device_token）。
            pk: b64(identity.publicKey),
          }),
        ),
        {
          receiverPublic: pcPublic,
          frameId,
          header: pairingFrameHeader(),
          direction: 'd2n',
        },
      )
      const requestPayload = JSON.stringify({
        ct: b64(sealed.ciphertext),
        nonce: b64(sealed.nonce),
        ep: b64(ephemeralPublic),
        eid: b64(frameId),
      })

      // ② 连 relay。手机凭据只认 HTTP 握手头（浏览器 WebSocket 无法带头，
      //    所以必须 uni.connectSocket——与 relayClient 同一条理由）。
      task = uni.connectSocket({
        url: relayWsUrl(target.relayBase, 'phone'),
        header: { Authorization: `Bearer ${target.phoneToken}` },
        complete: () => undefined,
      })

      task.onOpen(() => {
        // 登记帧：身份由 relay 从凭据侧自行判定，这里不带哈希也不带 node_id。
        task?.send({
          data: encodeEnvelope({
            protoVersion: PROTO_VERSION,
            tag: 'hello',
            tokenHash: '',
            nodeId: '',
          }),
        })
        task?.send({
          data: encodeEnvelope({
            protoVersion: PROTO_VERSION,
            tag: 'route',
            sessionId: PAIRING_SESSION_ID,
            payload: requestPayload,
          } satisfies Envelope),
        })
      })

      task.onMessage((ev) => {
        const raw = typeof ev.data === 'string' ? ev.data : ''
        const parsed = parseEnvelope(raw)
        if (!parsed.ok) {
          if (parsed.versionMismatch) fail('中转协议版本与电脑端不一致，请两端一起升级')
          return
        }
        const env = parsed.envelope
        if (env.tag !== 'route' || env.sessionId !== PAIRING_SESSION_ID) return
        if (!env.payload) return
        void (async () => {
          try {
            // nodeToPhones 是广播：多台手机同时在配对时，只认自己 request_id 的回信。
            const sf = JSON.parse(env.payload!) as {
              ct: string
              nonce: string
              ep: string
              eid: string
            }
            const plain = await openBody(
              { nonce: b64decode(sf.nonce), ciphertext: b64decode(sf.ct) },
              {
                frameId: b64decode(sf.eid),
                header: pairingFrameHeader(),
                direction: 'n2d',
                receiverPrivate: identity.privateKey,
                ephemeralPublic: await importPublicKey(b64decode(sf.ep)),
              },
            )
            const result = JSON.parse(new TextDecoder().decode(plain)) as {
              request_id?: string
              ok?: boolean
              device_id?: string
              device_token?: string
              error?: string
            }
            if (result.request_id !== request_id) return // 别人的回信，继续等
            if (!result.ok || !result.device_id || !result.device_token) {
              fail(result.error ?? '配对被电脑端拒绝，请在电脑上重新生成配对码。')
              return
            }
            cleanup()
            resolve({ deviceId: result.device_id, deviceToken: result.device_token })
          } catch (e) {
            fail(`配对回信解不开（${String(e)}）。请重新扫码再试。`)
          }
        })()
      })

      task.onError((e) => {
        const msg = String((e as { errMsg?: string })?.errMsg ?? '')
        if (/401|unauthorized/i.test(msg)) {
          fail('中转拒绝了这条连接（401）：配对码可能已被用过，请重新扫码。')
          return
        }
        fail('连不上中转。确认手机能上网，且电脑端已配置同一个中转地址。')
      })

      task.onClose((res?: { code?: number }) => {
        // 优雅路径（已 resolve 并主动 close）不算错误；走到这里说明被中途挂断。
        // relay 的关闭码要说人话（apps/relay/src/server.js）：4009 = 这台电脑
        // 已经连着另一台手机——照通用文案提示「重新扫码」会把人引去死路，
        // 真正该做的是先在电脑端撤销原设备。code 缺失时回落通用文案。
        if (settled) return
        if (res?.code === 4009) {
          fail('这台电脑已连接另一台手机。请先在电脑端「设备管理」撤销原设备，再重新扫码。')
          return
        }
        fail('与中转的连接被断开，请重新扫码。')
      })
    }

    boot().catch((e) => fail(`配对请求构造失败：${String(e)}`))
  })
}

/** 桌面上给设备起的名字。取不到系统信息就退回一个不空的常量。 */
export function defaultDeviceName(): string {
  try {
    const info = uni.getSystemInfoSync()
    const brand = (info.deviceBrand || '').trim()
    const model = (info.deviceModel || '').trim()
    const name = [brand, model].filter(Boolean).join(' ')
    if (name) return `Hermix M · ${name}`
  } catch {
    /* 取不到就用下面的兜底，不该因为一个装饰字段失败 */
  }
  return 'Hermix M'
}
