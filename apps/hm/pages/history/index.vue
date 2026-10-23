<script setup lang="ts">
/**
 * 历史会话页（桌面来源与阿黎来源共用，按来源可筛）。
 *
 * 入口：对话页顶栏的「☰」。
 * 行为：点击一行 → 切到该会话**自己的来源**并载入；右侧删除。
 *
 * 归属必须显式：同一条列表里既有"阿黎在手机上答的"，也有"电脑在桌面端跑的"，
 * 不标来源，用户根本分不清某句话是谁说的、在哪能继续。
 */
import { computed, ref } from 'vue'
import { onShow } from '@dcloudio/uni-app'
import {
  listSessions,
  deleteSession,
  type ChatSource,
  type SessionRec,
} from '../../core/history'
import { setChatSource, SOURCE_META } from '../../core/chatSource'

type Filter = 'all' | ChatSource
const filter = ref<Filter>('all')
const all = ref<SessionRec[]>([])

const sessions = computed(() =>
  filter.value === 'all' ? all.value : all.value.filter((s) => s.source === filter.value),
)

function reload() {
  all.value = listSessions()
}

onShow(reload)

const FILTERS: { id: Filter; label: string }[] = [
  { id: 'all', label: '全部' },
  { id: 'ali', label: SOURCE_META.ali.name },
  { id: 'desktop', label: SOURCE_META.desktop.name },
]

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
  // 先切来源再载入会话：否则阿黎的对话会落在桌面模式里，界面归属就错了。
  setChatSource(s.source)
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
      reload()
    },
  })
}
</script>

<template>
  <hm-shell title="历史会话" back>
    <!-- 来源筛选：三个片，选中态用主色实底 + 字重 -->
    <view class="filters">
      <view
        v-for="f in FILTERS"
        :key="f.id"
        class="f-pill"
        :class="{ on: filter === f.id }"
        @tap="filter = f.id"
      >
        <text>{{ f.label }}</text>
      </view>
    </view>

    <!-- 空态 -->
    <view v-if="sessions.length === 0" class="empty">
      <text class="e-t">还没有历史会话</text>
      <text class="e-s">
        {{ filter === 'all' ? '在对话页发一条消息就会自动保存' : '这个来源下还没有对话' }}
      </text>
    </view>

    <!-- 列表 -->
    <view v-else class="list">
      <view v-for="s in sessions" :key="s.id" class="row" @tap="openSession(s)">
        <view class="rbody">
          <view class="r-top">
            <text class="r-tag" :class="s.source">{{ SOURCE_META[s.source].name }}</text>
            <text class="r-t">{{ s.title }}</text>
          </view>
          <text class="r-s">{{ fmtTime(s.updatedAt) }} · {{ s.msgCount }} 条</text>
        </view>
        <view class="rdel" @tap.stop="confirmDelete(s)">
          <text>删除</text>
        </view>
      </view>
    </view>
  </hm-shell>
</template>

<style lang="scss" scoped>
.filters {
  display: flex;
  gap: 10rpx;
  padding: 8rpx $gutter 18rpx;
}
.f-pill {
  padding: 12rpx 26rpx;
  border-radius: $r-pill;
  background: $card;
  font-size: $fs-mini;
  color: $fg2;
}
.f-pill.on {
  background: $accent;
  color: $on-accent;
  font-weight: 600;
}

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
.r-top {
  display: flex;
  align-items: center;
  gap: 10rpx;
  min-width: 0;
}
/* 来源标签：阿黎用主色、桌面用中性——同一列表里一眼分出两条通路。 */
.r-tag {
  flex: none;
  padding: 2rpx 12rpx;
  border-radius: $r-pill;
  font-size: $fs-mini;
  background: $raised;
  color: $fg2;
}
.r-tag.ali {
  background: $accent;
  color: $on-accent;
}
.r-t {
  flex: 1;
  min-width: 0;
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
