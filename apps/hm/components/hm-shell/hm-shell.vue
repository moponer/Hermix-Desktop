<script setup lang="ts">
/**
 * 页面外壳：状态栏留白 + 顶栏 + 内容插槽 +（顶层页）底部标签栏。
 *
 * ## 2026-10-12 重设计：抽屉 → 底部标签栏 + 设备切换
 *
 * 旧版学网页后台做汉堡菜单 + 侧滑抽屉——那不是移动端的心智模型：
 * 原生 App 的顶层导航长在拇指够得着的**底部**，四个频道一眼可见、
 * 一次点击切换。现在与 Android/iOS 一致：**顶层页（对话/任务/审批/通话）
 * 带标签栏，次级页（设置/配对）顶栏带返回箭头、无标签栏**。
 *
 * ## 顶栏设备胶囊（多桌面）
 *
 * 顶层页的标题位置常驻「当前电脑名 + 状态点 + ▾」：连的是哪台一眼可判，
 * 点它弹底部切换面板——列表一键换机（= 改 activeDevice 指针，原凭据保留），
 * 或跳去配对新电脑。次级页不显示：那是「某一台的详情」语境，切换没有意义。
 */
import { ref, computed } from 'vue'
import { deviceId, isPaired, listDevices, activeDevice, switchDevice, type DeviceRec } from '../../core/pairing'

const props = withDefaults(
  defineProps<{
    title?: string
    /** 显示返回箭头而不是标签栏（次级页） */
    back?: boolean
    /** 透传给内部 scroll-view 的定位锚点（对话页用来滚到底） */
    scrollIntoView?: string
  }>(),
  { title: '', back: false, scrollIntoView: '' },
)

const paired = computed(() => isPaired())

/** 底部标签栏：只放顶层四个频道。设置从对话页进，不占一位。 */
const TABS = [
  { key: 'chat', label: '对话', path: '/pages/chat/index' },
  { key: 'tasks', label: '任务', path: '/pages/tasks/index' },
  { key: 'approval', label: '审批', path: '/pages/approval/index' },
  { key: 'call', label: '通话', path: '/pages/call/index' },
]

/** 当前页（按 title 前缀匹配，通话页标题是「通话中」）在标签栏里高亮；
 * 对话是首页兜底。 */
function tabActive(key: string) {
  const hit = TABS.find((t) => props.title.trim().startsWith(t.label))
  return (hit?.key ?? 'chat') === key
}

// ── 设备切换面板 ────────────────────────────────────────────────
const sheetOpen = ref(false)
const devices = ref<DeviceRec[]>([])
const curId = computed(() => deviceId())

function openSheet() {
  devices.value = listDevices()
  sheetOpen.value = true
}

function pick(id: string) {
  if (id === curId.value) {
    sheetOpen.value = false
    return
  }
  // 切换 = 改指针 + 同步 relay 镜像。reLaunch 让所有顶层页以新设备重读状态，
  // 对话流/任务/审批都锚定新机器，不会残留上一台的帧。
  if (switchDevice(id)) {
    sheetOpen.value = false
    uni.reLaunch({ url: '/pages/chat/index' })
  }
}

function addDevice() {
  sheetOpen.value = false
  uni.navigateTo({ url: '/pages/pairing/index' })
}

function go(path: string) {
  // 顶层频道互切：reLaunch 不让栈越压越深。
  uni.reLaunch({ url: path })
}

function back() {
  const pages = getCurrentPages()
  if (pages.length > 1) uni.navigateBack()
  else uni.reLaunch({ url: '/pages/chat/index' })
}

const devName = computed(() => activeDevice()?.name ?? '')
</script>

<template>
  <view class="shell">
    <view class="bar">
      <view v-if="props.back" class="icon-btn" @tap="back"><text>‹</text></view>
      <!-- 顶层页：标题位换成设备胶囊（多桌面切换入口） -->
      <view v-if="!props.back && paired" class="devpill" @tap="openSheet">
        <view class="dp-dot" />
        <text class="dp-name">{{ devName }}</text>
        <text class="dp-cv">▾</text>
      </view>
      <text v-else class="title">{{ props.title }}</text>
      <view class="slot-end"><slot name="end" /></view>
    </view>

    <scroll-view class="content" scroll-y :scroll-into-view="props.scrollIntoView" :scroll-with-animation="true">
      <slot />
    </scroll-view>

    <!-- 底部固定区（对话页的输入栏、通话页的按钮排）。放在滚动区**外面**，
         否则键盘一弹整页高度被压缩、输入栏跟着被顶走。 -->
    <slot name="bottom" />

    <!-- 底部标签栏：仅顶层页（非 back）显示。次级页不显示——原生 App
         推入的详情页同样没有标签栏，返回箭头是唯一出口。 -->
    <view v-if="!props.back" class="tabbar">
      <view
        v-for="t in TABS"
        :key="t.key"
        class="tab"
        :class="{ on: tabActive(t.key) }"
        @tap="go(t.path)"
      >
        <text class="tab-l">{{ t.label }}</text>
      </view>
    </view>

    <!-- 设备切换面板：v-if 卸载而非藏起来，不留挡点击的隐形层。 -->
    <view v-if="sheetOpen" class="mask" @tap="sheetOpen = false">
      <view class="sheet" @tap.stop>
        <view class="grip" />
        <text class="sh-t">选择要对话的电脑</text>
        <view
          v-for="d in devices"
          :key="d.id"
          class="dev"
          :class="{ on: d.id === curId }"
          @tap="pick(d.id)"
        >
          <view class="dev-dot" :class="{ cur: d.id === curId }" />
          <view class="dev-txt">
            <text class="dev-n">{{ d.name }}</text>
            <text class="dev-s">{{ d.id === curId ? '当前连接' : `配对于 ${d.addedAt.slice(0, 10)}` }}</text>
          </view>
          <text v-if="d.id === curId" class="dev-ck">✓</text>
        </view>
        <view class="adddev" @tap="addDevice"><text>＋ 连接另一台电脑</text></view>
        <text class="sh-note">切换后对话从新会话开始，原电脑的任务继续在它那边跑</text>
      </view>
    </view>
  </view>
</template>

<style lang="scss" scoped>
.shell {
  display: flex;
  flex-direction: column;
  height: 100vh;
  background: $paper;
}

/* 顶栏：不用 border-bottom 分隔——底色比页面深一档本身就是分界。 */
.bar {
  flex: none;
  display: flex;
  align-items: center;
  gap: 16rpx;
  background: $base;
  /* --status-bar-height 由 uni-app 在 App 端注入；H5 下为 0，回落到 24px。 */
  padding: calc(var(--status-bar-height, 24px) + 18rpx) 24rpx 18rpx;
}
.title {
  flex: 1;
  font-size: $fs-title;
  font-weight: 600;
  color: $fg;
}
.slot-end {
  flex: none;
  display: flex;
  align-items: center;
  gap: 12rpx;
}
.icon-btn {
  flex: none;
  width: $tap;
  height: $tap;
  margin: -16rpx 0 -16rpx -16rpx;
  display: flex;
  align-items: center;
  justify-content: center;
  border-radius: $r-pill;
  background: $raised;
  color: $fg;
  font-size: 40rpx;
}

/* ── 设备胶囊（顶栏） ────────────────────────────────────────── */
.devpill {
  flex: 1;
  min-width: 0;
  display: flex;
  align-items: center;
  gap: 12rpx;
  padding: 14rpx 22rpx;
  border-radius: $r-pill;
  background: $raised;
  align-self: center;
}
.dp-dot {
  flex: none;
  width: 14rpx;
  height: 14rpx;
  border-radius: 50%;
  background: $ok;
}
.dp-name {
  font-size: $fs-sub;
  font-weight: 600;
  color: $fg;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.dp-cv {
  flex: none;
  font-size: $fs-mini;
  color: $fg3;
}

.content {
  flex: 1;
  min-height: 0;
}

/* ── 底部标签栏 ─────────────────────────────────────────────── */
/* 与滚动区之间靠底色差分界，不画线。四项等宽、拇指热区 88rpx。 */
.tabbar {
  flex: none;
  display: flex;
  background: $base;
  padding: 10rpx 12rpx calc(env(safe-area-inset-bottom) + 10rpx);
}
.tab {
  flex: 1;
  height: $tap;
  display: flex;
  align-items: center;
  justify-content: center;
  border-radius: $r-card;
}
.tab-l {
  font-size: $fs-sub;
  color: $fg4;
  font-weight: 500;
}
.tab.on .tab-l {
  color: $accent;
  font-weight: 600;
}
.tab:active {
  background: $card;
}

/* ── 设备切换面板（bottom sheet） ───────────────────────────── */
.mask {
  position: fixed;
  inset: 0;
  z-index: 50;
  background: rgba(0, 0, 0, 0.55);
  display: flex;
  align-items: flex-end;
}
.sheet {
  width: 100%;
  background: $raised;
  border-radius: 36rpx 36rpx 0 0;
  padding: 16rpx 28rpx calc(env(safe-area-inset-bottom) + 28rpx);
}
.grip {
  width: 64rpx;
  height: 8rpx;
  border-radius: 4rpx;
  background: $fg4;
  margin: 0 auto 20rpx;
}
.sh-t {
  display: block;
  font-size: $fs-mini;
  color: $fg2;
  margin-bottom: 16rpx;
}
.dev {
  display: flex;
  align-items: center;
  gap: 20rpx;
  padding: 24rpx;
  border-radius: $r-card;
  background: $card;
  margin-bottom: 14rpx;
}
.dev.on {
  box-shadow: inset 0 0 0 2rpx $accent;
}
.dev-dot {
  flex: none;
  width: 18rpx;
  height: 18rpx;
  border-radius: 50%;
  background: $fg4;
}
.dev-dot.cur {
  background: $ok;
}
.dev-txt {
  flex: 1;
  min-width: 0;
}
.dev-n {
  display: block;
  font-size: $fs-body;
  font-weight: 500;
  color: $fg;
}
.dev.on .dev-n {
  color: $accent;
}
.dev-s {
  display: block;
  font-size: $fs-mini;
  color: $fg4;
  margin-top: 4rpx;
}
.dev-ck {
  flex: none;
  color: $accent;
  font-size: $fs-body;
}
.adddev {
  height: $tap;
  display: flex;
  align-items: center;
  justify-content: center;
  border-radius: $r-card;
  border: 2rpx dashed #3a3d42;
  color: $accent;
  font-size: $fs-sub;
  margin-top: 4rpx;
}
.adddev:active {
  opacity: 0.7;
}
.sh-note {
  display: block;
  margin-top: 18rpx;
  font-size: $fs-mini;
  color: $fg4;
  text-align: center;
  line-height: 1.6;
}
</style>
