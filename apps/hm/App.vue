<script setup lang="ts">
/**
 * 应用壳。
 *
 * 这里只做三件事，且都是「启动期不许静默失败」的保障：
 * 1. `onError` / `onUnhandledRejection` —— 未捕获异常一律落 core/boot，
 *    由 boot 页渲染成可恢复的错误屏，而不是留一块白屏；
 * 2. 时钟偏差**只记日志**，绝不阻塞启动——它是网络往返，发请求就带
 *    超时了（core/http.ts），不构成卡首屏的理由；
 * 3. 全局基底样式。
 *
 * 首屏决策（已配对走哪一页）不在这里，而在 pages/boot：放在 App 里会
 * 和 splashscreen 的关闭时机耦合，放进页面才有确定的「先出像素」保证。
 */
import { onLaunch, onError, onUnhandledRejection } from '@dcloudio/uni-app'
import { checkClockSkew } from './core/clock'
import { reportFatal } from './core/boot'

onLaunch(() => {
  // 时钟偏差必须在**配对前**就量一次：请求签名含 unix 时间戳，容许窗口只有
  // ±300 秒，设备时间不准会全部 401，而用户看到的现象是「配对成功了但连不上」
  // ——最难自查的一类故障。这里只记日志，配对屏负责展示。
  void checkClockSkew().then((skew) => {
    // null = 没能与服务端对时（离线 / 请求失败）。此时没有基准可比，
    // 告警只会是噪声。
    if (skew === null) return
    if (Math.abs(skew) > 30) {
      console.warn(`[hermix-m] 设备时钟与服务端偏差 ${skew}s，可能导致签名被拒`)
    }
  })
})

onError((err) => reportFatal(err, 'App.onError'))
onUnhandledRejection((event) => reportFatal(event?.reason ?? event, 'unhandledrejection'))
</script>

<style lang="scss">
/* 全局基底：暗色跟随主题，不给浅色留残影 */
page {
  background-color: $paper;
  color: $fg;
  font-size: 28rpx;
  line-height: 1.55;
  -webkit-font-smoothing: antialiased;
}
/* 首屏之前不许有任何一条漏出来的横向滚动条 */
page,
body {
  width: 100%;
  overflow-x: hidden;
}
</style>
