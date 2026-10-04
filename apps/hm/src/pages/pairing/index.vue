<script setup lang="ts">
/**
 * 屏 1 · 配对
 *
 * 顺序是刻意的：**先核对 relay 身份，再输配对码**。信任建立必须先于输入
 * 凭据——反过来做的话，用户已经在输码了才被告知要核对证书指纹。
 *
 * 三个安全相关的东西在这一屏：
 * 1. relay host + 证书指纹的显式确认（TOFU 单独用是不够的：首次信任对象
 *    由用户手输决定，输错或被换址就等于交出设备 token）；
 * 2. 设备时钟偏差（签名含时间戳，偏了会全 401，而现象是"连不上"）；
 * 3. 凭据落盘位置（见 credentials.ts 的诚实说明）。
 */
import { ref, computed } from 'vue'
import { onLoad } from '@dcloudio/uni-app'
import { checkClockSkew, SKEW_WARN_SECS, setRelayBaseUrl, relayBaseUrl } from '../../core/clock'
import { verifyConformance } from '../../core/signature'
import { completePairing, fingerprintOf } from '../../core/pairing'

const relayUrl = ref(relayBaseUrl() ?? 'https://relay.example.com')
const code = ref<string[]>(['', '', '', '', '', ''])
const fingerprint = ref('')
const fingerprintOk = ref(false)
const skewSecs = ref<number | null>(null)
const busy = ref(false)
const error = ref('')

const canSubmit = computed(
  () => code.value.every((c) => c !== '') && fingerprintOk.value && !busy.value,
)

onLoad(async (q) => {
  if (q?.code) {
    // 深链 hermix://pair?code=… 预填，但**不跳过指纹确认**
    const raw = decodeURIComponent(q.code).replace(/\D/g, '').slice(0, 6)
    if (raw.length === 6) code.value = raw.split('')
  }
  await probe()
})

async function probe() {
  error.value = ''
  fingerprint.value = ''
  fingerprintOk.value = false
  try {
    setRelayBaseUrl(relayUrl.value)
    // 顺带量时钟：两个探针一起发，省一次往返
    const [fp, skew] = await Promise.all([fingerprintOf(relayUrl.value), checkClockSkew(relayUrl.value)])
    if (!fp) {
      error.value = '连不上这个 relay。检查域名解析与证书。'
      return
    }
    fingerprint.value = fp
    skewSecs.value = skew
  } catch (e) {
    error.value = String(e)
  }
}

function onCodeInput(i: number, e: { detail: { value: string } }) {
  const digits = e.detail.value.replace(/\D/g, '')
  const next = [...code.value]
  // 一次粘贴 6 位时铺满，省得一个个点
  if (digits.length > 1) {
    digits.slice(0, 6).split('').forEach((d, k) => {
      if (i + k < 6) next[i + k] = d
    })
  } else {
    next[i] = digits
  }
  code.value = next
}

async function submit() {
  busy.value = true
  error.value = ''
  try {
    // 开发期自检：签名算法与 Rust 侧固定向量是否一致。
    // 不一致就别发请求——发出去必然 401，而现象是"配对失败"，难查。
    if (!(await verifyConformance())) {
      error.value = '签名自检未通过（与 hermix 服务端不一致），已中止。'
      return
    }
    await completePairing({ relayUrl: relayUrl.value, code: code.value.join('') })
    uni.redirectTo({ url: '/pages/chat/index' })
  } catch (e) {
    error.value = String(e)
  } finally {
    busy.value = false
  }
}
</script>

<template>
  <view class="screen">
    <view class="logo">
      <text class="bolt">⚡</text>
    </view>

    <text class="hero">连接你的助手</text>
    <text class="sub">先核对中转身份，再输码</text>

    <view class="card">
      <view class="fp-row">
        <text class="fp-host">{{ relayUrl.replace(/^https?:\/\//, '') }}</text>
        <text v-if="fingerprint" class="fp-ok">✓</text>
      </view>
      <text v-if="fingerprint" class="fp-hex">SHA-256 {{ fingerprint.slice(0, 8) }}…{{ fingerprint.slice(-4) }}</text>
      <text v-else class="fp-hex dim">正在读取证书指纹…</text>
    </view>

    <view class="code-row">
      <input
        v-for="(c, i) in code"
        :key="i"
        class="code-box"
        type="number"
        maxlength="6"
        :value="c"
        @input="onCodeInput(i, $event)"
      />
    </view>

    <view v-if="skewSecs !== null && Math.abs(skewSecs) > SKEW_WARN_SECS" class="notice warn">
      <text>时钟{{ skewSecs > 0 ? '快' : '慢' }} {{ Math.abs(skewSecs) }} 秒。签名含时间戳，超 ±300 秒会全部被拒。</text>
    </view>

    <view v-if="error" class="notice err">{{ error }}</view>

    <button class="btn-pri" :disabled="!canSubmit" @tap="submit">
      {{ busy ? '配对中…' : '扫码' }}
    </button>

    <view class="relay-row" @tap="probe">
      <text class="dim">relay 地址</text>
      <text class="dim">{{ relayUrl.replace(/^https?:\/\//, '') }}</text>
    </view>
  </view>
</template>

<style lang="scss" scoped>
.screen {
  min-height: 100vh;
  background: $paper;
  display: flex;
  flex-direction: column;
  align-items: center;
  padding: 120rpx 60rpx 60rpx;
  box-sizing: border-box;
}
.logo {
  width: 96rpx;
  height: 96rpx;
  border-radius: 28rpx;
  background: $accent;
  display: flex;
  align-items: center;
  justify-content: center;
  margin-bottom: 26rpx;
}
.bolt { font-size: 48rpx; }
.hero { font-size: 34rpx; font-weight: 600; color: $fg; }
.sub { font-size: 22rpx; color: $fg2; margin: 10rpx 0 40rpx; }

.card {
  width: 100%;
  background: $card;
  border: 1rpx solid $line;
  border-radius: $r-card;
  padding: 22rpx 24rpx;
  margin-bottom: 40rpx;
}
.fp-row { display: flex; align-items: center; justify-content: space-between; }
.fp-host { font-size: 24rpx; color: $fg2; }
.fp-ok { color: $ok; font-weight: 700; }
.fp-hex {
  display: block;
  margin-top: 10rpx;
  font-size: 19rpx;
  color: $fg4;
  font-family: monospace;
  word-break: break-all;
}
.dim { color: $fg4; font-size: 21rpx; }

.code-row { display: flex; gap: 12rpx; margin-bottom: 32rpx; }
.code-box {
  width: 68rpx;
  height: 84rpx;
  background: $card;
  border: 1rpx solid $edge;
  border-radius: 18rpx;
  color: $fg;
  font-size: 34rpx;
  font-weight: 600;
  text-align: center;
}

.notice {
  width: 100%;
  border-radius: 20rpx;
  padding: 18rpx 20rpx;
  font-size: 21rpx;
  line-height: 1.5;
  margin-bottom: 24rpx;
  box-sizing: border-box;
}
.notice.warn { background: $warn-bg; color: $warn; border: 1rpx solid rgba(211, 163, 76, 0.25); }
.notice.err { background: $err-bg; color: $err; border: 1rpx solid rgba(224, 107, 82, 0.25); }

.btn-pri {
  width: 100%;
  height: 88rpx;
  line-height: 88rpx;
  border-radius: 44rpx;
  background: $accent;
  color: $on-accent;
  font-size: 26rpx;
  font-weight: 600;
  border: none;
}
.btn-pri[disabled] { opacity: 0.4; }

.relay-row {
  margin-top: 32rpx;
  display: flex;
  gap: 12rpx;
  align-items: center;
}
</style>
