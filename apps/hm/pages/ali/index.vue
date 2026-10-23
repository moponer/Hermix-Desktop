<script setup lang="ts">
/**
 * 阿黎 · 专属页面。
 *
 * 这里是「阿黎」的对话归属地：她的历史会话只在这里列（`source === 'ali'`），
 * 与「桌面」那条通路的历史完全分开。
 *
 * 阿黎走的是**手机本地直连**：用电脑推来的凭据直接找云模型，电脑关机也在。
 * 所以这一页不显示"连电脑"的状态——它本来就不依赖电脑；缺的是凭据
 * （电脑端「模型配置推送」默认关闭），缺凭据时这里要说清怎么开，而不是
 * 让用户对着一个发不出去的空对话发呆。
 */
import { ref } from 'vue'
import { onShow } from '@dcloudio/uni-app'
import { listSessions, deleteSession, type SessionRec } from '../../core/history'
import { setChatSource } from '../../core/chatSource'
import { loadModelConfig } from '../../core/modelStore'

const sessions = ref<SessionRec[]>([])
/** 是否已经拿到电脑推来的凭据（没有就连不了云模型）。 */
const credReady = ref(false)

onShow(() => {
  sessions.value = listSessions('ali')
  const cfg = loadModelConfig()
  credReady.value = !!cfg && (cfg.providers?.length ?? 0) > 0
})

function fmtTime(iso: string) {
  const d = new Date(iso)
  const now = new Date()
  const days = Math.floor((now.getTime() - d.getTime()) / 86400000)
  if (days === 0) return '今天'
  if (days === 1) return '昨天'
  if (days < 7) return `${days} 天前`
  return d.toLocaleDateString('zh-CN', { month: 'short', day: 'numeric' })
}

function openSession(s: SessionRec) {
  setChatSource('ali')
  uni.setStorageSync('hermix.restoreSession', s.id)
  uni.switchTab({ url: '/pages/chat/index' })
}

function startNew() {
  setChatSource('ali')
  uni.removeStorageSync('hermix.restoreSession')
  uni.switchTab({ url: '/pages/chat/index' })
}

function confirmDelete(s: SessionRec) {
  uni.showModal({
    title: '删除对话',
    content: `删除「${s.title}」？此操作不可恢复。`,
    confirmText: '删除',
    confirmColor: '#e06b52',
    success: (res) => {
      if (!res.confirm) return
      deleteSession(s.id)
      sessions.value = listSessions('ali')
    },
  })
}
</script>

<template>
  <hm-shell title="阿黎" back>
    <!-- 身份卡：说清"阿黎是谁、这条对话走哪、什么时候不能用" -->
    <view class="hero">
      <view class="hero-row">
        <view class="avatar"><text>黎</text></view>
        <view class="hero-info">
          <text class="hero-name">阿黎</text>
          <text class="hero-desc">手机直接连云模型 · 电脑关机也能聊</text>
        </view>
      </view>
      <text class="hero-note">
        她与「桌面」是两条独立的对话：这里的话不经过电脑，也不会出现在桌面端。
      </text>
    </view>

    <!-- 没有凭据：明确指路，不留一个发不出去的空对话 -->
    <view v-if="!credReady" class="warn">
      <text class="warn-t">阿黎暂时没有可用的模型凭据</text>
      <text class="warn-s">
        凭据由电脑推来，且默认关闭。到桌面端「设置 → 手机端 → 模型配置推送」打开一次，手机连上后即可用。
      </text>
      <view class="hm-pill ghost warn-btn" @tap="startNew"><text>知道了</text></view>
    </view>

    <!-- 主入口 -->
    <view class="cta" @tap="startNew">
      <text class="cta-t">开始和阿黎说点什么</text>
      <text class="cta-s">新建一条对话</text>
    </view>

    <!-- 她的历史 -->
    <view class="sec-head">
      <text class="sec-t">与阿黎的对话</text>
      <text v-if="sessions.length" class="sec-n">{{ sessions.length }}</text>
    </view>

    <view v-if="sessions.length === 0" class="empty">
      <text class="e-t">还没有和她聊过</text>
      <text class="e-s">上面的入口新建一条；对话都存在这台手机上</text>
    </view>

    <view v-else class="list">
      <view v-for="s in sessions" :key="s.id" class="row" @tap="openSession(s)">
        <view class="rbody">
          <text class="r-t">{{ s.title }}</text>
          <text class="r-s">{{ fmtTime(s.updatedAt) }} · {{ s.msgCount }} 条</text>
        </view>
        <view class="rdel" @tap.stop="confirmDelete(s)"><text>删除</text></view>
      </view>
    </view>
  </hm-shell>
</template>

<style lang="scss" scoped>
/* 身份卡：阿黎是谁。用底色分层，不用描边（全站约定）。 */
.hero {
  margin: 8rpx $gutter 0;
  padding: 28rpx;
  background: $card;
  border-radius: $r-card;
}
.hero-row {
  display: flex;
  align-items: center;
  gap: 20rpx;
}
.avatar {
  flex: none;
  width: 84rpx;
  height: 84rpx;
  border-radius: 26rpx;
  background: $accent;
  display: flex;
  align-items: center;
  justify-content: center;
}
.avatar text {
  font-size: 36rpx;
  font-weight: 600;
  color: $on-accent;
}
.hero-info {
  flex: 1;
  min-width: 0;
}
.hero-name {
  display: block;
  font-size: $fs-title;
  font-weight: 600;
  color: $fg;
}
.hero-desc {
  display: block;
  margin-top: 6rpx;
  font-size: $fs-mini;
  color: $fg2;
}
.hero-note {
  display: block;
  margin-top: 20rpx;
  font-size: $fs-mini;
  line-height: 1.7;
  color: $fg4;
}

/* 缺凭据：可行动的提示，不是一句错误。 */
.warn {
  margin: 20rpx $gutter 0;
  padding: 24rpx;
  background: $warn-bg;
  border-radius: $r-card;
}
.warn-t {
  display: block;
  font-size: $fs-sub;
  font-weight: 500;
  color: $fg;
}
.warn-s {
  display: block;
  margin-top: 8rpx;
  font-size: $fs-mini;
  line-height: 1.7;
  color: $fg2;
}
.warn-btn {
  margin-top: 16rpx;
  align-self: flex-start;
  display: inline-flex;
}

.cta {
  margin: 20rpx $gutter 0;
  padding: 28rpx;
  background: $accent;
  border-radius: $r-card;
}
.cta:active {
  opacity: 0.85;
}
.cta-t {
  display: block;
  font-size: $fs-body;
  font-weight: 600;
  color: $on-accent;
}
.cta-s {
  display: block;
  margin-top: 6rpx;
  font-size: $fs-mini;
  color: $on-accent;
  opacity: 0.75;
}

.sec-head {
  display: flex;
  align-items: baseline;
  gap: 12rpx;
  margin: 36rpx $gutter 14rpx;
}
.sec-t {
  font-size: $fs-mini;
  font-weight: 500;
  color: $fg2;
}
.sec-n {
  font-size: $fs-mini;
  color: $fg4;
}

.empty {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 14rpx;
  padding: 80rpx $gutter;
}
.e-t {
  font-size: $fs-sub;
  color: $fg2;
}
.e-s {
  font-size: $fs-mini;
  color: $fg4;
  text-align: center;
  line-height: 1.7;
}

.list {
  padding: 0 $gutter 24rpx;
}
.row {
  display: flex;
  align-items: center;
  gap: 16rpx;
  padding: 26rpx 24rpx;
  background: $card;
  border-radius: $r-card;
  margin-bottom: 14rpx;
}
.row:active {
  opacity: 0.75;
}
.rbody {
  flex: 1;
  min-width: 0;
}
.r-t {
  display: block;
  font-size: $fs-sub;
  color: $fg;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.r-s {
  display: block;
  margin-top: 6rpx;
  font-size: $fs-mini;
  color: $fg4;
}
.rdel {
  flex: none;
  padding: 8rpx 14rpx;
  border-radius: $r-pill;
  background: $raised;
  font-size: $fs-mini;
  color: $fg2;
}
</style>
