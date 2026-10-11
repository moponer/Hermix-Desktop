//! 产品注册表——relay 上「挂了哪些产品」的唯一真相源。
//!
//! 一个条目 = 一个产品。加产品 = 这里加一行 +（payload 型）products/ 下
//! 一个同构模块；server.js 与本文件之外**不允许出现任何产品名**。
//!
//! type 语义：
//! - payload：更新载荷型产品。deploy/<name>/current/ 下放 manifest.json、
//!   releases.json、latest.zip，由 products/<name>.js 负责路由与现算校验。
//! - proxy：反代型产品。relay 把 <mount>/* 原样转发到 target（本机端口），
//!   不感知其内部协议；health 用于 /health 汇总探针。
//!
//! deployDir 相对 relay 工作目录（远端 = /www/wwwroot/hermix-relay）。

'use strict';

const PRODUCTS = [
  {
    name: 'desktop',
    mount: '/desktop',
    type: 'payload',
    deployDir: 'deploy/desktop/current',
  },
  {
    name: 'spiker',
    mount: '/spiker',
    type: 'payload',
    deployDir: 'deploy/spiker/current',
  },
  {
    name: 'bbs',
    mount: '/bbs',
    type: 'proxy',
    target: 'http://127.0.0.1:3901',
    health: '/bbs/health',
  },
];

function payloadProducts() {
  return PRODUCTS.filter((p) => p.type === 'payload');
}

function proxyProducts() {
  return PRODUCTS.filter((p) => p.type === 'proxy');
}

module.exports = { PRODUCTS, payloadProducts, proxyProducts };
