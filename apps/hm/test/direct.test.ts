import test from 'node:test';
import assert from 'node:assert/strict';
import { pairingFrameHeader, parsePairingUrl, PAIRING_SESSION_ID } from '../core/direct';
import { PROTO_VERSION } from '../core/envelope';

/**
 * 解析桌面端 `QrPayload.pairing_url`。
 *
 * 那个 URL 的格式**不是本文件定的**，是 `pairing.rs::create` 拼的：
 *   `{relay}/pair?c={code}&t={phone_token}&k={pc_public_key}`
 * 这里的向量按那条 format 串构造；改了解析器却没改 Rust 那边（或反过来），
 * 这套用例会红——那正是它存在的理由。
 */
const K = 'BJ2fHc9WqXvO1pQzR4sT8uWyB3nM6kL0aC5dE7fG9hI1jK3lM5nP7qR9sT1uV3wX5yZ7bB2dD4fF6hH8jJ='; // 88+ 字符形态
const TOKEN = 'hrel_0123456789abcdef';
const GOOD = `https://relay.example.com/pair?c=123456&t=${TOKEN}&k=${K}`;

test('接受桌面端真实格式的配对 URL', () => {
  const t = parsePairingUrl(GOOD);
  assert.ok(t);
  assert.equal(t.relayBase, 'https://relay.example.com');
  assert.equal(t.code, '123456');
  assert.equal(t.phoneToken, TOKEN);
  assert.equal(t.pcPublicKey, K);
});

test('容忍首尾空白与尾斜杠', () => {
  const t = parsePairingUrl(`  https://10.0.0.2:8443/pair/?c=000000&t=${TOKEN}&k=${K}  `);
  assert.ok(t);
  assert.equal(t.relayBase, 'https://10.0.0.2:8443');
});

test('wss 形态也认（relay 配置允许 wss://）', () => {
  const t = parsePairingUrl(`wss://relay.example.com/pair?c=000000&t=${TOKEN}&k=${K}`);
  assert.ok(t);
  assert.equal(t.relayBase, 'wss://relay.example.com');
});

test('拒绝 http —— 凭据与密封帧不该裸奔在明文信道上', () => {
  assert.equal(parsePairingUrl(GOOD.replace('https://', 'http://')), null);
});

test('拒绝非 /pair 路径', () => {
  assert.equal(parsePairingUrl(`https://relay.example.com/evil?c=123456&t=${TOKEN}&k=${K}`), null);
});

test('拒绝位数不对的码', () => {
  assert.equal(parsePairingUrl(`https://h/pair?c=12345&t=${TOKEN}&k=${K}`), null);
  assert.equal(parsePairingUrl(`https://h/pair?c=1234567&t=${TOKEN}&k=${K}`), null);
  assert.equal(parsePairingUrl(`https://h/pair?t=${TOKEN}&k=${K}`), null);
});

test('拒绝缺凭据 / 凭据格式不对 —— 连 relay 的握手头靠它', () => {
  assert.equal(parsePairingUrl(`https://h/pair?c=123456&k=${K}`), null);
  assert.equal(parsePairingUrl('https://h/pair?c=123456&t=not-hrel&k=' + K), null);
});

test('拒绝缺 PC 公钥 —— 没它配对请求封不出去（relay 拒收明文帧）', () => {
  assert.equal(parsePairingUrl(`https://h/pair?c=123456&t=${TOKEN}`), null);
  assert.equal(parsePairingUrl(`https://h/pair?c=123456&t=${TOKEN}&k=not-base64!`), null);
});

test('拒绝完全不是 URL 的输入（扫到别的东西）', () => {
  assert.equal(parsePairingUrl('随便一段文字'), null);
  assert.equal(parsePairingUrl(''), null);
  // 桌面上另一个二维码：更新包地址。没有 code/t，不该被当成配对码。
  assert.equal(parsePairingUrl('https://relay.example.com/desktop/latest.zip'), null);
});

test('配对帧 AAD 与 Rust Envelope::aad() 逐字节一致', () => {
  // Rust: format!("v={};tag={:?};sid={};nid={}", 2, FrameTag::Route, sid, "")
  //       {:?} 打印枚举变体名 "Route"；nid 恒为空串（配对阶段互不知身份）。
  assert.equal(
    new TextDecoder().decode(pairingFrameHeader()),
    `v=${PROTO_VERSION};tag=Route;sid=${PAIRING_SESSION_ID};nid=`,
  );
});
