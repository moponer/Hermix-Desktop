'use strict';

/**
 * `relayctl` —— 登记 / 查看 / 撤销参与者凭据。
 *
 * 凭据**明文只在这里出现一次**（登记时打印），之后库里只有 sha256 哈希，
 * 丢了只能重签。撤销立即生效：手机侧下次重连就被拒。
 *
 * 用法：
 *   node src/relayctl.js add-node  my-pc      [标签]
 *   node src/relayctl.js add-phone my-pc      [标签]
 *   node src/relayctl.js list      my-pc
 *   node src/relayctl.js revoke    <明文token>
 *
 * 环境变量：
 *   HERMIX_RELAY_DB  数据库路径（默认 ./relay.db，与 server.js 同一份）
 */

const { openRegistry } = require('./auth');

const DB_PATH = process.env.HERMIX_RELAY_DB || require('node:path').join(__dirname, '..', 'relay.db');

function usage(code = 1) {
  process.stderr.write(
    [
      '用法：',
      '  node src/relayctl.js add-node  <node_id> [标签]   签发一条电脑侧凭据',
      '  node src/relayctl.js add-phone <node_id> [标签]   签发一条手机侧凭据',
      '  node src/relayctl.js list      <node_id>          列出该 node 的凭据（只回哈希）',
      '  node src/relayctl.js revoke    <明文token>        立即撤销',
      '',
      `数据库：${DB_PATH}`,
      '',
    ].join('\n'),
  );
  process.exit(code);
}

function main(argv) {
  const [cmd, ...rest] = argv;
  if (!cmd) usage();

  const registry = openRegistry(DB_PATH);
  try {
    switch (cmd) {
      case 'add-node':
      case 'add-phone': {
        const [nodeId, label = ''] = rest;
        if (!nodeId) usage();
        const role = cmd === 'add-node' ? 'node' : 'phone';
        const token = registry.register({ nodeId, label, role });
        process.stdout.write(
          [
            `已签发 ${role} 凭据（node=${nodeId}）`,
            '',
            `  ${token}`,
            '',
            '⚠️ 明文只显示这一次。丢了只能重新签发。',
            role === 'node'
              ? '配置到桌面端 hermix 的 relay 配置里。'
              : '填进 Hermix M 屏 1 的 relay 地址配套设置里。',
            '',
          ].join('\n'),
        );
        break;
      }
      case 'list': {
        const [nodeId] = rest;
        if (!nodeId) usage();
        const rows = registry.list(nodeId);
        if (rows.length === 0) {
          process.stdout.write(`node=${nodeId} 下没有有效凭据。\n`);
          break;
        }
        process.stdout.write(`node=${nodeId}：\n`);
        for (const r of rows) {
          const when = new Date(r.created_at).toISOString();
          process.stdout.write(`  ${r.role.padEnd(5)} ${r.token_hash.slice(0, 16)}…  ${r.label || '(无标签)'}  ${when}\n`);
        }
        break;
      }
      case 'revoke': {
        const [token] = rest;
        if (!token) usage();
        const ok = registry.revokeToken(token);
        process.stdout.write(ok ? '已撤销。\n' : '撤销失败：token 不存在或已撤销。\n');
        process.exitCode = ok ? 0 : 2;
        break;
      }
      default:
        usage();
    }
  } finally {
    registry.close();
  }
}

if (require.main === module) {
  main(process.argv.slice(2));
}

module.exports = { main };
