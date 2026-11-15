import test from 'node:test';
import assert from 'node:assert/strict';
import { chatFrameHeader, chatSessionId } from '../core/session';
import { PROTO_VERSION } from '../core/envelope';

/**
 * 会话 id 与帧头 AAD——这两个字符串**不是**本文件定义的，是 Rust 侧定的：
 *
 * - 会话 id：PC 按 `session_id` 拆包（`relay_client.rs` 的 SessionMux），
 *   形状对不上就整条会话没有接收方。
 * - 帧头 AAD：`relay_proto.rs::Envelope::aad()` 的
 *   `format!("v={};tag={:?};sid={};nid={}", …)`，`{:?}` 打印变体名 `Route`。
 *
 * 任一处对不上，现象都是 **AES-GCM 认证失败**——没有错误码、没有重试，
 * 只是帧被悄悄丢掉。对不上时「消息莫名消失」比直接报错难查一个量级，
 * 所以这两条必须钉死。
 */
test('会话 id 与 PC 侧拆包格式一致', () => {
  assert.equal(chatSessionId('dev-1'), 'im:device:dev-1');
});

test('帧头 AAD 与 Rust Envelope::aad() 逐字节一致', () => {
  const sid = chatSessionId('dev-9');
  // Rust: format!("v={};tag={:?};sid={};nid={}", proto_version, tag, session_id, node_id)
  //       tag = FrameTag::Route → {:?} 打印 "Route"；手机发往 PC 的 envelope
  //       不带 node_id，PC 也按空串算 AAD。
  assert.equal(
    new TextDecoder().decode(chatFrameHeader(sid)),
    `v=${PROTO_VERSION};tag=Route;sid=${sid};nid=`,
  );
});

test('帧头不含协议头嵌套（配对侧同款坑的回归）', () => {
  // relayWsUrl 漏 https:// 分支时拼出过 "wss://https://…" ——AAD 这类
  // "前缀拼接" 的地方同一个错会静默失效，所以显式禁止前缀嵌套形态。
  const header = new TextDecoder().decode(chatFrameHeader(chatSessionId('dev-1')));
  assert.doesNotMatch(header, /;;/);
  assert.equal(header.split(';').length, 4);
});