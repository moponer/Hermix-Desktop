<script setup lang="ts">
/**
 * 屏 3 · 审批
 *
 * **只有两键，没有"永久允许"**——这不是没做完，是技术上不可能：
 * `Origin::PairedDevice` 对 `Capability::ApproveAlways` 是无条件 `Denied`
 * （`origin.rs:65-72`），且 `effective_decision_for_paired` 对矩阵 `Denied`
 * 不做挽救（`security.rs:429-440`）。设备 scopes 里写 `approvealways` 也不生效。
 *
 * 麻烦的是它**不能静默消失**：手机屏幕小，少一个按钮用户未必察觉，但下次审批
 * 照样弹，会以为是自己点错了。所以服务端显式下发 `can_persist:false`，这里
 * 必须把它渲染出来。
 */
import { ref, computed, onUnmounted } from 'vue'

interface Approval {
  requestId: string
  tool: string
  detail: string
  /** 剩余秒数；到 0 = 已超时自动拒绝 */
  remainSecs: number
  expired: boolean
}

const items = ref<Approval[]>([
  { requestId: 'r-7f2a', tool: 'terminal · rm -rf target/debug', detail: '{"command":"rm -rf target/debug","cwd":"F:\\\\spiker\\\\hermix-desktop"}', remainSecs: 347, expired: false },
  { requestId: 'r-3b91', tool: 'write_file · src/lib.rs', detail: '{"path":"src/lib.rs","bytes":4821}', remainSecs: 0, expired: true },
])

let timer: number | undefined
function startTicker() {
  timer = window.setInterval(() => {
    for (const a of items.value) {
      if (a.expired || a.remainSecs <= 0) continue
      a.remainSecs -= 1
      // 到点 = 服务端已自动 deny（默认 600s）。前端只是把既成事实显示出来，
      // 真正生效的是服务端；这里不能自己判"通过"。
      if (a.remainSecs <= 0) a.expired = true
    }
  }, 1000)
}
onUnmounted(() => {
  if (timer) clearInterval(timer)
})

const active = computed(() => items.value.find((a) => !a.expired))

async function decide(choice: 'allow' | 'deny') {
  const a = active.value
  if (!a) return
  // TODO(接 REST): POST /v1/sessions/{sid}/approval
  //   { request_id, decision: 'approved' | 'denied' }
  // 决定后卡片置灰 + 徽标；点击未生效的态要显示 expired 而不是静默无响应
  items.value = items.value.filter((x) => x.requestId !== a.requestId)
}
</script>

<template>
  <view class="screen">
    <view class="appbar"><text class="title">待决定</text></view>

    <scroll-view class="body" scroll-y>
      <view v-for="a in items" :key="a.requestId" class="appr" :class="{ gone: a.expired }">
        <view class="top">
          <text>{{ a.expired ? '已超时自动拒绝' : '需要权限确认' }}</text>
          <text class="rid">{{ a.requestId }}</text>
        </view>
        <text class="tool">{{ a.tool }}</text>
        <text class="args">{{ a.detail }}</text>

        <template v-if="!a.expired">
          <view class="count">
            <text>{{ a.remainSecs }}s 后自动拒绝</text>
            <view class="bar"><view class="fill" :style="{ width: Math.min(100, (a.remainSecs / 600) * 100) + '%' }" /></view>
          </view>
          <view class="keys">
            <button class="btn allow" @tap="decide('allow')">允许一次</button>
            <button class="btn deny" @tap="decide('deny')">拒绝</button>
          </view>
          <text class="cap">永久放行需在桌面端确认</text>
        </template>

        <view v-else class="expired-note">
          <text>600 秒内没点，已拒绝。这次没执行任何操作。</text>
        </view>
      </view>
    </scroll-view>
  </view>
</template>

<style lang="scss" scoped>
.screen { display: flex; flex-direction: column; height: 100vh; background: $paper; }
.appbar { padding: 100rpx 30rpx 20rpx; background: $base; border-bottom: 1rpx solid $line; }
.title { font-size: 28rpx; font-weight: 600; color: $fg; }
.body { flex: 1; padding: 24rpx 26rpx; }

.appr {
  background: $card;
  border: 1rpx solid $line;
  border-radius: $r-card;
  padding: 24rpx;
  margin-bottom: 24rpx;
}
.appr.gone { opacity: 0.55; }
.top { display: flex; justify-content: space-between; font-size: 24rpx; font-weight: 600; color: $fg; margin-bottom: 14rpx; }
.rid { font-size: 18rpx; color: $fg4; }
.tool { display: block; font-size: 20rpx; color: $accent; margin-bottom: 14rpx; }
.args {
  display: block;
  background: $base;
  border: 1rpx solid $line;
  border-radius: 16rpx;
  padding: 16rpx;
  font-size: 19rpx;
  color: $fg2;
  font-family: monospace;
  margin-bottom: 18rpx;
  word-break: break-all;
}

.count { display: flex; align-items: center; gap: 14rpx; font-size: 20rpx; color: $warn; margin-bottom: 20rpx; }
.bar { flex: 1; height: 6rpx; background: $raised; border-radius: 3rpx; overflow: hidden; }
.fill { height: 100%; background: $warn; }

.keys { display: flex; gap: 12rpx; }
.btn { flex: 1; height: 72rpx; line-height: 72rpx; border-radius: 36rpx; font-size: 24rpx; border: none; padding: 0; }
.btn.allow { background: $accent; color: $on-accent; }
.btn.deny { background: transparent; border: 1rpx solid $edge; color: $fg2; }

.cap { display: block; margin-top: 16rpx; font-size: 19rpx; color: $fg4; }
.expired-note {
  background: $err-bg;
  border: 1rpx solid rgba(224, 107, 82, 0.25);
  border-radius: 16rpx;
  padding: 16rpx;
  font-size: 20rpx;
  color: $err;
}
</style>
