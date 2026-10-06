<script setup lang="ts">
/**
 * 屏 4 · 任务
 *
 * 归属过滤是这一屏的**安全要点**：`GET /v1/tasks` 服务端已按 `origin.device_id()`
 * 过滤（只回本设备的 sid），App 侧不要自己"帮忙"过滤——那等于把过滤逻辑
 * 复制一份，两边漂移就会漏。
 *
 * 每一位次都要显示：IM 侧原本的消息是挤聊天锁的，用户发完看不到任何反馈，
 * 轮后整批重跑。位次是"排队了"与"没收到"的唯一区别。
 */
import { ref } from 'vue'

interface TaskView {
  sid: string
  state: 'running' | 'queued'
  first_line: string
  position: number | null
}

const tasks = ref<TaskView[]>([
  { sid: 's1', state: 'running', first_line: '把 tools 那一层的 dispatch 拆出来…', position: null },
  { sid: 's1', state: 'queued', first_line: '顺便把 README 的安装步骤补上', position: 1 },
  { sid: 's1', state: 'queued', first_line: '跑一遍全量测试', position: 2 },
])

/** 上一轮没跑完提示——挂在这一屏，不主动推送。 */
const unfinishedNote = ref('⏸ 上一轮没跑完（10-04 23:12 停在中途）。发 /继续 接着来——会先核对现状，不盲目重做')

async function refresh() {
  // TODO(接 REST): GET /v1/tasks（Authorization + 签名头）
  // 服务端只回本设备的 sid，App 不做二次过滤
}
async function stop(sid: string) {
  // TODO(接 REST): POST /v1/sessions/{sid}/interrupt
}
</script>

<template>
  <view class="screen">
    <view class="appbar"><text class="title">任务</text></view>

    <scroll-view class="body" scroll-y>
      <view v-for="(t, i) in tasks" :key="i" class="task">
        <text class="idx">#{{ i + 1 }}</text>
        <view v-if="t.state === 'running'" class="spin" />
        <text v-else class="badge">第 {{ t.position }} 位</text>
        <view class="txt">
          <text class="line">{{ t.first_line }}</text>
          <text class="sub">{{ t.state === 'running' ? '运行中' : '排队' }}</text>
        </view>
        <view v-if="t.state === 'running'" class="stop" @tap="stop(t.sid)">■</view>
      </view>

      <view v-if="unfinishedNote" class="notice warn">
        <text>{{ unfinishedNote }}</text>
      </view>

      <view v-if="tasks.length === 0" class="empty">当前没有任务</view>
    </scroll-view>
  </view>
</template>

<style lang="scss" scoped>
.screen { display: flex; flex-direction: column; height: 100vh; background: $paper; }
.appbar { padding: 100rpx 30rpx 20rpx; background: $base; border-bottom: 1rpx solid $line; }
.title { font-size: 28rpx; font-weight: 600; color: $fg; }
.body { flex: 1; padding: 24rpx 26rpx; }

.task {
  display: flex;
  align-items: center;
  gap: 16rpx;
  background: $card;
  border: 1rpx solid $line;
  border-radius: $r-card;
  padding: 22rpx;
  margin-bottom: 16rpx;
}
.idx { font-size: 19rpx; color: $fg4; }
.spin {
  width: 22rpx;
  height: 22rpx;
  border: 3rpx solid $line;
  border-top-color: $accent;
  border-radius: 50%;
  animation: sp 0.8s linear infinite;
}
@keyframes sp { to { transform: rotate(360deg); } }
.badge { font-size: 19rpx; color: $warn; background: $warn-bg; padding: 4rpx 10rpx; border-radius: 8rpx; }
.txt { flex: 1; min-width: 0; }
.line { display: block; font-size: 24rpx; color: $fg; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.sub { display: block; font-size: 19rpx; color: $fg2; margin-top: 4rpx; }
.stop {
  width: 48rpx; height: 48rpx; line-height: 48rpx; text-align: center;
  border-radius: 24rpx; background: $err-bg; color: $err; font-size: 20rpx;
}

.notice { border-radius: 20rpx; padding: 20rpx; font-size: 21rpx; line-height: 1.5; margin-top: 20rpx; }
.notice.warn { background: $warn-bg; color: $warn; border: 1rpx solid rgba(211, 163, 76, 0.25); }
.empty { text-align: center; color: $fg4; font-size: 22rpx; padding: 60rpx 0; }
</style>
