<script setup lang="ts">
/**
 * 设置页。
 *
 * ## 2026-10-12 重设计：做减法
 *
 * 旧版 13 个元素从上铺到下——relay 地址、凭据输入、配对方式、诊断结果、
 * 设备 ID、版本……全是平铺的表单行，信息密度是网页后台的，不是手机设置页的。
 * 现在按「一眼一件事」重排：
 *
 *   1. **连接状态卡**——顶部一张大卡，已连/未连一眼可判，附带设备号；
 *   2. **扫码配对**——唯一的主按钮（推荐路径），说明压成一行；
 *   3. **手动配置**——默认收起：99% 的用户走扫码，永远用不到手填；
 *   4. **版本 + 危险操作**——页脚两行。
 */
import { ref } from 'vue'
import { onShow } from '@dcloudio/uni-app'
import { relayBaseUrl, setRelayBaseUrl, checkClockSkew, type SkewProbe } from '../../core/clock'
import {
  fingerprintOf,
  deviceId,
  isPaired,
  clearCredentials,
  setRelayToken,
  hasRelayToken,
  listDevices,
  activeDevice,
  renameDevice,
  removeDevice,
  switchDevice,
  type DeviceRec,
  type RelayIdentity,
} from '../../core/pairing'

const url = ref(relayBaseUrl() ?? '')
/** 手动配置默认收起——扫码是主路径，手填是兜底。 */
const manualOpen = ref(false)
const token = ref('')
const saved = ref('')
const id = ref<RelayIdentity | null>(null)
const skew = ref<SkewProbe | null>(null)
const probing = ref(false)
const note = ref('')

const paired = ref(isPaired())

/* ── 外观：4 套主题（默认 Notion，与桌面端同源）──────────────────
 * 真正换色的是 App.vue 的 --c-* 变量；这里只负责记住选择并把
 * 全局钩子叫起来（__hmSetTheme 由 App.vue 挂到 globalThis）。 */
const THEMES = [
  { id: 'notion', label: 'Notion' },
  { id: 'notion-dark', label: 'Notion 夜' },
] as const
const theme = ref<string>('notion')
try {
  const stored = uni.getStorageSync('hm-theme')
  if (stored) theme.value = String(stored)
} catch {
  /* 存储不可用：保持默认 */
}
function pickTheme(t: string) {
  theme.value = t
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  ;(globalThis as any).__hmSetTheme?.(t)
}

/** 已配对电脑清单（设备管理区）。onShow 刷新：从配对页回来要看到新机器。 */
const devices = ref<DeviceRec[]>([])
onShow(() => {
  devices.value = listDevices()
  paired.value = isPaired()
})

const curId = ref(deviceId())

/** 点非当前设备 = 切换（与顶栏 sheet 同一语义：改指针 + reLaunch 重读）。 */
function pickDevice(id: string) {
  if (id === deviceId()) return
  if (switchDevice(id)) uni.reLaunch({ url: '/pages/chat/index' })
}

function askRename(d: DeviceRec) {
  uni.showModal({
    title: '重命名',
    editable: true,
    placeholderText: '给这台电脑起个好认的名字',
    content: d.name,
    success: (res) => {
      if (!res.confirm) return
      if (renameDevice(d.id, res.content ?? '')) devices.value = listDevices()
    },
  })
}

function askRemove(d: DeviceRec) {
  uni.showModal({
    title: `移除「${d.name}」`,
    content: '清掉这台电脑的配对凭据。以后要连它得重新扫码；电脑上的数据不受影响。',
    confirmText: '移除',
    confirmColor: '#e06b52',
    success: (res) => {
      if (!res.confirm) return
      removeDevice(d.id)
      devices.value = listDevices()
      paired.value = isPaired()
      curId.value = deviceId()
    },
  })
}

/** 行尾 ⋯：重命名 / 移除（删最后一台 = 回到未配对）。 */
function manageDevice(d: DeviceRec) {
  uni.showActionSheet({
    itemList: ['重命名', '移除这台电脑'],
    success: (res) => {
      if (res.tapIndex === 0) askRename(d)
      else askRemove(d)
    },
  })
}

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
    <!-- 1. 连接状态卡：这一页唯一需要「一眼」的东西。
         注意措辞：配对过 ≠ 此刻在线（在线状态只在对话页的链路条里体现）。 -->
    <view class="conn" :class="{ off: !paired }">
      <view class="conn-dot" />
      <view class="conn-txt">
        <text class="conn-t">{{ paired ? '已配对电脑' : '还没有配对' }}</text>
        <text class="conn-s">{{ paired ? deviceId() : '扫一次码就好，不用手填任何东西' }}</text>
      </view>
    </view>

    <!-- 2. 主按钮：扫码配对（已有设备时话术变成「另一台」） -->
    <view class="hm-pill pri pri" @tap="goPairing"><text>{{ devices.length ? '连接另一台电脑' : '扫码连接电脑' }}</text></view>
    <text class="hint">桌面端「设置 → 设备管理 → 手机端」生成配对码</text>

    <!-- 2.5 设备管理：多桌面。点行=切换（非当前），⋯=重命名/移除。 -->
    <template v-if="devices.length">
      <text class="sec-t">已配对的电脑</text>
      <view
        v-for="d in devices"
        :key="d.id"
        class="drow"
        @tap="pickDevice(d.id)"
      >
        <view class="ddot" :class="{ cur: d.id === deviceId() }" />
        <view class="dtxt">
          <text class="dn" :class="{ on: d.id === deviceId() }">{{ d.name }}</text>
          <text class="ds">{{ d.id === deviceId() ? '当前连接' : d.id }}</text>
        </view>
        <text v-if="d.id === deviceId()" class="dtag">当前</text>
        <view class="dmore" @tap.stop="manageDevice(d)"><text>⋯</text></view>
      </view>
    </template>

    <!-- 3. 手动配置：默认收起 -->
    <view class="manual-head" @tap="manualOpen = !manualOpen">
      <text class="manual-t">手动配置 relay</text>
      <text class="manual-c">{{ manualOpen ? '⌄' : '›' }}</text>
    </view>
    <view v-if="manualOpen" class="manual">
      <view class="frow">
        <text class="fl">relay 地址</text>
        <input v-model="url" class="fv" placeholder="https://…" :adjust-position="false" />
      </view>
      <view class="frow">
        <text class="fl">relay 凭据</text>
        <input
          v-model="token"
          class="fv"
          placeholder="hrel_…"
          :password="true"
          :adjust-position="false"
        />
      </view>
      <view class="fbtns">
        <view class="hm-pill solid fbtn" @tap="saveToken"><text>存凭据</text></view>
        <view class="hm-pill solid fbtn" @tap="diagnose"><text>{{ probing ? '测试中…' : '测试连接' }}</text></view>
      </view>
      <text v-if="saved" class="fnote" :class="saved === '已保存' ? 'ok' : 'warn'">{{ saved }}</text>
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
      </view>
    </view>

    <!-- 3.5 外观：默认单套 Notion，另三套保留给用户回退 -->
    <text class="sec-t">外观</text>
    <view class="theme-row">
      <view
        v-for="t in THEMES"
        :key="t.id"
        class="theme-pill"
        :class="{ on: theme === t.id }"
        @tap="pickTheme(t.id)"
      >
        <text>{{ t.label }}</text>
      </view>
    </view>

    <!-- 4. 页脚：版本 + 危险操作 -->
    <view class="foot">
      <text class="ver">Hermix M {{ version }}</text>
      <view class="hm-pill danger danger" @tap="resetPairing"><text>清空凭据并重新配对</text></view>
    </view>
  </hm-shell>
</template>

<style lang="scss" scoped>
/* 连接状态卡：状态靠色点 + 大字，不靠表单行。 */
.conn {
  display: flex;
  align-items: center;
  gap: 20rpx;
  margin: 28rpx $gutter 0;
  padding: 28rpx;
  background: $card;
  border-radius: $r-card;
}
.conn-dot {
  flex: none;
  width: 20rpx;
  height: 20rpx;
  border-radius: 50%;
  background: $ok;
}
.conn.off .conn-dot {
  background: $warn;
}
.conn-txt {
  flex: 1;
  min-width: 0;
}
.conn-t {
  display: block;
  font-size: $fs-title;
  font-weight: 600;
  color: $fg;
}
.conn-s {
  display: block;
  margin-top: 6rpx;
  font-size: $fs-mini;
  color: $fg3;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.pri {
  margin: 24rpx $gutter 0;
  font-size: $fs-body;
  font-weight: 600;
}

/* 设备管理区 */
.sec-t {
  display: block;
  margin: 32rpx $gutter 12rpx;
  font-size: $fs-mini;
  color: $fg4;
}
/* 外观选择：药丸行（与全站 hm-pill 同形，只是尺寸小一号） */
.theme-row {
  display: flex;
  flex-wrap: wrap;
  gap: 12rpx;
  margin: 0 $gutter 12rpx;
}
.theme-pill {
  padding: 14rpx 26rpx;
  border-radius: $r-pill;
  background: $card;
  font-size: $fs-mini;
  color: $fg2;
}
.theme-pill.on {
  background: $accent;
  color: $on-accent;
}
.drow {
  display: flex;
  align-items: center;
  gap: 20rpx;
  padding: 24rpx 26rpx;
  background: $card;
  border-radius: $r-card;
  margin: 0 $gutter 12rpx;
}
.drow:active {
  opacity: 0.75;
}
.ddot {
  flex: none;
  width: 18rpx;
  height: 18rpx;
  border-radius: 50%;
  background: $fg4;
}
.ddot.cur {
  background: $ok;
}
.dtxt {
  flex: 1;
  min-width: 0;
}
.dn {
  display: block;
  font-size: $fs-body;
  color: $fg;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.dn.on {
  color: $accent;
  font-weight: 600;
}
.ds {
  display: block;
  font-size: $fs-mini;
  color: $fg4;
  margin-top: 4rpx;
}
.dtag {
  flex: none;
  font-size: $fs-mini;
  color: $accent;
  background: rgba(111, 174, 148, 0.14);
  padding: 4rpx 16rpx;
  border-radius: $r-pill;
}
.dmore {
  flex: none;
  width: 56rpx;
  height: 56rpx;
  display: flex;
  align-items: center;
  justify-content: center;
  border-radius: 28rpx;
  color: $fg3;
  font-size: $fs-title;
}
.hint {
  display: block;
  margin: 14rpx $gutter 0;
  font-size: $fs-mini;
  color: $fg4;
  line-height: 1.6;
}

/* 手动配置：折叠头 + 收起后的表单。 */
.manual-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin: 36rpx $gutter 0;
  padding: 24rpx 28rpx;
  background: $card;
  border-radius: $r-card;
}
.manual-head:active {
  opacity: 0.7;
}
.manual-t {
  font-size: $fs-sub;
  color: $fg2;
}
.manual-c {
  font-size: $fs-sub;
  color: $fg4;
}
.manual {
  margin: 12rpx $gutter 0;
  padding: 24rpx 28rpx;
  background: $card;
  border-radius: $r-card;
}
.frow {
  display: flex;
  align-items: center;
  gap: 20rpx;
  min-height: $tap;
}
.fl {
  flex: none;
  width: 170rpx;
  font-size: $fs-sub;
  color: $fg2;
}
.fv {
  flex: 1;
  min-width: 0;
  height: $tap;
  font-size: $fs-sub;
  color: $fg;
}
.fbtns {
  display: flex;
  gap: 16rpx;
  margin-top: 8rpx;
}
.fbtn {
  flex: 1;
  height: 76rpx;
  border-radius: 38rpx;
}
.fnote {
  display: block;
  margin-top: 14rpx;
  font-size: $fs-mini;
}
.fnote.ok {
  color: $ok;
}
.fnote.warn {
  color: $warn;
}
.diag {
  margin-top: 16rpx;
  padding: 18rpx 20rpx;
  border-radius: 14rpx;
  background: $base;
}
.diag-t {
  display: block;
  font-size: $fs-mini;
  color: $fg;
  line-height: 1.5;
}
.diag-s {
  display: block;
  margin-top: 8rpx;
  font-size: $fs-mini;
  color: $fg4;
  font-family: monospace;
}

.foot {
  margin: 44rpx $gutter 40rpx;
}
.ver {
  display: block;
  text-align: center;
  font-size: $fs-mini;
  color: $fg4;
}
.danger {
  margin-top: 20rpx;
}
</style>
