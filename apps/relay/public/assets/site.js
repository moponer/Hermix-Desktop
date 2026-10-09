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

  function navHtml(active) {
    var links = NAV.map(function (n) {
      return '<a href="' + n.href + '"' + (n.key === active ? ' class="on"' : '') + '>' + n.label + '</a>';
    }).join('');
    return (
      '<div class="brand" onclick="location.href=\'index.html\'"><span class="dot"></span>Hermix</div>' +
      '<button class="burger" id="burger" aria-label="菜单">菜单</button>' +
      '<div class="links" id="navlinks">' + links + '</div>' +
      '<div class="cta">' +
      '<span class="v" data-ver="desktop.version">—</span>' +
      '<a class="btn" href="desktop.html#download">下载</a>' +
      '</div>'
    );
  }

  function footerHtml() {
    var repo = REPO_URL
      ? '<a href="' + esc(REPO_URL) + '" target="_blank" rel="noopener">开源仓库</a>'
      : '<span>开源地址待公布</span>';
    return (
      '<span>Hermix</span><span>hermix.cssso.com</span>' + repo +
      '<span class="sp">Windows x64 · Android</span>'
    );
  }

  function mount(active) {
    var nav = document.querySelector('[data-nav]');
    if (nav) nav.innerHTML = navHtml(active);
    var ft = document.querySelector('[data-footer]');
    if (ft) ft.innerHTML = footerHtml();
    var b = document.getElementById('burger');
    if (b) {
      b.addEventListener('click', function () {
        document.getElementById('navlinks').classList.toggle('open');
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
    return d.getUTCFullYear() + '-' + p(d.getUTCMonth() + 1) + '-' + p(d.getUTCDate()) +
      ' ' + p(d.getUTCHours()) + ':' + p(d.getUTCMinutes()) + ' UTC';
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
      out[ch + '.version'] = ok ? m.version : '未发布';
      out[ch + '.date'] = ok ? fmtDate(m.pub_date || m.updatedAt) : '';
      out[ch + '.sha'] = ok ? (m.sha256 || '') : '';
      out[ch + '.shaShort'] = ok ? short(m.sha256) : '';
      out[ch + '.notes'] = ok ? (m.notes || '') : '';
      out[ch + '.min'] = ok ? (m.min_version || '不限制') : '';
      out[ch + '.url'] = ok ? (m.url || url) : url;
      out[ch + '.ok'] = ok ? '1' : '';
    }
    put('desktop', desk, 'desktop/latest.zip');
    put('spiker', spik, 'spiker/latest.zip');
    // APK 走独立接口，字段命名不同（available / version 可能为空）。
    var aok = !!(apk && apk.available);
    out['apk.ok'] = aok ? '1' : '';
    out['apk.version'] = aok ? (apk.version || '未标注') : '未上传';
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
    flatten: flatten,
    // 页面里写 <script>HERMIX.boot('desktop')</script> 即可装配导航 + 版本。
    boot: function (active) {
      mount(active);
      loadVersions();
    },
  };
})();
