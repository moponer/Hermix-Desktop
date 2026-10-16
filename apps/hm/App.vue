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

function applyHmTheme(t: 'light' | 'dark') {
  const cls = t === 'light' ? 'theme-light' : 'theme-dark'
  const other = t === 'light' ? 'theme-dark' : 'theme-light'
  try { uni.setStorageSync('hm-theme', t) } catch {}
  // 非 H5 端（如小程序）没有 document，仅支持浅色默认，跳过切换。
  if (typeof document === 'undefined') return
  const root = document.documentElement
  root.classList.add(cls)
  root.classList.remove(other)
}
;(globalThis as any).__hmSetTheme = applyHmTheme

onLaunch(() => {
  // 默认浅色（墨绿）主题；曾手动切到“深色”则回退深色。
  let saved: string | null = null
  try { saved = uni.getStorageSync('hm-theme') } catch {}
  applyHmTheme(saved === 'dark' ? 'dark' : 'light')

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
/* 跨页复用原件（提示条 / 胶囊按钮 / 卡片壳）。只此一处引入。 */
@import './styles/shared.scss';

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

/* ── 主题变量（墨绿浅色默认 / 深色可切换）─────────────────────────
 * 暗色 $ 变量已改为引用这里的 --c-*，故所有页面随 :root / page 的这套值换色。
 * 同时挂到 page 以兼容小程序端（小程序根不是 :root，而是 page）。
 * 默认浅色：与 uni.scss 的 $hm-* 墨绿浅色套一一对应。 */
:root, page {
  --c-base: #f2f3f5;
  --c-paper: #f2f3f5;   /* 页 / 对话底 */
  --c-card: #ffffff;    /* 卡片 */
  --c-raised: #eef0f3;  /* 输入底 / 次表面 */
  --c-fg: #1a1d21;     /* 主文字 */
  --c-fg2: #5f636b;    /* 次要文字 */
  --c-fg3: #9aa0a8;    /* 辅助文字（仅大字/图标） */
  --c-fg4: #9aa0a8;
  --c-accent: #2f6b4f; /* 墨绿主色 */
  --c-on-accent: #ffffff;
  --c-ok: #2ba24a;
  --c-warn: #e8920c;
  --c-warn-bg: #fdf2e0;
  --c-err: #e5484d;
  --c-err-bg: #fdecec;
}
/* 深色：与桌面端同源（青碧 #6fae94），保留为可切换项。 */
:root.theme-dark, page.theme-dark {
  --c-base: #101113;
  --c-paper: #161719;
  --c-card: #1d1f22;
  --c-raised: #24262a;
  --c-fg: #ededed;
  --c-fg2: #9a9ea6;
  --c-fg3: #7f848c;
  --c-fg4: #5f636b;
  --c-accent: #6fae94;
  --c-on-accent: #14201b;
  --c-ok: #84b383;
  --c-warn: #d3a34c;
  --c-warn-bg: #332916;
  --c-err: #e06b52;
  --c-err-bg: #20262b;
}
</style>
