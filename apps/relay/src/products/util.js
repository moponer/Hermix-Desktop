//! products/ 共享工具：文件哈希（mtime 缓存）。
//! 从 server.js 抽出——desktop 与 spiker 两个 payload 型产品同构共用。

'use strict';

const fs = require('fs');
const crypto = require('crypto');

function hashFileSync(p) {
  const h = crypto.createHash('sha256');
  h.update(fs.readFileSync(p));
  return h.digest('hex');
}

/// 按文件 mtime 缓存 sha256——更新包一天下载不了几次，但每次请求
/// 重算大文件哈希会把 CPU 打满；mtime 变了才算新。
function makeHashCache() {
  const cache = new Map(); // path -> { mtimeMs, sha256 }
  return function hashFileCached(p) {
    let st;
    try {
      st = fs.statSync(p);
    } catch {
      return null;
    }
    const hit = cache.get(p);
    if (hit && hit.mtimeMs === st.mtimeMs) return hit.sha256;
    const sha256 = hashFileSync(p);
    cache.set(p, { mtimeMs: st.mtimeMs, sha256 });
    return sha256;
  };
}

module.exports = { hashFileSync, makeHashCache };
