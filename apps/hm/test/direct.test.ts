import test from 'node:test';
import assert from 'node:assert/strict';
import { parsePairingUrl, shortFingerprint } from '../core/direct';

/**
 * 解析桌面端 `QrPayload.pairing_url`。
 *
 * 那个 URL 的格式**不是本文件定的**，是 `pairing.rs:538` 拼的：
 *   `https://{host}:{port}/pair?c={code}&fp={fingerprint}`
 * 这里的向量按那条 format 串构造；改了解析器却没改 Rust 那边（或反过来），
 * 这套用例会红——那正是它存在的理由。
 */
const FP = 'a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90';
const GOOD = `https://192.168.1.7:8443/pair?c=123456&fp=${FP}`;

test('接受桌面端真实格式的配对 URL', () => {
  const t = parsePairingUrl(GOOD);
  assert.ok(t);
  assert.equal(t.baseUrl, 'https://192.168.1.7:8443');
  assert.equal(t.code, '123456');
  assert.equal(t.fingerprint, FP);
});

test('容忍首尾空白与指纹大写', () => {
  const t = parsePairingUrl(`  ${GOOD.replace(FP, FP.toUpperCase())}  `);
  assert.ok(t);
  assert.equal(t.fingerprint, FP);
});

test('path 带尾斜杠也认', () => {
  const t = parsePairingUrl(`https://10.0.0.2:8443/pair/?c=000000&fp=${FP}`);
  assert.ok(t);
  assert.equal(t.baseUrl, 'https://10.0.0.2:8443');
});

test('拒绝 http —— 配对依赖加密面，放行等于给中间人开门', () => {
  assert.equal(parsePairingUrl(GOOD.replace('https://', 'http://')), null);
});

test('拒绝非 /pair 路径', () => {
  assert.equal(parsePairingUrl(`https://192.168.1.7:8443/evil?c=123456&fp=${FP}`), null);
});

test('拒绝位数不对的码', () => {
  assert.equal(parsePairingUrl(`https://h:8443/pair?c=12345&fp=${FP}`), null);
  assert.equal(parsePairingUrl(`https://h:8443/pair?c=1234567&fp=${FP}`), null);
  assert.equal(parsePairingUrl(`https://h:8443/pair?fp=${FP}`), null);
});

test('拒绝缺指纹 / 指纹格式不对 —— 服务端会 403，提前拒掉更省一次往返', () => {
  assert.equal(parsePairingUrl('https://h:8443/pair?c=123456'), null);
  assert.equal(parsePairingUrl('https://h:8443/pair?c=123456&fp=short'), null);
  assert.equal(parsePairingUrl(`https://h:8443/pair?c=123456&fp=${'z'.repeat(64)}`), null);
});

test('拒绝完全不是 URL 的输入（扫到别的东西）', () => {
  assert.equal(parsePairingUrl('随便一段文字'), null);
  assert.equal(parsePairingUrl(''), null);
  // 桌面上另一个二维码：更新包地址。没有 code/fp，不该被当成配对码。
  assert.equal(parsePairingUrl('https://relay.example.com/desktop/latest.zip'), null);
});

test('指纹短显格式与桌面端设置页一致', () => {
  const shown = shortFingerprint(FP);
  assert.equal(shown, `${FP.slice(0, 16)}…${FP.slice(-4)}`);
  assert.ok(shown.length < FP.length);
});
