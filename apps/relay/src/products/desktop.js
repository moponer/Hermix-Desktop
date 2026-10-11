//! desktop 产品路由（payload 型）：/desktop/manifest.json + /desktop/latest.zip
//! + /desktop/releases.json。
//!
//! 与旧实现的差别（决策见 docs/notes/implemented/architecture/
//! 2026-10-11-relay-structured-deployment.md §6）：
//! - **releases.json 是单一真相源**：manifest.json 不再是独立维护的文件，
//!   而是从 releases.json 最新条目**现算**——notes/sha256/版本号天然与
//!   zip 同批一致，不存在「zip 新了清单还是旧的」窗口期。
//! - **serve 时对账**：manifest 的 sha256 与 zip 实际哈希不一致 → 500 +
//!   日志。坏载荷（上传半截/错批）永远不会被客户端消费。
//! - 兼容：manifest 响应形状不变（version/notes/pub_date/url/sha256/
//!   min_version），老客户端零感知；releases.json 是新增文件。
//! - 若 releases.json 不存在（迁移期旧目录），回退读 manifest.json 文件，
//!   行为与旧实现一致——迁移不破坏现有更新链路。

'use strict';

const fs = require('fs');
const path = require('path');
const { makeHashCache } = require('./util');

function register(app, { deployDir, mount }) {
  const hashFileCached = makeHashCache();
  const zipPath = path.join(deployDir, 'latest.zip');
  const releasesPath = path.join(deployDir, 'releases.json');
  const legacyManifestPath = path.join(deployDir, 'manifest.json');

  /// 读 releases.json；不存在则 null（调用方回退 legacy manifest）。
  function readReleases() {
    try {
      return JSON.parse(fs.readFileSync(releasesPath, 'utf8'));
    } catch {
      return null;
    }
  }

  /// 从 releases.json 现算 manifest；迁移期无 releases.json 时回退读
  /// legacy manifest.json 文件（返回 null 表示两者都缺 → 404）。
  function currentManifest() {
    const releases = readReleases();
    if (Array.isArray(releases) && releases.length > 0) {
      const r = releases[0]; // 条目按版本倒序，[0] 即最新
      return {
        version: r.version,
        notes: Array.isArray(r.notes) ? r.notes.join('；') : r.notes || '本次更新包含修复与改进。',
        pub_date: r.date ? new Date(r.date).toISOString() : new Date().toISOString(),
        url: `${mount}/latest.zip`,
        sha256: r.sha256 || null,
        min_version: r.min_version ?? null,
      };
    }
    try {
      return JSON.parse(fs.readFileSync(legacyManifestPath, 'utf8'));
    } catch {
      return null;
    }
  }

  app.get(`${mount}/manifest.json`, (_req, res) => {
    const manifest = currentManifest();
    if (!manifest) {
      return res.status(404).type('text/plain; charset=utf-8')
        .send(`更新清单尚未上传（运维需发布到 ${mount}/，见 release.sh）`);
    }
    // serve 时对账：manifest 声称的 sha256 必须与 zip 实际哈希一致。
    const zipSha = hashFileCached(zipPath);
    if (zipSha && manifest.sha256 && zipSha !== manifest.sha256) {
      console.error(`[desktop] manifest sha256 与 zip 实际值不一致（manifest=${manifest.sha256} zip=${zipSha}）——载荷错批，拒发`);
      return res.status(500).type('text/plain; charset=utf-8')
        .send('更新载荷不一致（服务端对账失败），请稍后重试');
    }
    if (zipSha) res.setHeader('X-Checksum-Sha256', zipSha);
    res.setHeader('Cache-Control', 'no-cache');
    res.json(manifest);
  });

  app.get(`${mount}/releases.json`, (_req, res) => {
    const releases = readReleases();
    if (!releases) {
      return res.status(404).type('text/plain; charset=utf-8')
        .send('版本历史尚未生成（release.sh 发布时自动写入）');
    }
    res.setHeader('Cache-Control', 'no-cache');
    res.json(releases);
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
    res.setHeader('Content-Disposition', 'attachment; filename="hermix-desktop-update.zip"');
    res.setHeader('X-Checksum-Sha256', hashFileCached(zipPath) || '');
    fs.createReadStream(zipPath).pipe(res);
  });
}

module.exports = { register };
