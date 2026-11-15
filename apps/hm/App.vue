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

type HmTheme = 'notion' | 'notion-dark'

/** 旧色系（墨绿 / 夜墨）已删除，只留 Notion 亮/暗。 */
const THEME_CLASS: Record<HmTheme, string> = {
  notion: 'theme-notion',
  'notion-dark': 'theme-notion-dark',
}

/** 旧存储值迁移：light → notion，dark → notion-dark。 */
function normalizeTheme(v: unknown): HmTheme {
  if (v === 'notion-dark' || v === 'dark') return 'notion-dark'
  return 'notion'
}

function applyHmTheme(t: HmTheme) {
  const cls = THEME_CLASS[t] ?? THEME_CLASS.notion
  try { uni.setStorageSync('hm-theme', t) } catch {}
  // 非 H5 端（如小程序）没有 document，仅支持默认浅色，跳过切换。
  if (typeof document === 'undefined') return
  const paint = () => {
    // **三处都要挂**：默认值同时声明在 `:root` 与 `uni-page-body`（`page` 在
    // H5 编译成后者），只改 <html> 的话页体上的默认值会把主题值再盖回去 ——
    // 现象是「算出来是 Notion、画出来还是墨绿」。
    const nodes = [document.documentElement, document.body, document.querySelector('uni-page-body')]
    for (const el of nodes) {
      if (!el) continue
      for (const c of Object.values(THEME_CLASS)) el.classList.toggle(c, c === cls)
    }
  }
  paint()
  // uni-page-body 可能晚于 onLaunch 挂载：下一帧补一次。
  requestAnimationFrame(paint)
}
;(globalThis as any).__hmSetTheme = applyHmTheme

onLaunch(() => {
  // 默认浅色（墨绿）主题；曾手动切到“深色”则回退深色。
  let saved: string | null = null
  try { saved = uni.getStorageSync('hm-theme') } catch {}
  // 默认 Notion；旧版本存过 light/dark 的一并迁移到 notion / notion-dark。
  applyHmTheme(normalizeTheme(saved))

  // 时钟偏差必须在**配对前**就量一次：请求签名含 unix 时间戳，容许窗口只有
  // ±300 秒，设备时间不准会全部 401，而用户看到的现象是「配对成功了但连不上」
  // ——最难自查的一类故障。这里只记日志，配对屏负责展示。
  void checkClockSkew().then((probe) => {
    // null = 没能与服务端对时（离线 / 请求失败）。此时没有基准可比，
    // 告警只会是噪声。
    if (probe.skew === null) return
    if (Math.abs(probe.skew) > 30) {
      console.warn(`[hermix-m] 设备时钟与服务端偏差 ${probe.skew}s，可能导致签名被拒`)
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
  /* Notion 亮色（默认，与桌面端 themes/notion.css 同源）。
     旧色系（墨绿 / 夜墨）2026-10-27 已删除。 */
  --c-base: #efefed;
  --c-paper: #ffffff;
  --c-card: #fbfbfa;
  --c-raised: #f1f1ef;
  --c-fg: #37352f;
  --c-fg2: #605c54;
  --c-fg3: #6b6862;
  --c-fg4: #8a8782;
  --c-accent: #1c6ac4;
  --c-on-accent: #ffffff;
  --c-ok: #3a794e;
  --c-warn: #b07a1a;
  --c-warn-bg: #fdecc8;
  --c-err: #bf4134;
  --c-err-bg: #ffe2dd;
}
/* 旧深色套（夜墨）已删除。 */

/* ── 主题套 3/4：Notion（浅 / 深）· 2026-10-27 增加 ───────────────
 * 取值与桌面端 themes/notion.css、themes/notion-dark.css 同源：
 * 暖灰纸面（#f7f7f5 / #191919）+ 墨色文字 + Notion 蓝（#1c6ac4 / #6ba6ff）。
 * 手机端默认就是这一套，四端（桌面 / 查看器 / 手机 / 网页）看起来是同一个产品。
 * 语义四色改用文字安全档的取值，保证在暖底与深底上都 ≥4.5:1。 */
/* 亮色即上面 `:root, page` 的默认值，不再单列一套（避免两份取值漂移）。 */

:root.theme-notion-dark, page.theme-notion-dark {
  --c-base: #101010;
  --c-paper: #191919;
  --c-card: #1f1f1f;
  --c-raised: #2c2c2c;
  --c-fg: #d4d4d4;
  --c-fg2: #a3a3a3;
  --c-fg3: #9b9b9b;
  --c-fg4: #6f6f6f;
  --c-accent: #6ba6ff;
  --c-on-accent: #0d1520;
  --c-ok: #84b383;
  --c-warn: #d3a34c;
  --c-warn-bg: #2b2519;
  --c-err: #e06b52;
  --c-err-bg: #262020;
}
</style>
