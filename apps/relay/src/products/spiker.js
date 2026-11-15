//! spiker 产品路由（payload 型）：/spiker/manifest.json + /spiker/latest.zip
//! + /spiker/releases.json。
//!
//! 与 desktop.js 同构，差异只有两点：附件名（spiker 无 sidecar，zip 只含
//! exe + resources）与 deployDir。spiker 的发布说明暂走 legacy manifest
//! （并行会话维护该产品，本模块只包壳不改其内部约定；待其接入
//! releases.json 流水线时删掉回退分支即可，见 desktop.js 的同构实现）。

'use strict';

const fs = require('fs');
const path = require('path');
const { makeHashCache } = require('./util');

function register(app, { deployDir, mount }) {
  const hashFileCached = makeHashCache();
  const zipPath = path.join(deployDir, 'latest.zip');
  const manifestPath = path.join(deployDir, 'manifest.json');

  app.get(`${mount}/manifest.json`, (_req, res) => {
    let manifest;
    try {
      manifest = JSON.parse(fs.readFileSync(manifestPath, 'utf8'));
    } catch {
      return res.status(404).type('text/plain; charset=utf-8')
        .send(`更新清单尚未上传（运维需发布到 ${mount}/，见 release.sh）`);
    }
    const zipSha = hashFileCached(zipPath);
    if (zipSha) res.setHeader('X-Checksum-Sha256', zipSha);
    res.setHeader('Cache-Control', 'no-cache');
    res.json(manifest);
  });

  app.get(`${mount}/latest.zip`, (_req, res) => {
    let st;
    try {
      st = fs.statSync(zipPath);
    } catch {
      return res.status(404).type('text/plain; charset=utf-8')
        .send(`更新包尚未上传（运维需发布到 ${mount}/，见 release.sh）`);
    }
    res.setHeader('Content-Type', 'application/zip');
    res.setHeader('Content-Length', st.size);
    res.setHeader('Content-Disposition', 'attachment; filename="spiker-tauri-update.zip"');
    res.setHeader('X-Checksum-Sha256', hashFileCached(zipPath) || '');
    fs.createReadStream(zipPath).pipe(res);
  });
}

module.exports = { register };
