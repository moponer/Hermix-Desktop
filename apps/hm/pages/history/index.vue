<script setup lang="ts">
/**
 * 历史会话页。
 *
 * 入口：对话页顶栏的「≡」按钮。
 * 行为：点击一行 → 加载该会话并切到对话页；左滑/长按 → 删除。
 */
import { ref, onMounted } from 'vue'
import { listSessions, deleteSession, type SessionRec } from '../../core/history'

const sessions = ref<SessionRec[]>([])

onMounted(() => {
  sessions.value = listSessions()
})

function fmtTime(iso: string) {
  const d = new Date(iso)
  const now = new Date()
  const diff = now.getTime() - d.getTime()
  const days = Math.floor(diff / 86400000)
  if (days === 0) return '今天'
  if (days === 1) return '昨天'
  if (days < 7) return `${days}天前`
  return d.toLocaleDateString('zh-CN', { month: 'short', day: 'numeric' })
}

function openSession(s: SessionRec) {
  // 存目标 session id，chat 页 onLoad 时检测并恢复
  uni.setStorageSync('hermix.restoreSession', s.id)
  uni.switchTab({ url: '/pages/chat/index' })
}

function confirmDelete(s: SessionRec) {
  uni.showModal({
    title: '删除会话',
    content: `删除「${s.title}」？此操作不可恢复。`,
    confirmText: '删除',
    confirmColor: '#e06b52',
    success: (res) => {
      if (!res.confirm) return
      deleteSession(s.id)
      sessions.value = listSessions()
    },
  })
}
</script>

<template>
  <hm-shell title="历史会话" back>
    <!-- 空态 -->
    <view v-if="sessions.length === 0" class="empty">
      <text class="e-t">还没有历史会话</text>
      <text class="e-s">在对话页发一条消息就会自动保存</text>
    </view>

    <!-- 列表 -->
    <view v-else class="list">
      <view
        v-for="s in sessions"
        :key="s.id"
        class="row"
        @tap="openSession(s)"
      >
        <view class="rbody">
          <text class="r-t">{{ s.title }}</text>
          <text class="r-s">{{ fmtTime(s.updatedAt) }} · {{ s.msgCount }}条消息</text>
        </view>
        <view class="rdel" @tap.stop="confirmDelete(s)">
          <text>🗑</text>
        </view>
      </view>
    </view>
  </hm-shell>
</template>

<style lang="scss" scoped>
.empty {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  padding: 120rpx $gutter;
  gap: 16rpx;
}
.e-t {
  font-size: $fs-body;
  color: $fg2;
}
.e-s {
  font-size: $fs-mini;
  color: $fg4;
  text-align: center;
}
.list {
  padding: 0 $gutter;
}
.row {
  display: flex;
  align-items: center;
  gap: 16rpx;
  padding: 28rpx 24rpx;
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
  font-size: $fs-mini;
  color: $fg4;
  margin-top: 6rpx;
}
.rdel {
  flex: none;
  width: 64rpx;
  height: 64rpx;
  display: flex;
  align-items: center;
  justify-content: center;
  border-radius: 32rpx;
  color: $fg4;
  font-size: 30rpx;
}
</style>
