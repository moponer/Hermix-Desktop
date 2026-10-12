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

interface Approval {
  requestId: string
  tool: string
  detail: string
  /** 剩余秒数；到 0 = 已超时自动拒绝 */
  remainSecs: number
  expired: boolean
}

const items = ref<Approval[]>([])

let timer: ReturnType<typeof setInterval> | undefined
onMounted(() => {
  timer = setInterval(() => {
    for (const a of items.value) {
      if (a.expired || a.remainSecs <= 0) continue
      a.remainSecs -= 1
      // 到点 = 服务端已自动 deny（默认 600s）。前端只是把既成事实显示出来，
      // 真正生效的是服务端；这里不能自己判「通过」。
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
  // TODO(接 REST): POST /v1/sessions/{sid}/approval
  //   { request_id, decision: 'approved' | 'denied' }
  //   决定后卡片置灰 + 徽标；点击未生效的态要显示 expired 而不是静默无响应
  items.value = items.value.filter((x) => x.requestId !== a.requestId)
}

onShow(() => {
  // TODO(接 REST): 拉待审批列表。服务端在 relay 边界已按设备过滤。
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
        <text class="cap">永久放行需在桌面端确认</text>
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
</style>
