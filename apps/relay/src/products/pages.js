//! 官网页面路由（静态引导面）：/ 、/static-mascot.png 、/api/app-info 、
//! /download/hermix.apk。
//!
//! 从 server.js 原样搬移（行为不变，决策见 docs/notes/implemented/
//! architecture/2026-10-11-relay-structured-deployment.md §4 静态层）：
//! APK 指纹按 mtime 缓存，安装包一天传不了几次，每次请求重算大文件
//! 哈希会把 CPU 打满。没上传就如实说「未上传」，不给假按钮。

'use strict';

const fs = require('fs');
const path = require('path');
const { hashFileSync } = require('./util');

function register(app, { publicDir }) {
  const apkPath = path.join(publicDir, 'hermix.apk');
  let apkCache = { mtimeMs: 0, sha256: null };

  function apkInfoSync() {
    let st;
    try {
      st = fs.statSync(apkPath);
    } catch {
      return { available: false, version: null, size: 0, sha256: null, updatedAt: null };
    }
    if (apkCache.mtimeMs !== st.mtimeMs) {
      apkCache = { mtimeMs: st.mtimeMs, sha256: hashFileSync(apkPath) };
    }
    const version = (() => {
      try {
        const html = fs.readFileSync(path.join(publicDir, 'index.html'), 'utf8');
        const m = html.match(/HERMIX_M_VERSION\s*=\s*["']([^"']+)["']/) || html.match(/版本[：:]?\s*v?([0-9][^<\s]*)/);
        return m ? m[1] : null;
      } catch {
        return null;
      }
    })();
    return {
      available: true,
      version,
      size: st.size,
      sha256: apkCache.sha256,
      updatedAt: new Date(st.mtimeMs).toISOString(),
    };
  }

  app.get('/', (_req, res) => {
    res.sendFile(path.join(publicDir, 'index.html'));
  });

  app.get('/static-mascot.png', (_req, res) => {
    res.sendFile(path.join(publicDir, 'static-mascot.png'));
  });

  app.get('/api/app-info', (_req, res) => {
    res.json(apkInfoSync());
  });

  // 下载文件名带版本+平台标识（2026-10-12）：裸 hermix.apk 落到手机文件
  // 管理器里认不出是哪个包、哪个端。版本号单一来源 = index.html 的
  // HERMIX_M_VERSION（与 /api/app-info 同源，无第二处手填）。固定 URL
  // 302 到带版本 URL，浏览器落盘名随之带版本；盘上仍只存一份 hermix.apk
  // ——两份文件就是两份哈希，必然漂移。带版本 URL 是一次一发的易逝地址，
  // 旧版本 URL 在新包上传后会拿到新字节+旧名字，以固定 URL 为准。
  const apkVersionSafe = () => {
    const v = apkInfoSync().version;
    return v && /^[0-9A-Za-z.\-]+$/.test(v) ? v : null;
  };

  const serveApk = (res, filename) => {
    let st;
    try {
      st = fs.statSync(apkPath);
    } catch {
      return res.status(404).type('text/plain; charset=utf-8')
        .send('安装包尚未上传（运维需把 APK 放到 relay 目录的 public/hermix.apk 后重启项目）');
    }
    res.setHeader('Content-Type', 'application/vnd.android.package-archive');
    res.setHeader('Content-Length', st.size);
    res.setHeader('Content-Disposition', `attachment; filename="${filename}"`);
    res.setHeader('X-Checksum-Sha256', apkInfoSync().sha256 || '');
    fs.createReadStream(apkPath).pipe(res);
  };

  app.get('/download/hermix.apk', (_req, res) => {
    const v = apkVersionSafe();
    if (v) return res.redirect(302, `/download/hermix-m-${v}.apk`);
    // 版本号读不到（index.html 缺 marker）：退回旧行为，文件名不带版本
    serveApk(res, 'hermix.apk');
  });

  app.get('/download/hermix-m-:ver.apk', (req, res) => {
    const ver = /^[0-9A-Za-z.\-]+$/.test(req.params.ver) ? req.params.ver : null;
    if (!ver) {
      return res.status(400).type('text/plain; charset=utf-8').send('bad version in filename');
    }
    serveApk(res, `hermix-m-${ver}.apk`);
  });
}

module.exports = { register };
