import test from 'node:test';
import assert from 'node:assert/strict';
import { BROWSER_UNSUPPORTED_HINT, isBrowserEnv, relayWsUrl } from '../core/relayClient';

/**
 * relay WS 地址拼接：三种输入写法都要落到同一个可用地址上。
 *
 * 用户在桌面端填的是 `https://…`，而 WS 握手走 `wss://`。按字面当 http 用
 * 会直接握手失败，而现象是「填了地址连不上」——指向错误的方向。
 */
test('relay 地址拼接受输入写法影响', () => {
  assert.equal(relayWsUrl('https://relay.example.com', 'phone'), 'wss://relay.example.com/relay/phone');
  assert.equal(relayWsUrl('https://relay.example.com/', 'phone'), 'wss://relay.example.com/relay/phone');
  assert.equal(relayWsUrl('wss://relay.example.com', 'node'), 'wss://relay.example.com/relay/node');
  assert.equal(relayWsUrl('http://127.0.0.1:8787', 'node'), 'ws://127.0.0.1:8787/relay/node');
  // 裸 host 默认补 wss：外网默认走 TLS，不给「明文上公网」留口子。
  assert.equal(relayWsUrl('relay.example.com', 'phone'), 'wss://relay.example.com/relay/phone');
});

/**
 * 环境识别：relay 凭据只走 HTTP 握手头，浏览器的 WebSocket 带不了自定义头，
 * 于是 H5 下**必然**被 401，且浏览器把状态码吞成通用错误。
 *
 * 不识别这个环境，报出来的就是「连不上中转，确认手机能上网」——网络好好的，
 * 用户却被引去查错方向。所以这两个分支都要钉死。
 */
test('浏览器环境被识别出来，App 端不被误判', () => {
  const g = globalThis as { document?: unknown; window?: unknown };
  const hadDocument = 'document' in g;
  const hadWindow = 'window' in g;
  const doc = g.document;
  const win = g.window;
  try {
    // Node（= App 端 V8 的等价物：没有 DOM 全局）→ 不是浏览器
    assert.equal(isBrowserEnv(), false, '无 DOM 全局时不得误判为浏览器');

    g.document = {};
    g.window = {};
    assert.equal(isBrowserEnv(), true, '有 DOM 全局时必须识别为浏览器');

    // 半个不算：H5 一定有 window，App 端一定两个都没有。
    delete g.window;
    assert.equal(isBrowserEnv(), false, '只有 document 不能判定为浏览器');
  } finally {
    if (hadDocument) g.document = doc;
    else delete g.document;
    if (hadWindow) g.window = win;
    else delete g.window;
  }
});

test('浏览器不可用的提示说清「不是网络问题」', () => {
  // 用户是被这句文案引去查网络的，所以它必须自带否定，否则等于把人送进死路。
  assert.match(BROWSER_UNSUPPORTED_HINT, /App/);
  assert.match(BROWSER_UNSUPPORTED_HINT, /不是网络问题/);
});