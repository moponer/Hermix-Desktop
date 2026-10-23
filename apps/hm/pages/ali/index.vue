<script setup lang="ts">
/**
 * 阿黎 · 首页（列表即首页 + 任务卡优先）：
 * 打开手机第一眼 = 她在干嘛 / 我能不能开口 / 找回刚才那段对话。
 * 这一页 = 状态带 + 最近对话 + 常驻新建入口；对话页只负责对话本身。
 */
import { ref } from 'vue'
import { onShow } from '@dcloudio/uni-app'
import { listSessions, deleteSession, type SessionRec } from '../../core/history'
import { setChatSource } from '../../core/chatSource'
import { loadModelConfig } from '../../core/modelStore'
import { isPaired } from '../../core/pairing'
import { getTasks } from '../../core/tasks'
import { knownApprovals } from '../../core/approvalQueue'

const sessions = ref<SessionRec[]>([])
/** 电脑推来的会话列表（桌面会话浏览，slice 1：只读列表）。 */
const desk = ref<any[]>([])
const credReady = ref(false)
const paired = ref(false)
const running = ref(0)
const approvals = ref(0)

onShow(() => {
  sessions.value = listSessions('ali')
  const cfg = loadModelConfig()
  credReady.value = !!cfg && (cfg.providers?.length ?? 0) > 0
  paired.value = isPaired()
  approvals.value = knownApprovals().length
  // 桌面会话列表：电脑连上时经密封帧推来，落在本地存储
  try {
    desk.value = JSON.parse(uni.getStorageSync('hermix.desktopSessions') || '[]') || []
  } catch {
    desk.value = []
  }
  // 她在干嘛：running 的会话数。失败按 0——首页不该因一次网络抖动红掉。
  void getTasks()
    .then((list) => {
      running.value = list.filter((t: any) => t.state === 'running').length
    })
    .catch(() => {
      running.value = 0
    })
})

function fmtTime(iso: string) {
  const d = new Date(iso)
  const days = Math.floor((Date.now() - d.getTime()) / 86400000)
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

/** 桌面会话：slice 1 只读列表；完整内容仍在电脑上看（不假装能在手机上翻全文）。 */
function openDesktop(d: any) {
  uni.showToast({ title: `「${d.title || '会话'}」共 ${d.message_count || 0} 条 · 到电脑上查看全文`, icon: 'none' })
}

function goApproval() {
  uni.navigateTo({ url: '/pages/approval/index' })
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
    <view class="status">
      <view class="st">
        <view class="st-dot" :class="paired ? 'on' : 'off'" />
        <text class="st-t">{{ paired ? '电脑在线' : '电脑未连' }}</text>
      </view>
      <view class="st">
        <text class="st-n">{{ running }}</text>
        <text class="st-t">正在跑</text>
      </view>
      <view class="st" @tap="goApproval">
        <text class="st-n" :class="{ alert: approvals > 0 }">{{ approvals }}</text>
        <text class="st-t">待你审批</text>
      </view>
    </view>

    <text class="desc">她在电脑上执行 · 工具 / 工作区 / 派 agent 都在桌面端</text>

    <view v-if="!credReady" class="warn">
      <text class="warn-t">阿黎暂时没有可用的模型凭据</text>
      <text class="warn-s">凭据由电脑推来，且默认关闭。到桌面端「设置 → 手机端 → 模型配置推送」打开一次即可。</text>
    </view>

    <view class="cta" @tap="startNew">
      <text class="cta-t">开始和阿黎说点什么</text>
      <text class="cta-s">新建一条对话</text>
    </view>

    <!-- 电脑上的会话：她在那边跑过什么。slice 1 只读列表（标题/条数/归属），
         全文仍在电脑上看——不假装手机能翻全文。 -->
    <view class="sec-head">
      <text class="sec-t">电脑上的会话</text>
      <text v-if="desk.length" class="sec-n">{{ desk.length }}</text>
    </view>

    <view v-if="desk.length === 0" class="empty">
      <text class="e-t">还没拿到电脑的会话列表</text>
      <text class="e-s">电脑连上后自动推过来</text>
    </view>

    <view v-else class="list">
      <view v-for="d in desk" :key="d.id" class="row" @tap="openDesktop(d)">
        <view class="rbody">
          <text class="r-t">{{ d.title || '会话' }}</text>
          <text class="r-s">{{ d.message_count || 0 }} 条 · {{ d.workspace ? '项目' : '独立' }}</text>
        </view>
        <view class="rdel"><text>电脑</text></view>
      </view>
    </view>

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
.status { display: flex; gap: 14rpx; margin: 8rpx $gutter 0; }
.st { flex: 1; padding: 20rpx 16rpx; background: $card; border-radius: $r-card; text-align: center; }
.st-dot { width: 16rpx; height: 16rpx; border-radius: 50%; margin: 6rpx auto; }
.st-dot.on { background: $ok; }
.st-dot.off { background: $fg4; }
.st-n { display: block; font-size: $fs-title; font-weight: 600; color: $fg; }
.st-n.alert { color: $err; }
.st-t { display: block; margin-top: 6rpx; font-size: $fs-mini; color: $fg2; }
.desc { display: block; margin: 20rpx $gutter 0; font-size: $fs-mini; color: $fg4; }
.warn { margin: 20rpx $gutter 0; padding: 24rpx; background: $warn-bg; border-radius: $r-card; }
.warn-t { display: block; font-size: $fs-sub; font-weight: 500; color: $fg; }
.warn-s { display: block; margin-top: 8rpx; font-size: $fs-mini; line-height: 1.7; color: $fg2; }
.cta { margin: 20rpx $gutter 0; padding: 28rpx; background: $accent; border-radius: $r-card; }
.cta:active { opacity: 0.85; }
.cta-t { display: block; font-size: $fs-body; font-weight: 600; color: $on-accent; }
.cta-s { display: block; margin-top: 6rpx; font-size: $fs-mini; color: $on-accent; opacity: 0.75; }
.sec-head { display: flex; align-items: baseline; gap: 12rpx; margin: 36rpx $gutter 14rpx; }
.sec-t { font-size: $fs-mini; font-weight: 500; color: $fg2; }
.sec-n { font-size: $fs-mini; color: $fg4; }
.empty { display: flex; flex-direction: column; align-items: center; gap: 14rpx; padding: 80rpx $gutter; }
.e-t { font-size: $fs-sub; color: $fg2; }
.e-s { font-size: $fs-mini; color: $fg4; text-align: center; }
.list { padding: 0 $gutter 24rpx; }
.row { display: flex; align-items: center; gap: 16rpx; padding: 26rpx 24rpx; background: $card; border-radius: $r-card; margin-bottom: 14rpx; }
.row:active { opacity: 0.75; }
.rbody { flex: 1; min-width: 0; }
.r-t { display: block; font-size: $fs-sub; color: $fg; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.r-s { display: block; margin-top: 6rpx; font-size: $fs-mini; color: $fg4; }
.rdel { flex: none; padding: 8rpx 14rpx; border-radius: $r-pill; background: $raised; font-size: $fs-mini; color: $fg2; }
</style>
