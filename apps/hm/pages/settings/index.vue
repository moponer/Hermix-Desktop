<script setup lang="ts">
/**
 * 设置页。
 *
 * **只放真的有东西的条目。** 参考了 WorkBuddy 设置页的结构，但砍掉了它的
 * 记忆库、主题/语言四档、分享日志、意见反馈、账号注销——那些要么我们没有，
 * 要么（主题）只发布暗色一档，做个开关是假的。
 *
 * 四项都是当场可验的：relay 地址、连接诊断、配对状态、重新配对。
 */
import { ref } from 'vue'
import { relayBaseUrl, setRelayBaseUrl, checkClockSkew, type SkewProbe } from '../../core/clock'
import {
  fingerprintOf,
  deviceId,
  isPaired,
  clearCredentials,
  setRelayToken,
  hasRelayToken,
  type RelayIdentity,
} from '../../core/pairing'

const url = ref(relayBaseUrl() ?? '')
/**
 * relay 凭据（`hrel_*`）。
 *
 * 只在内存里编辑、点保存才落盘：明文 token 不该在界面上常驻显示，
 * 也没必要为它做「显示/隐藏」——它是一次性从桌面端复制过来的，
 * 不是需要反复查看的东西。
 */
const token = ref('')
const saved = ref('')
const id = ref<RelayIdentity | null>(null)
const skew = ref<SkewProbe | null>(null)
const probing = ref(false)
const note = ref('')

/** App 端能读到真实版本号；H5 没有 version，退回「—」。 */
const version = (() => {
  try {
    return uni.getAppBaseInfo?.().version || '—'
  } catch {
    return '—'
  }
})()

async function diagnose() {
  const target = url.value.trim()
  if (!/^https?:\/\/.+/.test(target)) {
    note.value = '地址要带 http:// 或 https://'
    return
  }
  probing.value = true
  note.value = ''
  try {
    setRelayBaseUrl(target)
    const [ident, sk] = await Promise.all([fingerprintOf(target), checkClockSkew(target)])
    id.value = ident
    skew.value = sk
    if (ident.state === 'verified') note.value = 'relay 可达，证书指纹已拿到。'
    else if (ident.state === 'invisible')
      note.value = 'relay 可达，但读不到证书指纹 —— 服务端多半没配 HERMIX_RELAY_CERT_SHA256（浏览器下也可能是跨域限制）。'
    else note.value = ident.timeout ? 'relay 超时。' : 'relay 不可达。'
  } catch (e) {
    note.value = String(e)
    id.value = null
  } finally {
    probing.value = false
  }
}

function saveToken() {
  const t = token.value.trim()
  if (!t) {
    saved.value = '没填内容，凭据未改动'
    return
  }
  if (!t.startsWith('hrel_')) {
    // 在填错时就拦下：连一次被 relay 401 再回来猜，比现在多一次往返。
    saved.value = '这串不像 relay 凭据（应以 hrel_ 开头）'
    return
  }
  setRelayToken(t)
  token.value = ''
  saved.value = '已保存'
}

function goPairing() {
  uni.navigateTo({ url: '/pages/pairing/index' })
}

function resetPairing() {
  uni.showModal({
    title: '重新配对',
    content: '清空本机的设备凭据，需要重新扫一次桌面端的配对码。',
    confirmText: '清空',
    success: (res) => {
      if (!res.confirm) return
      clearCredentials()
      uni.reLaunch({ url: '/pages/pairing/index' })
    },
  })
}
</script>

<template>
  <hm-shell title="设置" back>
    <!-- 连接 -->
    <text class="sec">连接</text>
    <view class="row tap" @tap="goPairing">
      <text class="k">扫码配对</text>
      <text class="v-t ok">扫电脑上的配对码 →</text>
    </view>
    <view class="note">
      推荐入口：扫码后 relay 地址与凭据自动带上，无需手填。桌面端在「设置 → 设备管理 → 手机端」生成配对码。
    </view>
    <view class="row">
      <text class="k">配对方式</text>
      <text class="v-t">扫电脑上的码（经中转）</text>
    </view>
    <view class="row">
      <text class="k">relay 凭据</text>
      <text class="v-t" :class="hasRelayToken() ? 'ok' : 'warn'">
        {{ hasRelayToken() ? '已填写' : '未填写' }}
      </text>
    </view>
    <view v-if="hasRelayToken()" class="note">
      已配对后，跨网络发消息靠它连中转。没有它就只能局域网直连。
    </view>
    <view class="row">
      <input
        v-model="token"
        class="v"
        placeholder="hrel_…（桌面端「手机端」渠道复制）"
        :password="true"
      />
      <view class="btn-s" @tap="saveToken"><text>保存</text></view>
    </view>
    <text v-if="saved" class="note" :class="saved === '已保存' ? 'ok' : 'warn'">{{ saved }}</text>
    <view class="row">
      <text class="k">relay</text>
      <input v-model="url" class="v" placeholder="https://…" />
    </view>
    <view class="acts">
      <view class="btn" @tap="diagnose"><text>{{ probing ? '诊断中…' : '测试连接' }}</text></view>
    </view>

    <view v-if="note" class="diag">
      <text class="diag-t">{{ note }}</text>
      <text v-if="id?.state === 'verified'" class="diag-s">
        SHA-256 {{ id.fingerprint.slice(0, 8) }}…{{ id.fingerprint.slice(-4) }}
      </text>
      <text v-if="skew" class="diag-s">
        {{
          skew.skew === null
            ? '时钟偏差：测不到'
            : `时钟偏差：${skew.skew > 0 ? '快' : '慢'} ${Math.abs(skew.skew)} 秒`
        }}
      </text>
      <text v-if="note" class="diag-s">改动后记得点「测试连接」生效</text>
    </view>

    <!-- 本机 -->
    <text class="sec">本机</text>
    <view class="row">
      <text class="k">配对状态</text>
      <text class="v-t">{{ isPaired() ? '已配对' : '未配对' }}</text>
    </view>
    <view class="row">
      <text class="k">设备</text>
      <text class="v-t mono">{{ deviceId() || '—' }}</text>
    </view>
    <view class="row">
      <text class="k">版本</text>
      <text class="v-t">{{ version }}</text>
    </view>

    <!-- TODO(在线更新): 版本检查入口放这里。manifest 里带 android 段，
         App 启动时拉同一份清单，version 高于自身就在本页挂红点。
         见 docs/notes/implemented/feature/2026-10-09-online-update.md 的手机端一节。 -->

    <view class="acts">
      <view class="btn danger" @tap="resetPairing"><text>清空凭据并重新配对</text></view>
    </view>
  </hm-shell>
</template>

<style lang="scss" scoped>
.sec {
  display: block;
  margin: 34rpx 26rpx 14rpx;
  font-size: 20rpx;
  color: $fg4;
  letter-spacing: 1rpx;
}
.note {
  display: block;
  margin: 0 26rpx 16rpx;
  font-size: 20rpx;
  color: $fg4;
  line-height: 1.6;
}/* 每行自成一张卡。原先靠 `:first-of-type` / `.sec + .row` 这些兄弟选择器去
   拼「首行上圆角、末行下圆角」，加一行就错位；每行独立圆角反而更省事。 */
.row {
  display: flex;
  align-items: center;
  gap: 20rpx;
  padding: 24rpx 26rpx;
  background: $card;
  border-radius: $r-card;
  margin: 0 0 8rpx;
}
.tap:active {
  opacity: 0.7;
}
.k {
  flex: none;
  width: 160rpx;
  font-size: 23rpx;
  color: $fg2;
}
.v {
  flex: 1;
  min-width: 0;
  font-size: 23rpx;
  color: $fg;
}
.v-t {
  flex: 1;
  font-size: 23rpx;
  color: $fg;
  word-break: break-all;
}
.ok {
  color: $ok;
}
.warn {
  color: $warn;
}
.btn-s {
  flex: none;
  padding: 10rpx 22rpx;
  border-radius: $r-pill;
  background: $accent;
  color: $on-accent;
  font-size: 21rpx;
}
.mono {
  font-family: monospace;
  font-size: 20rpx;
  color: $fg3;
}

.acts {
  padding: 22rpx 26rpx 0;
}
.btn {
  height: 76rpx;
  line-height: 76rpx;
  text-align: center;
  border-radius: 38rpx;
  background: $card;
  color: $fg;
  font-size: 23rpx;
}
.btn.danger {
  background: $err-bg;
  color: $err;
}

.diag {
  margin: 16rpx 26rpx 0;
  padding: 20rpx 22rpx;
  border-radius: $r-card;
  background: $card;
}
.diag-t {
  display: block;
  font-size: 22rpx;
  color: $fg;
}
.diag-s {
  display: block;
  margin-top: 8rpx;
  font-size: 19rpx;
  color: $fg4;
  font-family: monospace;
}
</style>
