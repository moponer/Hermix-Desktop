<script setup lang="ts">
/**
 * 首屏。
 *
 * ## 为什么必须有这一页
 *
 * 改造前 `pages.json` 的第一页是 `pages/pairing`，于是**每次**冷启动都落在
 * 配对屏；而配对屏的 `onLoad` 里 `await probe()`，走的是没有超时的裸
 * `fetch`。三层叠加出来就是用户报的现象：转圈（splash 的 waiting 层）
 * → 空白（webview 没内容）→ 半透明黑蒙版（waiting 层残留）。整条链路上
 * **没有任何一处会给出错误**，因为 `main.js` 当时没有 errorHandler。
 *
 * 现在把「决定去哪一页」和「干活」拆开：
 * - 这一页**只读本地存储**，不发任何网络请求，因此不存在「卡在首屏」；
 * - 它**先渲染出像素**再做决定，白屏时间等于零；
 * - 上一步崩了的话，`fatal` 里有记录，这一页直接渲染成可恢复的错误屏，
 *   而不是把用户留在原地。
 *
 * ## 只剩一个出口
 *
 * 曾经这里是分流器（「配对了去对话，没配对去配对」）。2026-10-09 去掉：把
 * 未配对的用户按在配对屏上，等于用「还没连」这件事把整个 App 锁住。现在
 * **一律进主界面**，未连接是主界面里的一个状态而不是一道门。
 */
import { ref } from 'vue'
import { onLoad, onReady } from '@dcloudio/uni-app'
import { fatal, clearFatal, ensureSplashClosed } from '../../core/boot'

/** 分流失败时的兜底文案（reLaunch 被拒等）。 */
const stuck = ref('')

/**
 * **永远进主界面**，不管配没配对。
 *
 * 2026-10-09 改：原先是「配对了去对话，没配对去配对」，于是每次冷启动都把
 * 用户按在配对屏上——哪怕他只是想看一眼任务列表。主界面是壳，未连接只是壳
 * 上的一个状态；把「没连接」做成主界面里的一张卡片，用户想连的时候一点就到，
 * 不想连也能浏览。
 */
function route() {
  uni.reLaunch({
    url: '/pages/chat/index',
    fail: () => {
      stuck.value = '跳转到主界面失败'
    },
  })
}

function retry() {
  clearFatal()
  stuck.value = ''
  route()
}

onLoad(() => {
  if (fatal.value) return // 有致命错误就停在这儿，把错误摊开给用户看
  route()
})

// 首屏像素已上屏才关启动图：正常路径 onReady 可能因 onLoad 里的 reLaunch 而
// 跳过，最终由 chat 页的 onReady 兜底；致命错误停在本页时这里就是唯一关点。
onReady(() => {
  ensureSplashClosed()
})
</script>

<template>
  <view class="boot">
    <image class="logo" src="/static/mascot.png" mode="aspectFit" />
    <text class="name">Hermix M</text>

    <!-- 上一次启动崩了：把根因摊开，并给两条能走的路 -->
    <view v-if="fatal" class="panel">
      <text class="p-title">上次启动没能完成</text>
      <text class="p-msg">{{ fatal.message }}</text>
      <text class="p-hint">详情</text>
      <text class="p-detail">{{ fatal.detail }}</text>
      <view class="acts">
        <view class="btn pri" @tap="retry"><text>重试</text></view>
      </view>
    </view>

    <view v-else-if="stuck" class="panel">
      <text class="p-title">启动卡住了</text>
      <text class="p-msg">{{ stuck }}</text>
      <view class="acts">
        <view class="btn pri" @tap="route"><text>再试一次</text></view>
      </view>
    </view>

    <text v-else class="wait">正在准备…</text>
  </view>
</template>

<style lang="scss" scoped>
.boot {
  min-height: 100vh;
  background: $paper;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  padding: 60rpx;
  box-sizing: border-box;
}
.logo {
  width: 132rpx;
  height: 132rpx;
}
.name {
  margin-top: 20rpx;
  font-size: 26rpx;
  color: $fg3;
  letter-spacing: 2rpx;
}
.wait {
  margin-top: 60rpx;
  font-size: 21rpx;
  color: $fg4;
}

.panel {
  margin-top: 56rpx;
  width: 100%;
  background: $card;
  border-radius: $r-card;
  padding: 28rpx;
}
.p-title {
  display: block;
  font-size: 26rpx;
  font-weight: 600;
  color: $err;
}
.p-msg {
  display: block;
  margin-top: 12rpx;
  font-size: 23rpx;
  color: $fg;
  line-height: 1.5;
}
.p-hint {
  display: block;
  margin-top: 18rpx;
  font-size: 19rpx;
  color: $fg4;
}
.p-detail {
  display: block;
  margin-top: 6rpx;
  font-size: 19rpx;
  color: $fg3;
  font-family: monospace;
  word-break: break-all;
  max-height: 240rpx;
  overflow: hidden;
}
.acts {
  margin-top: 26rpx;
}
.btn.pri {
  height: 76rpx;
  line-height: 76rpx;
  text-align: center;
  border-radius: 38rpx;
  background: $accent;
  color: $on-accent;
  font-size: 25rpx;
  font-weight: 600;
}
</style>
