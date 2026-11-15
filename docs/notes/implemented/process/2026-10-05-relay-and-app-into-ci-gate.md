# Decision Note: relay 与 Hermix M 接进 CI 门禁

Status: implemented — `ci-gate.sh` 新增 3b（relay 测试）与 4b（App 协议测试 + H5 构建）两道硬门禁

## 背景

[手机伴侣 App + 自建 Relay](../../proposed/feature/2026-10-04-mobile-relay-selfbuilt.md)
§实施顺序第 5 步写的是：

> 全量 `./scripts/ci-gate.sh`；App 与 relay 各接一条 CI（App 至少
> `./gradlew test`）。**不接受「本期只到本地构建」**——四屏 + 审批是本方案
> 交互风险最高的部分，零自动化验证等于把 review 压力全压到人工真机冒烟上。

前四步（P1 地基 / 对接层 / relay / App）都已落地，但**第 5 步没有**：
`scripts/ci-gate.sh` 的 11 道门禁里没有任何一道触碰 `apps/relay` 或
`apps/hm`。两个 Node 子项目此前只在本地手工跑过，CI 上等于不存在——笔记
明令禁止的状态一直挂着。

## Decision

新增两道**硬门禁**（失败即 exit 1），沿用脚本既有的 `3b` / `4b` 编号法
（此前只有 `7b` 一个先例），不重排 1–11：

- **`[3b/11] relay tests`** — 紧随 Rust 单测之后。relay 不是外部依赖，是本仓库
  内的服务，承担手机侧鉴权、帧路由与 SQLite 审计日志；它出 bug 的表现是所有
  已配对设备静默失联，没有任何其他门禁能覆盖。
- **`[4b/11] hermix-m 协议测试 + H5 构建`** — 紧随前端 tsc 之后。App 对每个请求
  做签名、对每条 relay 消息做分帧，两边都是对着协议规格黑盒写的
  （见 `signature.conformance.test.ts` 两侧同一固定向量互为佐证）。本机没有
  JDK / Android SDK / Gradle，`build:h5` 是这里唯一能跑的自动编译，因此
  **它自己就得是硬门禁**——模板语法错误不能有机会发到手机上。

两道门禁都在 `node_modules` 缺失时**明确报错退出**而不是静默 SKIP。已有的
第 4 步（`pnpm tsc`）同样是这个假设：先装依赖再跑门禁。静默跳过等于把
「没跑」和「跑过了」混为一谈，正是本篇要防的那类假通过。

`apps/hm` 补了 `tsx` 正式 devDependency 与 `test` 脚本。此前测试靠
`npx --yes tsx` 临时下载——门禁里不能有隐藏的网络依赖。

## Alternatives considered

**把 3b / 4b 做成软门禁（失败只警告）。** 与既有第 4 步（前端 tsc）保持一致
会更省事。但笔记已经写明「不接受只到本地构建」，而软门禁的实际效果就是
本地构建——CI 变绿、坏代码照进。选硬门禁。

**门禁里跑 `npm ci` 装依赖。** 更接近真实 CI 语义。但会让门禁从「几分钟」
变成「十几分钟」且强依赖网络可用性，而桌面端第 4 步早就假设了依赖已装。
保持一致：门禁只跑测试，装依赖交给开发者与真正的 CI pipeline。

**用 `pnpm workspace` 统一管理两个 Node 子项目。** 结构上更整齐，但要把
`apps/relay` / `apps/hm` 迁进 workspace 会动 `pnpm-workspace.yaml` 和锁文件，
与本篇「把缺失的门禁补上」的范围无关。等 App 和 relay 再加子项目时再谈。

**补 JDK / Android SDK 以跑 `gradlew test`。** 本机无这三者，无法验证，且
hermix 侧明文禁止引入跨平台分发相关依赖链。H5 构建已能覆盖模板与类型错误，
真机冒烟留待人工。

## Consequences

- **门禁变慢。** 新增两次 Node 启动 + 一次 uni 构建（约 +20–40s）。换取
  两个此前完全裸奔的子项目有回归保护。
- **`apps/hm/node_modules` 成为门禁前置条件。** 首次在干净检出上跑门禁的人
  会看到明确报错而不是神秘失败。
- **H5 构建产物 `apps/hm/dist/` 已被 `apps/hm/.gitignore` 覆盖**，不进版本库。
- **`tsx` 进入 `apps/hm` 的 devDependencies**，锁文件相应更新；这是对既有
  依赖面的小幅扩大，换掉一个隐式的 `npx --yes` 下载。
- **门禁的硬/软划分现在是 1, 2, 3, 3b, 4b, 6, 8, 9, 10, 11**，脚本头部注释
  已同步。
