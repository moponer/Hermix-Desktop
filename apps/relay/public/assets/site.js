/* Hermix 官网公共脚本：顶导航、页脚、版本数据填充。
 *
 * 版本号与校验值一律现取现填——页面上的数字和客户端点「检查更新」读到的
 * 是同一份 manifest，不存在网页写新版、客户端说没有的可能。取不到就显示
 * 「未发布」，不回填一个看起来像的版本号。 */

(function () {
  'use strict';

  // 开源地址待定。填了就会出现在导航和页脚；没填时这些位置显示待公布。
  var REPO_URL = '';
  var VERSION_MARKER = 'var HERMIX_M_VERSION = "";';

  var NAV = [
    { href: 'index.html', key: 'home', label: '首页' },
    { href: 'desktop.html', key: 'desktop', label: 'Hermix Desktop' },
    { href: 'mobile.html', key: 'mobile', label: 'Hermix 手机端' },
    { href: 'spiker.html', key: 'spiker', label: 'Spiker' },
    { href: 'changelog.html', key: 'changelog', label: '更新日志' },
    { href: 'articles.html', key: 'articles', label: '文章' },
    { href: 'docs.html', key: 'docs', label: '文档' },
  ];

  function esc(s) {
    return String(s == null ? '' : s).replace(/[&<>"]/g, function (c) {
      return { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c];
    });
  }

  /* 导航与页脚各套一层居中容器：条带本身全宽（底纹与分隔线铺满），
     内容锁在 --container 里，与正文左边缘对齐。 */
  function navHtml(active) {
    var links = NAV.map(function (n) {
      return '<a href="' + n.href + '"' + (n.key === active ? ' class="on"' : '') + '>' + n.label + '</a>';
    }).join('');
    return (
      '<div class="nav-in">' +
      '<a class="brand" href="index.html"><img class="mark" src="brand.png" alt="" width="26" height="26">Hermix</a>' +
      '<button class="burger" id="burger" aria-label="菜单" aria-expanded="false" aria-controls="navlinks">菜单</button>' +
      '<div class="links" id="navlinks">' + links + '</div>' +
      '<div class="cta">' +
      '<span class="v" data-ver="desktop.version">—</span>' +
      '<a class="btn" href="desktop.html#download">下载</a>' +
      '</div></div>'
    );
  }

  function footerHtml() {
    var repo = REPO_URL
      ? '<a href="' + esc(REPO_URL) + '" target="_blank" rel="noopener">开源仓库</a>'
      : '<span>开源地址待公布</span>';
    return (
      '<div class="ft-in">' +
      '<img class="mark" src="brand.png" alt="" width="20" height="20"><span>Hermix</span>' +
      '<span>hermix.cssso.com</span>' + repo +
      '<span class="sp">Windows x64 · Android</span></div>'
    );
  }

  /* 分享卡片：一处注入，九个页面共用。文章页标题是 JS 事后替换的，
     那边调 setMeta 把 og 一起改掉，否则分享出去的还是通用标题。 */
  function ensureMeta(selector, attr, key) {
    var el = document.querySelector(selector);
    if (!el) {
      el = document.createElement('meta');
      el.setAttribute(attr, key);
      document.head.appendChild(el);
    }
    return el;
  }

  function mountMeta() {
    var descEl = document.querySelector('meta[name="description"]');
    var desc = descEl ? descEl.content : '';
    var img = new URL('static-mascot.png', location.href).href;
    ensureMeta('meta[property="og:title"]', 'property', 'og:title').content = document.title;
    ensureMeta('meta[property="og:description"]', 'property', 'og:description').content = desc;
    ensureMeta('meta[property="og:type"]', 'property', 'og:type').content = 'website';
    ensureMeta('meta[property="og:url"]', 'property', 'og:url').content = location.href;
    ensureMeta('meta[property="og:image"]', 'property', 'og:image').content = img;
    ensureMeta('meta[property="og:locale"]', 'property', 'og:locale').content = 'zh_CN';
    ensureMeta('meta[name="twitter:card"]', 'name', 'twitter:card').content = 'summary';
    ensureMeta('meta[name="twitter:title"]', 'name', 'twitter:title').content = document.title;
    ensureMeta('meta[name="twitter:description"]', 'name', 'twitter:description').content = desc;
    ensureMeta('meta[name="theme-color"]', 'name', 'theme-color').content = '#161412';
  }

  function setMeta(o) {
    mountMeta();
    if (o.title) {
      ensureMeta('meta[property="og:title"]', 'property', 'og:title').content = o.title;
      ensureMeta('meta[name="twitter:title"]', 'name', 'twitter:title').content = o.title;
    }
    if (o.description) {
      var d = document.querySelector('meta[name="description"]');
      if (d) d.content = o.description;
      ensureMeta('meta[property="og:description"]', 'property', 'og:description').content = o.description;
      ensureMeta('meta[name="twitter:description"]', 'name', 'twitter:description').content = o.description;
    }
    if (o.url) ensureMeta('meta[property="og:url"]', 'property', 'og:url').content = o.url;
  }

  /* 首屏预览的等比缩放。
   * 预览块内部按固定逻辑宽排版（窄栏里窗口不该压成一条），可用宽度不够时
   * 整体缩小。缩放系数必须由这里量 —— 元素自己带 transform 时，
   * 纯 CSS 拿不到它未被变换前的容器宽度。 */
  function fitShots() {
    document.querySelectorAll('.stage .fit').forEach(function (el) {
      var logical = el.offsetWidth;
      var avail = el.parentNode.clientWidth;
      if (!logical || !avail) return;
      var s = Math.min(1, avail / logical);
      el.style.transform = s < 1 ? 'scale(' + s + ')' : '';
      el.parentNode.style.height = s < 1 ? (el.offsetHeight * s) + 'px' : '';
    });
  }

  /* 标签页图标：页面已显式声明时跳过，这里只作兜底。 */
  function mountFavicon() {
    if (document.querySelector('link[rel="icon"]')) return;
    var l = document.createElement('link');
    l.rel = 'icon';
    l.type = 'image/png';
    l.href = 'favicon.png';
    document.head.appendChild(l);
  }

  function mount(active) {
    mountFavicon();
    mountMeta();
    fitShots();
    if (!window.__hermixFitBound) {
      window.__hermixFitBound = true;
      window.addEventListener('resize', fitShots);
      // 字体加载完成后行高会变，缩放后高度要重算一次
      if (document.fonts && document.fonts.ready) document.fonts.ready.then(fitShots);
    }
    var nav = document.querySelector('[data-nav]');
    if (nav) nav.innerHTML = navHtml(active);
    var ft = document.querySelector('[data-footer]');
    if (ft) ft.innerHTML = footerHtml();
    var b = document.getElementById('burger');
    if (b) {
      b.addEventListener('click', function () {
        var open = document.getElementById('navlinks').classList.toggle('open');
        b.setAttribute('aria-expanded', open ? 'true' : 'false');
      });
    }
  }

  // ── 版本数据 ──────────────────────────────────────────────
  // 清单由发布脚本写在 public/<channel>/manifest.json，relay 逐条路由吐出；
  // APK 走 /api/app-info（没上传时 available=false）。
  function fmtDate(iso) {
    if (!iso) return '';
    var d = new Date(iso);
    if (isNaN(d.getTime())) return String(iso);
    var p = function (n) { return n < 10 ? '0' + n : '' + n; };
    // 本地时区渲染（2026-10-12 拍板不用 UTC）：getUTC* 换 get*，
    // 浏览器自己按系统时区换算，用户看到的就是手机/电脑时钟上的时刻。
    return d.getFullYear() + '-' + p(d.getMonth() + 1) + '-' + p(d.getDate()) +
      ' ' + p(d.getHours()) + ':' + p(d.getMinutes());
  }

  function short(sha) {
    if (!sha) return '';
    return sha.length > 16 ? sha.slice(0, 8) + '…' + sha.slice(-5) : sha;
  }

  function getJson(url) {
    return fetch(url, { cache: 'no-store' })
      .then(function (r) { return r.ok ? r.json() : null; })
      .catch(function () { return null; });
  }

  /* 把三个通道的数据摊平成 key -> value，供 data-ver 取值。
   * desktop.* / spiker.* / apk.* */
  function flatten(desk, spik, apk) {
    var out = {};
    function put(ch, m, url) {
      var ok = !!(m && m.version);
      // 取不到清单不等于产品没发版——只是这台机器上没有清单文件
      // （发布脚本部署到服务器，本地预览时没有）。措辞必须说清是「没读到」。
      out[ch + '.version'] = ok ? m.version : '未读到清单';
      // 只显日期:午夜 00:00 UTC 的时刻部分没有信息量,还占宽度
      out[ch + '.date'] = ok ? (m.pub_date || m.updatedAt || '').slice(0, 10) : '';
      out[ch + '.sha'] = ok ? (m.sha256 || '') : '';
      out[ch + '.shaShort'] = ok ? short(m.sha256) : '';
      out[ch + '.notes'] = ok ? (m.notes || '') : '';
      out[ch + '.min'] = ok ? (m.min_version || '不限制') : '';
      // 应用名+版本号:按钮文案用它(「更新包 zip」这类无名文案被否过)
      var appName = ch === 'desktop' ? 'Hermix Desktop' : ch === 'spiker' ? 'Spiker' : ch;
      out[ch + '.label'] = ok ? appName + ' ' + m.version : '';
      out[ch + '.url'] = ok ? (m.url || url) : url;
      out[ch + '.ok'] = ok ? '1' : '';
    }
    put('desktop', desk, 'desktop/latest.zip');
    put('spiker', spik, 'spiker/latest.zip');
    // APK 走独立接口，字段命名不同（available / version 可能为空）。
    var aok = !!(apk && apk.available);
    out['apk.ok'] = aok ? '1' : '';
    out['apk.version'] = aok ? (apk.version || '未标注') : '未上传';
    out['apk.hint'] = aok ? '' : 'APK 还没放到发布目录';
    out['apk.date'] = aok ? fmtDate(apk.updatedAt) : '';
    out['apk.sha'] = aok ? (apk.sha256 || '') : '';
    out['apk.shaShort'] = aok ? short(apk.sha256) : '';
    out['apk.size'] = aok && apk.size ? Math.round(apk.size / 1048576) + ' MB' : '';
    out['apk.url'] = 'download/hermix.apk';
    return out;
  }

  function apply(map) {
    document.querySelectorAll('[data-ver]').forEach(function (el) {
      var v = map[el.getAttribute('data-ver')];
      if (v == null) return;
      el.textContent = v;
    });
    // 有内容才显示的行：data-ver-hide 在值为空时整行隐藏
    document.querySelectorAll('[data-ver-hide]').forEach(function (el) {
      var v = map[el.getAttribute('data-ver-hide')];
      if (!v) el.style.display = 'none';
    });
    // data-ver-href：把链接指向清单里的 url
    document.querySelectorAll('[data-ver-href]').forEach(function (el) {
      var v = map[el.getAttribute('data-ver-href')];
      if (v) el.setAttribute('href', v);
    });
    document.dispatchEvent(new CustomEvent('hermix:versions', { detail: map }));
  }

  function loadVersions() {
    return Promise.all([
      getJson('desktop/manifest.json'),
      getJson('spiker/manifest.json'),
      getJson('api/app-info'),
    ]).then(function (r) {
      apply(flatten(r[0], r[1], r[2]));
      return flatten(r[0], r[1], r[2]);
    });
  }

  window.HERMIX = {
    REPO_URL: REPO_URL,
    VERSION_MARKER: VERSION_MARKER,
    mount: mount,
    loadVersions: loadVersions,
    esc: esc,
    fmtDate: fmtDate,
    shortSha: short,
    setMeta: setMeta,
    flatten: flatten,
    // 页面里写 <script>HERMIX.boot('desktop')</script> 即可装配导航 + 版本。
    boot: function (active) {
      mount(active);
      loadVersions();
    },
  };
})();
