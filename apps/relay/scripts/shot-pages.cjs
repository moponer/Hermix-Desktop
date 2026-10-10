/* 官网页面截图：桌面 + 移动各一张，供设计评审用。
 * 用法：node scripts/shot-pages.cjs [baseUrl] */
const path = require('path');
const fs = require('fs');
const { chromium } = require(path.join(
  __dirname, '..', '..', 'desktop-tauri', 'node_modules', 'playwright'));

const BASE = process.argv[2] || 'http://127.0.0.1:8099';
const OUT = path.join(__dirname, '..', '..', '..', 'outputs', 'relay-shots');
const PAGES = process.argv[3]
  ? [process.argv[3]]
  : ['index', 'desktop', 'mobile', 'spiker', 'changelog', 'articles', 'docs', 'dev'];

(async () => {
  fs.mkdirSync(OUT, { recursive: true });
  // 本机没有下载 playwright 自带 chromium，走系统 Chrome / Edge 通道
  const browser = await chromium.launch({ channel: 'chrome', args: ['--no-proxy-server'] })
    .catch(() => chromium.launch({ channel: 'msedge', args: ['--no-proxy-server'] }));

  for (const size of [{ w: 1440, h: 900, tag: 'desktop' }, { w: 390, h: 844, tag: 'mobile' }]) {
    const ctx = await browser.newContext({
      viewport: { width: size.w, height: size.h },
      deviceScaleFactor: 2,
    });
    for (const p of PAGES) {
      const page = await ctx.newPage();
      await page.goto(`${BASE}/${p}.html`, { waitUntil: 'load', timeout: 30000 });
      await page.waitForTimeout(400);
      await page.screenshot({
        path: path.join(OUT, `${p}-${size.tag}.png`),
        fullPage: true,
      });
      await page.close();
    }
    await ctx.close();
  }

  await browser.close();
  console.log('SHOTS::' + OUT);
})().catch((e) => { console.error(e); process.exit(1); });
