/* 官网静态契约检查：设计规则里「写死的部分」用代码钉住，
 * 不靠人肉复查。任何一条不过就 exit 1。
 * 用法：node apps/relay/scripts/check-site-contract.cjs */
const fs = require('fs');
const path = require('path');

const PUB = path.join(__dirname, '..', 'public');
const pages = fs.readdirSync(PUB).filter((f) => f.endsWith('.html'));
const bad = [];

const EMOJI = /[\u{1F300}-\u{1FAFF}\u{2600}-\u{27BF}\u{FE0F}]/u;

for (const f of pages) {
  const s = fs.readFileSync(path.join(PUB, f), 'utf8');
  const must = {
    '<html lang="zh-CN">': s.includes('<html lang="zh-CN">'),
    '<title>': s.includes('<title>'),
    'meta description': s.includes('name="description"'),
    'rel="icon"': s.includes('rel="icon"'),
    '跳转正文链接 .skip': s.includes('class="skip"'),
    '不出现斜体': !s.includes('font-style: italic'),
    '不出现 emoji': !EMOJI.test(s),
  };
  for (const [k, ok] of Object.entries(must)) if (!ok) bad.push(`${f} 缺少/违反：${k}`);
}

const css = fs.readFileSync(path.join(PUB, 'assets', 'site.css'), 'utf8');
const cssMust = {
  '已删除旧低对比灰 #97a39a': !css.includes('#97a39a'),
  '已删除旧禁用底色 #cfd6cd': !css.includes('#cfd6cd'),
  '无左侧竖线装饰': !/border-left:\s*[1-9]/.test(css),
  '无 transition:all': !/transition:\s*all/.test(css),
  '有 prefers-reduced-motion 兜底': css.includes('prefers-reduced-motion'),
  '有 hover 能力守卫': css.includes('@media (hover:hover)'),
  '只用强化 ease-out': !/(?<!-)ease-in(?!-out)/.test(css), // ease-in 单独出现即违规
  '有统一容器变量': css.includes('--container'),
  '有禁用原因样式 .why': css.includes('.why'),
};
for (const [k, ok] of Object.entries(cssMust)) if (!ok) bad.push(`site.css 违反：${k}`);

const js = fs.readFileSync(path.join(PUB, 'assets', 'site.js'), 'utf8');
if (!js.includes('setMeta')) bad.push('site.js 缺少 setMeta（文章页分享卡片不会随篇更新）');
if (!js.includes('fitShots')) bad.push('site.js 缺少 fitShots（窄屏首屏预览不会缩放）');

if (bad.length) {
  console.error('FAIL 契约检查未通过：\n' + bad.map((b) => ' - ' + b).join('\n'));
  process.exit(1);
}
console.log(`PASS ${pages.length} 个页面 + site.css + site.js 契约全部通过`);
