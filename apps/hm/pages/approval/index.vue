<script setup lang="ts">
/**
 * 审批页。
 *
 * **只有两键，没有「永久允许」**——这不是没做完，是技术上不可能：
 * `Origin::PairedDevice` 对 `Capability::ApproveAlways` 是无条件 `Denied`
 * （`origin.rs:65-72`），且 `effective_decision_for_paired` 对矩阵 `Denied`
 * 不做挽救（`security.rs:429-440`）。设备 scopes 里写 `approvealways` 也不生效。
 *
 * 麻烦的是它**不能静默消失**：手机屏幕小，少一个按钮用户未必察觉，但下次审批
 * 照样弹，会以为是自己点错了。所以服务端显式下发 `can_persist:false`，这里
 * 必须把它渲染出来。
 *
 * 改造重点：删掉两条假审批（含一条「已超时」的假状态），改真空态；
 * `window.setInterval` 换成全局 `setInterval`——App / 小程序端没有 `window`，
 * 原版一旦真被调用就是 ReferenceError。
 */
import { ref, computed, onMounted, onUnmounted } from 'vue'
import { onShow } from '@dcloudio/uni-app'
import { getTasks } from '../../core/tasks'
import { getAllApprovals, submitDecision } from '../../core/approval'

interface Approval {
  requestId: string
  sid: string
  tool: string
  detail: string
  remainSecs: number
  expired: boolean
  canPersist: boolean
}

const items = ref<Approval[]>([])

async function loadApprovals() {
  // 先拿 tasks 列表，从中提取 sid（relay 模式下手机只有一个设备级会话）
  const tasks = await getTasks()
  const sids = [...new Set(tasks.map((t) => t.sid).filter(Boolean))]
  if (!sids.length) {
    items.value = []
    return
  }
  const raw = await getAllApprovals(sids)
  items.value = raw.map((r) => ({
    requestId: r.request_id,
    sid: r.sid,
    tool: r.tool,
    detail: r.detail,
    remainSecs: r.remain_secs,
    expired: r.expired,
    canPersist: r.can_persist,
  }))
}

let timer: ReturnType<typeof setInterval> | undefined
onMounted(() => {
  void loadApprovals()
  timer = setInterval(() => {
    for (const a of items.value) {
      if (a.expired || a.remainSecs <= 0) continue
      a.remainSecs -= 1
      if (a.remainSecs <= 0) a.expired = true
    }
  }, 1000)
})
onUnmounted(() => {
  if (timer) clearInterval(timer)
})

const active = computed(() => items.value.find((a) => !a.expired))

async function decide(choice: 'allow' | 'deny') {
  const a = active.value
  if (!a) return
  const res = await submitDecision(a.sid, a.requestId, choice === 'allow' ? 'approved' : 'denied')
  if (!res.ok) {
    uni.showToast({ title: res.reason ?? '提交失败', icon: 'none' })
    return
  }
  // 成功后把这条从列表移除（服务端会同步更新）
  items.value = items.value.filter((x) => x.requestId !== a.requestId)
}

onShow(() => {
  void loadApprovals()
})
</script>

<template>
  <hm-shell title="待决定">
    <hm-empty
      v-if="!items.length"
      title="没有待确认的操作"
      hint="电脑上的任务要动用权限时会出现在这里，点一下就能决定放不放行。"
    />

    <view v-for="a in items" :key="a.requestId" class="appr" :class="{ gone: a.expired }">
      <view class="top">
        <text class="state">{{ a.expired ? '已超时自动拒绝' : '需要权限确认' }}</text>
        <text class="rid">{{ a.requestId }}</text>
      </view>
      <text class="tool">{{ a.tool }}</text>
      <text class="args">{{ a.detail }}</text>

      <template v-if="!a.expired">
        <view class="count">
          <text class="remain">{{ a.remainSecs }}s 后自动拒绝</text>
          <view class="bar"><view class="fill" :style="{ width: Math.min(100, (a.remainSecs / 600) * 100) + '%' }" /></view>
        </view>
        <view class="keys">
          <view class="key allow" @tap="decide('allow')"><text>允许一次</text></view>
          <view class="key deny" @tap="decide('deny')"><text>拒绝</text></view>
        </view>
        <text v-if="!a.canPersist" class="cap">永久放行需在桌面端确认</text>
        <text v-else class="cap accent">此次允许后仍可永久放行</text>
      </template>

      <view v-else class="expired-note">
        <text>600 秒内没点，已拒绝。这次没执行任何操作。</text>
      </view>
    </view>
  </hm-shell>
</template>

<style lang="scss" scoped>
.appr {
  background: $card;
  border-radius: $r-card;
  padding: 26rpx;
  margin: 0 $gutter 22rpx;
}
.appr.gone {
  opacity: 0.55;
}
.top {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 12rpx;
  margin-bottom: 14rpx;
}
.state {
  font-size: $fs-body;
  font-weight: 600;
  color: $fg;
}
.rid {
  flex: none;
  font-size: $fs-mini;
  color: $fg4;
  font-family: monospace;
}
.tool {
  display: block;
  font-size: $fs-sub;
  color: $accent;
  margin-bottom: 14rpx;
}
/* 参数块再往下沉一档（$base），比卡片本身更「内」——两层底色替代描边。 */
.args {
  display: block;
  background: $base;
  border-radius: 14rpx;
  padding: 18rpx;
  font-size: $fs-mini;
  color: $fg2;
  font-family: monospace;
  margin-bottom: 20rpx;
  word-break: break-all;
}

.count {
  display: flex;
  align-items: center;
  gap: 14rpx;
  margin-bottom: 20rpx;
}
.remain {
  flex: none;
  font-size: $fs-mini;
  color: $warn;
}
.bar {
  flex: 1;
  height: 6rpx;
  background: $raised;
  border-radius: 3rpx;
  overflow: hidden;
}
.fill {
  height: 100%;
  background: $warn;
}

.keys {
  display: flex;
  gap: 14rpx;
}
.key {
  flex: 1;
  height: $tap;
  display: flex;
  align-items: center;
  justify-content: center;
  border-radius: 44rpx;
  font-size: $fs-sub;
  background: $raised;
  color: $fg2;
}
.key.allow {
  background: $accent;
  color: $on-accent;
}

.cap {
  display: block;
  margin-top: 16rpx;
  font-size: $fs-mini;
  color: $fg4;
}
.expired-note {
  background: $err-bg;
  border-radius: 14rpx;
  padding: 16rpx;
  font-size: $fs-mini;
  color: $err;
}
.cap.accent {
  color: $accent;
}
</style>
