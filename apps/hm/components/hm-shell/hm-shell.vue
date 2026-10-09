<script setup lang="ts">
/**
 * 页面外壳：状态栏留白 + 顶栏 + 导航抽屉 + 内容插槽。
 *
 * ## 为什么要抽这一层
 *
 * 改造前五个页面各自复制一份 `.screen` / `.appbar` 的样式，其中顶栏
 * 留白是硬编码的 `padding: 100rpx`——那是个「大概是状态栏高度」的数字，
 * 在挖孔屏上会切标题，在普通屏上又会多一截。抽出来之后安全区只在
 * `pages.json` 与这一个组件里生效，全站一致。
 *
 * ## 抽屉的入口集合
 *
 * 只有**当前真的有功能**的页面。参考了 WorkBuddy 侧边栏的结构
 * （顶部身份区 → 主导航 → 底部设置），但砍掉了它的「云端工作 / 连接电脑」
 * 双模式切换：我们的 agent 引擎只在 PC 上，造一个假的模式开关比不做更糟。
 * 同理没有「任务历史」——消息还没落盘，列不出真实历史。
 */
import { ref, computed } from 'vue'
import { relayBaseUrl } from '../../core/clock'
import { deviceId, isPaired } from '../../core/pairing'

const props = withDefaults(
  defineProps<{
    title?: string
    /** 显示返回箭头而不是菜单按钮 */
    back?: boolean
    /** 顶栏右侧插槽 */
    showMenu?: boolean
    /** 透传给内部 scroll-view 的定位锚点（对话页用来滚到底） */
    scrollIntoView?: string
  }>(),
  { title: '', back: false, showMenu: true, scrollIntoView: '' },
)

const open = ref(false)

const paired = ref(isPaired())

const NAV = [
  { key: 'chat', label: '对话', hint: '下任务、追问', path: '/pages/chat/index' },
  { key: 'tasks', label: '任务', hint: '运行中与排队', path: '/pages/tasks/index' },
  { key: 'approval', label: '审批', hint: '待你点头的操作', path: '/pages/approval/index' },
  { key: 'call', label: '通话', hint: '边走边听', path: '/pages/call/index' },
]

/** 未配对时把「连接电脑」顶到第一位 —— 此刻它就是唯一有用的那件事。 */
const items = computed(() =>
  paired.value
    ? NAV
    : [{ key: 'connect', label: '连接电脑', hint: '还没连上', path: '/pages/pairing/index' }, ...NAV],
)

const relay = computed(() => {
  const u = relayBaseUrl()
  return u ? u.replace(/^https?:\/\//, '') : '未配置 relay'
})
const me = computed(() => (paired.value ? deviceId() : '未连接'))

function go(path: string) {
  open.value = false
  // 抽屉换的是「顶层频道」，用 reLaunch 不让栈越压越深；但「连接电脑」
  // 不是顶层页（它要从主界面 push 上去，返回时该回到对话），故走 navigateTo。
  if (path === '/pages/pairing/index') uni.navigateTo({ url: path })
  else uni.reLaunch({ url: path })
}

function back() {
  const pages = getCurrentPages()
  if (pages.length > 1) uni.navigateBack()
  else uni.reLaunch({ url: '/pages/chat/index' })
}
</script>

<template>
  <view class="shell">
    <view class="bar">
      <view v-if="props.back" class="icon-btn" @tap="back"><text>‹</text></view>
      <view v-else-if="props.showMenu" class="icon-btn" @tap="open = true"><text>≡</text></view>
      <text class="title">{{ props.title }}</text>
      <view class="slot-end"><slot name="end" /></view>
    </view>

    <scroll-view class="content" scroll-y :scroll-into-view="props.scrollIntoView" :scroll-with-animation="true">
      <slot />
    </scroll-view>

    <!-- 底部固定区（对话页的输入栏）。放在滚动区**外面**，否则键盘一弹
         整页高度被压缩、输入栏跟着被顶走。 -->
    <slot name="bottom" />

    <!-- 抽屉：v-if 卸载而非靠 opacity 藏起来。留一层「看不见但还在」的节点
         会挡住下面的点击，是移动端最常见的一类「点不动了」。 -->
    <view v-if="open" class="mask" @tap="open = false">
      <view class="drawer" @tap.stop>
        <view class="d-head">
          <image class="d-logo" src="/static/mascot.png" mode="aspectFit" />
          <view class="d-id">
            <text class="d-name">Hermix M</text>
            <text class="d-me">{{ me }}</text>
          </view>
        </view>

        <view class="d-nav">
          <view v-for="n in items" :key="n.key" class="d-item" @tap="go(n.path)">
            <view class="d-txt">
              <text class="d-label">{{ n.label }}</text>
              <text class="d-hint">{{ n.hint }}</text>
            </view>
            <text class="d-arrow">›</text>
          </view>
        </view>

        <view class="d-foot">
          <view class="d-item" @tap="go('/pages/settings/index')">
            <view class="d-txt">
              <text class="d-label">设置</text>
              <text class="d-hint">relay · 诊断 · 重新配对</text>
            </view>
            <text class="d-arrow">›</text>
          </view>
          <text class="d-relay">{{ relay }}</text>
        </view>
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
  font-size: 30rpx;
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
  width: 56rpx;
  height: 56rpx;
  display: flex;
  align-items: center;
  justify-content: center;
  border-radius: $r-pill;
  background: $raised;
  color: $fg;
  font-size: 34rpx;
}

.content {
  flex: 1;
  min-height: 0;
}

/* ── 抽屉 ───────────────────────────────────────────────────── */
.mask {
  position: fixed;
  inset: 0;
  z-index: 50;
  background: rgba(0, 0, 0, 0.55);
  display: flex;
}
.drawer {
  width: 78%;
  max-width: 560rpx;
  height: 100%;
  background: $raised;
  box-shadow: $shadow-drawer;
  display: flex;
  flex-direction: column;
  padding: calc(var(--status-bar-height, 24px) + 32rpx) 0
    calc(env(safe-area-inset-bottom) + 24rpx);
}

.d-head {
  display: flex;
  align-items: center;
  gap: 20rpx;
  padding: 0 32rpx 32rpx;
}
.d-logo {
  width: 76rpx;
  height: 76rpx;
  border-radius: $r-pill;
}
.d-id {
  flex: 1;
  min-width: 0;
}
.d-name {
  display: block;
  font-size: 28rpx;
  font-weight: 600;
  color: $fg;
}
.d-me {
  display: block;
  font-size: 20rpx;
  color: $fg4;
  margin-top: 4rpx;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.d-nav {
  flex: 1;
  padding: 0 20rpx;
}
.d-item {
  display: flex;
  align-items: center;
  gap: 16rpx;
  padding: 22rpx 20rpx;
  border-radius: $r-card;
  background: $card;
  margin-bottom: 14rpx;
}
.d-txt {
  flex: 1;
  min-width: 0;
}
.d-label {
  display: block;
  font-size: 26rpx;
  color: $fg;
}
.d-hint {
  display: block;
  font-size: 20rpx;
  color: $fg4;
  margin-top: 2rpx;
}
.d-arrow {
  color: $fg4;
  font-size: 30rpx;
}

.d-foot {
  padding: 0 20rpx;
}
.d-relay {
  display: block;
  padding: 12rpx 20rpx 0;
  font-size: 19rpx;
  color: $fg4;
  font-family: monospace;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
</style>
