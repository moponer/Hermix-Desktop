<script setup lang="ts">
/**
 * 配对屏 —— 扫桌面端的码，经 relay 中转绑定。
 *
 * ## 这条链路是唯一的（2026-10-10 拍板）
 *
 * 桌面端生成配对码时**当场**向 relay 签发一条手机凭据并印进二维码
 * （`c=码 & t=凭据 & k=PC公钥`）。本屏扫到后：用凭据连 relay → 密封配对帧
 * → 电脑判码 → 密封回 device_token。全程不要求同网、不比对证书指纹、
 * 不需要电脑开任何入站端口——实现复杂度归实现侧。
 *
 * 旧「局域网直连」流程已退役（`fp=` 参数的码桌面端不再生成）。
 */
import { ref } from 'vue'
import {
  defaultDeviceName,
  pairOverRelay,
  parsePairingUrl,
  type PairingTarget,
} from '../../core/direct'
import { generateIdentity } from '../../core/crypto'
import { setRelayToken, storePairedDevice, listDevices } from '../../core/pairing'
import { setRelayBaseUrl } from '../../core/clock'

/** 扫到并解析成功、但用户还没确认提交的码。 */
const target = ref<PairingTarget | null>(null)
/** 多桌面后「哪台是哪台」靠用户起名：默认按来源猜一个，可改。 */
const devName = ref('')
const busy = ref(false)
const error = ref('')

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
      // 起个默认名：设备 id 还没拿到（配对完成才有），先用时间槽位兜底，
      // 用户在下一步可以改成「家里的电脑 / 公司工作站」这类好认的名字。
      devName.value = `电脑 ${listDevices().length + 1}`
    },
    fail: (e) => {
      const msg = String((e as { errMsg?: string })?.errMsg ?? '')
      if (/cancel/i.test(msg)) return // 用户自己取消不是错误
      error.value = '扫码失败，请确认已允许相机权限。'
    },
  })
}

async function submit() {
  const t = target.value
  if (!t) return
  busy.value = true
  error.value = ''
  try {
    // 配对身份只服务这一次密封往返：私钥不可导出，重启后自然换新，
    // 每次连接都会重发 key.exchange（PC 侧 learn_peer 是 upsert）。
    const identity = await generateIdentity()
    const r = await pairOverRelay({ target: t, deviceName: defaultDeviceName(), identity })
    // relay 地址与凭据随码而来，落盘后聊天页的常驻连接直接用。
    // 多桌面：name/relayBase/relayToken 一并入设备清单，不覆盖已有设备。
    setRelayBaseUrl(t.relayBase)
    setRelayToken(t.phoneToken)
    // PC 公钥(k) 一并落盘：聊天连接用它 seal 给 PC，冷重启后不丢。
    await storePairedDevice({
      deviceId: r.deviceId,
      deviceToken: r.deviceToken,
      pcPublicKey: t.pcPublicKey,
      name: devName.value,
      relayBase: t.relayBase,
      relayToken: t.phoneToken,
    })
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
          <!-- 中央示意图标：给「哪里能点」一个视觉落点，纯 CSS 不引资源 -->
          <view class="scan-glyph"><view class="scan-glyph-line" /></view>
        </view>
        <text class="scan-t">点击中间开始扫码连接</text>
      </view>
      <text class="card-hint">
        手机和电脑不必在同一网络：配对与连接都经中转（relay），手机能上网就行。
      </text>
      <!-- 错误必须在这一步就可见：扫码失败/解析失败时 target 还是空，
           视图停在第一步——提示若只放第二步，用户看到的就是「响一声没反应」。 -->
      <view v-if="error" class="notice err"><text>{{ error }}</text></view>
    </view>

    <!-- 第二步：确认身份 + 起名，再提交 -->
    <template v-else>
      <view class="card">
        <view class="fp-row">
          <text class="fp-label">这台电脑</text>
          <text class="fp-change" @tap="reset">换一个</text>
        </view>
        <!-- 配对码是用户唯一要辨认的东西：大号居中，网页地址不参与辨认、不展示 -->
        <text class="code-caption">配对码</text>
        <text class="fp-code">{{ target.code }}</text>
        <text class="card-hint">
          请对照电脑屏幕上显示的配对码，一致再点连接。
        </text>
        <!-- 多桌面：名字进设备清单，切换面板/设备管理都靠它辨认 -->
        <view class="namer">
          <text class="namer-l">给它起个名字</text>
          <input v-model="devName" class="namer-i" placeholder="比如：家里的电脑" :maxlength="20" />
        </view>
      </view>

      <view v-if="error" class="notice err"><text>{{ error }}</text></view>

      <!-- E2EE 已换 @noble/* 纯 JS 实现（2026-10-12），打进应用包内，
           不再依赖运行时的 WebCrypto——旧「没有 WebCrypto」门禁随之退役。 -->
      <view
        class="btn-pri"
        :class="{ off: busy }"
        @tap="busy ? undefined : submit()"
      >
        <text>{{ busy ? '正在连接…' : '确认并连接' }}</text>
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
  font-size: $fs-title;
  font-weight: 600;
  color: $fg;
}
.sub {
  font-size: $fs-sub;
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
  font-size: $fs-mini;
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
/* 框中央的扫码示意图标：方框 + 中间扫描线，纯 CSS。 */
.scan-glyph {
  position: absolute;
  top: 50%;
  left: 50%;
  width: 64rpx;
  height: 64rpx;
  margin: -32rpx 0 0 -32rpx;
  border: 6rpx solid $accent;
  border-radius: 14rpx;
  opacity: 0.9;
}
.scan-glyph-line {
  position: absolute;
  top: 50%;
  left: 10rpx;
  right: 10rpx;
  height: 6rpx;
  margin-top: -3rpx;
  border-radius: 3rpx;
  background: $accent;
}
.scan-t {
  margin-top: 24rpx;
  font-size: $fs-body;
  font-weight: 600;
  color: $accent;
}

.fp-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
}
.fp-label {
  font-size: $fs-mini;
  color: $fg4;
}
.fp-change {
  font-size: $fs-mini;
  color: $accent;
}
.code-caption {
  display: block;
  margin-top: 24rpx;
  font-size: $fs-mini;
  color: $fg4;
  text-align: center;
}
/* 配对码是确认环节的主角：大号 + 字距拉开，扫一眼就能和电脑屏对上 */
.fp-code {
  display: block;
  margin-top: 8rpx;
  font-size: 76rpx;
  font-weight: 700;
  line-height: 1.2;
  color: $fg;
  font-family: monospace;
  letter-spacing: 14rpx;
  text-indent: 14rpx; /* 抵消末字符字距，保证真正居中 */
  text-align: center;
}

/* 命名输入：多桌面后靠名字辨认设备，扫码确认卡里就地起名。 */
.namer {
  margin-top: 20rpx;
  padding-top: 20rpx;
}
.namer-l {
  display: block;
  font-size: $fs-mini;
  color: $fg4;
  margin-bottom: 10rpx;
}
.namer-i {
  height: 80rpx;
  border-radius: 16rpx;
  background: $base;
  padding: 0 22rpx;
  font-size: $fs-body;
  color: $fg;
}

.notice {
  width: 100%;
  margin-top: 20rpx;
  border-radius: 18rpx;
  padding: 18rpx 20rpx;
  font-size: $fs-mini;
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
  height: $tap;
  display: flex;
  align-items: center;
  justify-content: center;
  border-radius: 44rpx;
  background: $accent;
  color: $on-accent;
  font-size: $fs-body;
  font-weight: 600;
}
.btn-pri.off {
  opacity: 0.4;
}
.btn-sec {
  width: 100%;
  margin-top: 14rpx;
  height: $tap;
  display: flex;
  align-items: center;
  justify-content: center;
  border-radius: 44rpx;
  background: $card;
  color: $fg2;
  font-size: $fs-sub;
}

.foot {
  margin-top: 28rpx;
  font-size: $fs-mini;
  color: $fg4;
  text-align: center;
}
</style>
