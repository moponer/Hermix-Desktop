<script setup lang="ts">
/**
 * 配对屏 —— 扫桌面端的码，直连那台电脑。
 *
 * ## 这条链路是通的，不是新造的
 *
 * 桌面端 `设置 → 设备 → 手机端` 已经发着一个二维码，内容就是
 * `https://{lan_ip}:8443/pair?c={code}&fp={fingerprint}`；同一个 TLS 监听器上
 * 已经挂着 `POST /api/pairing/complete`。这一屏做的事，和那个二维码指向的
 * `/pair` 落地页里那段 JS **完全一样**（契约见 `core/direct.ts` 文件头）。
 *
 * ## 为什么不再走 relay
 *
 * 原先这屏打的是 `POST {relay}/api/pairing/complete`，而 relay 里没有这条路由，
 * 必然 404。补 relay 中转要跨三端（relay + Rust + hm），因为配对码的判定权在
 * PC 的 `PairingSessions::consume`，而 PC 只出站连 relay。先把能用的接通。
 *
 * ## 顺序还是刻意的：先核对指纹，再提交
 *
 * 二维码里带着桌面端本机 TLS 证书的 SHA-256 指纹。提交时原样回传，服务端与
 * 自己的指纹比对，**不符直接 403 拒发 token**（`pairing.rs:810-830`）。这一步
 * 不是形式：同网段 MITM 出示自签证书 + 克隆 `/pair` 页就能把配对码换成
 * device_token。本屏把指纹显示出来，是为了让用户能拿它和桌面上那一行肉眼比对，
 * 而不是把一个自己声明的值当真。
 */
import { ref } from 'vue'
import { parsePairingUrl, completeDirect, shortFingerprint, defaultDeviceName, type DirectTarget } from '../../core/direct'
import { storePairedDevice, hasSubtleCrypto, relayToken } from '../../core/pairing'

/** 扫到并解析成功、但用户还没确认提交的码。 */
const target = ref<DirectTarget | null>(null)
const busy = ref(false)
const error = ref('')

const cryptoOk = hasSubtleCrypto()

function onScan() {
  error.value = ''
  uni.scanCode({
    success: (res) => {
      const t = parsePairingUrl(res.result)
      if (!t) {
        error.value = '这不是 hermix 的配对码。请在电脑上「设置 → 设备 → 手机端」重新生成。'
        return
      }
      target.value = t
    },
    fail: (e) => {
      const msg = String((e as { errMsg?: string })?.errMsg ?? '')
      if (/cancel/i.test(msg)) return // 用户自己取消不是错误
      error.value = '扫码失败，请确认已允许相机权限。也可以用手机自带相机扫码，在浏览器里完成配对。'
    },
  })
}

async function submit() {
  const t = target.value
  if (!t) return
  busy.value = true
  error.value = ''
  try {
    // 把 relay 凭据一起报上去（报的是 sha256 哈希，不是明文）。电脑端靠它
    // 才能在「撤销设备」时让 relay 断开这台手机——否则撤销只断一半。
    const r = await completeDirect({
      target: t,
      deviceName: defaultDeviceName(),
      relayToken: relayToken(),
    })
    await storePairedDevice({ deviceId: r.deviceId, deviceToken: r.deviceToken })
    uni.reLaunch({ url: '/pages/chat/index' })
  } catch (e) {
    error.value = e instanceof Error ? e.message : String(e)
  } finally {
    busy.value = false
  }
}

function reset() {
  target.value = null
  error.value = ''
}
</script>

<template>
  <view class="screen">
    <image class="logo" src="/static/mascot.png" mode="aspectFit" />
    <text class="hero">连接你的电脑</text>
    <text class="sub">扫电脑「设置 → 设备」里的配对码</text>

    <!-- 第一步：扫 -->
    <view v-if="!target" class="card">
      <view class="scan" @tap="onScan">
        <view class="scan-box">
          <view class="corner tl" />
          <view class="corner tr" />
          <view class="corner bl" />
          <view class="corner br" />
        </view>
        <text class="scan-t">扫码连接</text>
      </view>
      <text class="card-hint">
        手机和电脑要在同一个 Wi-Fi。扫不了就用手机自带相机扫同一个码，会在浏览器里打开配对页。
      </text>
    </view>

    <!-- 第二步：核对身份，再提交 -->
    <template v-else>
      <view class="card">
        <view class="fp-row">
          <text class="fp-label">这台电脑</text>
          <text class="fp-change" @tap="reset">换一个</text>
        </view>
        <text class="fp-host">{{ target.baseUrl.replace(/^https?:\/\//, '') }}</text>
        <text class="fp-hex">证书指纹 {{ shortFingerprint(target.fingerprint) }}</text>
        <text class="card-hint">
          请与电脑「设置 → 设备 → 手机端」那一行显示的指纹核对一致后再连接。
        </text>
      </view>

      <view v-if="!cryptoOk" class="notice err">
        <text>本机运行时没有 WebCrypto，配对后的端到端加密无法工作。请换用官方安装包。</text>
      </view>

      <view v-if="error" class="notice err"><text>{{ error }}</text></view>

      <view
        class="btn-pri"
        :class="{ off: busy || !cryptoOk }"
        @tap="busy || !cryptoOk ? undefined : submit()"
      >
        <text>{{ busy ? '连接中…' : '确认并连接' }}</text>
      </view>

      <view class="btn-sec" @tap="reset"><text>返回</text></view>
    </template>

    <text class="foot">配对码 60 分钟内有效、只能用一次</text>
  </view>
</template>

<style lang="scss" scoped>
.screen {
  min-height: 100vh;
  background: $paper;
  display: flex;
  flex-direction: column;
  align-items: center;
  padding: calc(var(--status-bar-height, 24px) + 80rpx) 48rpx
    calc(env(safe-area-inset-bottom) + 48rpx);
  box-sizing: border-box;
}
.logo {
  width: 112rpx;
  height: 112rpx;
  margin-bottom: 22rpx;
}
.hero {
  font-size: 34rpx;
  font-weight: 600;
  color: $fg;
}
.sub {
  font-size: 22rpx;
  color: $fg2;
  margin: 10rpx 0 34rpx;
}

.card {
  width: 100%;
  background: $card;
  border-radius: $r-card;
  padding: 26rpx 24rpx;
}
.card-hint {
  display: block;
  margin-top: 18rpx;
  font-size: 20rpx;
  color: $fg4;
  line-height: 1.6;
}

/* 扫码区：四角标记画在底色块上，不描边。 */
.scan {
  display: flex;
  flex-direction: column;
  align-items: center;
  padding: 24rpx 0 8rpx;
}
.scan-box {
  position: relative;
  width: 190rpx;
  height: 190rpx;
  background: $raised;
  border-radius: 24rpx;
}
.corner {
  position: absolute;
  width: 40rpx;
  height: 40rpx;
  border-radius: 8rpx;
  background: $accent;
}
.tl {
  top: -6rpx;
  left: -6rpx;
}
.tr {
  top: -6rpx;
  right: -6rpx;
}
.bl {
  bottom: -6rpx;
  left: -6rpx;
}
.br {
  bottom: -6rpx;
  right: -6rpx;
}
.scan-t {
  margin-top: 24rpx;
  font-size: 25rpx;
  font-weight: 600;
  color: $accent;
}

.fp-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
}
.fp-label {
  font-size: 20rpx;
  color: $fg4;
}
.fp-change {
  font-size: 21rpx;
  color: $accent;
}
.fp-host {
  display: block;
  margin-top: 8rpx;
  font-size: 25rpx;
  color: $fg;
}
.fp-hex {
  display: block;
  margin-top: 10rpx;
  font-size: 19rpx;
  color: $fg3;
  font-family: monospace;
  word-break: break-all;
}

.notice {
  width: 100%;
  margin-top: 20rpx;
  border-radius: 18rpx;
  padding: 18rpx 20rpx;
  font-size: 21rpx;
  line-height: 1.5;
  box-sizing: border-box;
  color: $fg2;
  background: $card;
}
.notice.err {
  background: $err-bg;
  color: $err;
}

.btn-pri {
  width: 100%;
  margin-top: 26rpx;
  height: 88rpx;
  line-height: 88rpx;
  text-align: center;
  border-radius: 44rpx;
  background: $accent;
  color: $on-accent;
  font-size: 26rpx;
  font-weight: 600;
}
.btn-pri.off {
  opacity: 0.4;
}
.btn-sec {
  width: 100%;
  margin-top: 14rpx;
  height: 76rpx;
  line-height: 76rpx;
  text-align: center;
  border-radius: 38rpx;
  background: $card;
  color: $fg2;
  font-size: 23rpx;
}

.foot {
  margin-top: 28rpx;
  font-size: 20rpx;
  color: $fg4;
  text-align: center;
}
</style>
