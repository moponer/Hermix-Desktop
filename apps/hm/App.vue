<script setup lang="ts">
import { onLaunch } from '@dcloudio/uni-app'
import { checkClockSkew } from './core/clock'

onLaunch(() => {
  // 时钟偏差必须在**配对前**就量一次：请求签名含 unix 时间戳，容许窗口只有
  // ±300 秒，设备时间不准会全部 401，而用户看到的现象是"配对成功了但连不上"
  // ——最难自查的一类故障。这里只记日志，屏 1 负责展示。
  checkClockSkew().then((skew) => {
    // null = 没能与服务端对时（离线 / 请求失败）。此时没有基准可比，
    // 告警只会是噪声。原先靠 `Math.abs(null) === 0` 侥幸不触发，
    // 但类型上不成立，vue-tsc 会拦。
    if (skew === null) return
    if (Math.abs(skew) > 30) {
      console.warn(`[hermix-m] 设备时钟与服务端偏差 ${skew}s，可能导致签名被拒`)
    }
  })
})
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
</style>
