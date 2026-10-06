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

## 工程布局（2026-10-06 转 HBuilderX 布局）

骨架全部在**项目根**：`manifest.json` / `pages.json` / `App.vue` / `main.js` /
`uni.scss` / `pages/` / `core/`。这是 HBuilderX 认得的经典 uni-app 结构——
之前的 CLI 布局（骨架在 `src/`）会让 HBuilderX「打开目录」时按普通 web 项目
处理并报"根目录缺少 index.html"。转换后 HBuilderX 直接打开本目录即可。

CLI 侧两条门禁仍然有效：

- `npm test`（42 项，tsx 直接跑 `core/*.ts`，不依赖布局）；
- `npm run build:h5` —— 通过 `cross-env UNI_INPUT_DIR=.` 指向根布局 +
  `index.html` 入口改指 `/main.js`。

## 打包（APK）

**app 目标只能由 HBuilderX 编译**。npm 工具链下 `dev:app` / `build:app`
在 bundling 阶段即失败（恒为 `"isInSSRComponentSetup" is not exported by
vue`，已实测 dcloud 4 个版本线 × vue 3.4.21/3.5.22 全部如此）——已发布的
任何 vue 都不带该导出，而每版 `@dcloudio/uni-app` 都 import 它。因此脚本里
不再保留 `dev:app` / `build:app`，避免给出跑不通的入口。

1. **AppID**：`manifest.json` 的 `appid` 当前是 `__UNI__0B255CA`（占位），
   **打包前换成你自己在 [DCloud 开发者中心](https://dev.dcloud.net.cn/)
   注册应用拿到的 AppID**（云打包必须绑定账号）。
2. HBuilderX「文件 → 打开目录」选**本目录**（`apps/hm`）——现在会被正确
   识别为 uni-app 项目。
3. 「运行 → 运行到手机或模拟器」做真机调试；首次运行需装 DCloud 的
   **自定义运行基座**。
4. 「发行 → 原生 App-云打包」→ Android → 证书先用**公测证书**（省去自备
   keystore）→ 下载 APK。

**装上 APK 后第一件事**：扫码配对 + 收第一帧加密消息——这是
`crypto.subtle` 在 app-plus runtime 可用性的唯一验证点（风险记录在
relay E2EE 决策笔记的 Consequences 一节）。

> 附：把源码转成 HBuilderX 布局后，`src/unpackage`（H5 构建产物）已随
> 布局删除，H5 输出统一在 `dist/`。
