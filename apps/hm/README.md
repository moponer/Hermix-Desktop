# Hermix M

hermix 的手机伴侣端。**薄客户端**：agent 引擎在 PC，手机只做入口、审批与呈现。

## 为什么是 uni-app 而不是原生

本仓库**没有 JDK / Android SDK / Gradle**，原生路线的代码一行都验证不了，
交付的是"看起来对"的东西。uni-app 的 **H5 目标能用 Node 构建验证**
（`npm run build:h5`），逻辑层因此有一道真门禁。附带 iOS 顺带有了。

代价（如实记）：不是原生 M3 组件，而是"用 CSS 变量实现得很像 M3"——色值
逐个取自 `apps/desktop-tauri/src/styles/themes/dark.css`，抬升顺序
`#101113 < #161719 < #1d1f22 < #24262a < #2e3136` 与主题文件那一行一致。
Android / iOS 打包仍需各自原生 SDK，不在 CI 的 H5 路径上。

## 验证

```bash
npm install
npm run build:h5                                   # 逻辑层真门禁
npx tsx --test test/signature.conformance.test.ts  # 签名跨语言契约
```

签名那条**不是 round-trip**。它比的是 TS 侧算出的值与 Rust 侧固定向量
（`crates/hermix-server/src/request_signature.rs::cross_language_signature_vector`），
因为 round-trip 只证明"自己算的能被自己验"，两边一起错照样绿。

> 这条测试上线第一天就抓到真问题：Rust 的 `compute_signature` 原本是
> `SHA256(token ‖ 标签 ‖ message)` 这个**手搓构造**，而文档注释写的是 HMAC。
> 按文档实现客户端的人做出的签名一律被拒（现象是"配对成功但全部 401"），
> 而且那个构造是**长度扩展攻击**的标准靶子。已改为真 HMAC-SHA256。

## 五个界面

| | 位置 | 关键取舍 |
|---|---|---|
| 配对 | `pages/pairing` | 指纹核对在**输码之前**；时钟偏差当场报 |
| 对话 | `pages/chat` | 长按说话（不做"说完自动发"） |
| 审批 | `pages/approval` | **只有两键**，没有"永久允许" |
| 任务 | `pages/tasks` | 每个排队项都标位次；不主动推送 |
| 通话 | `pages/call` | 第五个**模式**；所有审批要视觉确认 |

三条"刻意不做"写在各页注释里，别当成没做完的补回去。

## 未决项（不是已解决的问题）

**凭据落盘。** `src/core/pairing.ts` 用 `uni.setStorageSync`（**明文**）存
`hmxd_*` 设备 token 与 TTS/ASR 的 API key——uni-app 的 JS 层拿不到 Android
Keystore（`EncryptedSharedPreferences` 是原生能力）。token 明文泄漏 =
任意远程会话 + 任意工具授权。三个待选：UTS 原生插件透出 Keystore /
凭据不落手机由 PC 代调 / 接受明文（不可接受）。

**指纹是自证的。** H5 没有读 TLS 证书的标准 API，屏 1 的指纹只能来自 relay
自己声明的 `X-Hermix-Cert-Sha256` 响应头。除非 relay 与客户端做公钥固定
（pinning），否则那个指纹**没有安全价值**。原生壳要用原生 TLS 校验取真实证书。

**流式 ASR / TTS 尚未接线。** 各页 `TODO(...)` 标了接入点。按已定方向：
ASR 与 TTS 都在**手机端**直连云端（用户自带 API key），音频与文本都不过 relay，
relay 架构一行不改。

## 打包（APK）——现状与路径（2026-10-06 实测矩阵）

**`npm run build:app` 在纯 npm 工具链下无解**，且已排除"版本线配平"这一解释：

| dcloud 版本 | vue 版本 | `build:app` | `build:h5` / `npm test` |
|---|---|---|---|
| 3.0.0-4060620250520001（当前 pin，预览线） | 3.4.21 / 3.5.22 | ✗ | ✓ |
| 3.0.0-4080720251210001（最新正式线） | 3.4.21 | ✗ | ✓ |
| 3.0.0-4040520250106001 / 3.0.0-4030320241128001 | 3.4.21 | ✗ | — |

失败信息恒为：

```
"isInSSRComponentSetup" is not exported by "vue/dist/vue.runtime.esm-bundler.js"
（来自 @dcloudio/uni-app/dist/uni-app.es.js）
```

已逐版本核实：**已发布的任何 vue（3.4.21 / 3.5.13 / 3.5.22）的全部 dist
构建都不导出 `isInSSRComponentSetup`**，而每一版已发布的 `@dcloudio/uni-app`
都 import 它。**结论：app 目标必须经 HBuilderX 的运行时**（它自带配平好的
uni-app + vue 工具链），npm 侧无论怎么配版本都编不出来。H5 与 `npm test`
（tsx，不走 bundler）都正常——所以逻辑层有真门禁，缺的只是原生打包链路。

**HBuilderX 里的两个坑**：

1. **直接「打开目录」选 `apps/hm` 会报"请确认您的项目模板是否支持vue3：
   根目录缺少 index.html"**——误导性报错。本项目是 **CLI 布局**
   （`manifest.json` / `pages.json` 在 `src/`，`index.html` + `vite.config.ts`
   在根），HBuilderX 按根目录 `manifest.json` 判定项目类型，认不出就退回
   普通 web 分支。**真机调试的正确姿势是命令行**：`npm run dev:app`
   （uni CLI 会唤起 HBuilderX 并在其运行时里跑）。
2. **APK 云打包需要 AppID**：uni-app 原生 App 必须先在 DCloud 开发者中心
   注册应用取 AppID，填进 `src/manifest.json` 的 `appid`（当前值
   `__UNI__0B255CA` 是既有占位/他人注册，**打包前换成你自己的**），
   然后在 HBuilderX 内完成发行 → 原生 App-云打包 → Android。
   若 CLI 无法产出 `dist/build/app`，则需要把工程转成 HBuilderX 布局
   （`manifest.json` / `pages.json` / `App.vue` / `main.js` 移到根目录）
   ——属于工程形态变更，动手前先与该项目的当前负责人对齐。

**装上 APK 后第一件事**：扫码配对 + 收第一帧加密消息——这是
`crypto.subtle` 在 app-plus runtime 可用性的唯一验证点（风险已记录在
relay E2EE 决策笔记的 Consequences 一节）。
