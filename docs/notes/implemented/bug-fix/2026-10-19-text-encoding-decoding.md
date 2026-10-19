# Decision Note: 文本预览的编码判定归共享前端层

Status: implemented — 两端 Rust 只送原始字节；BOM / UTF-16 / GBK 的解码规则只有一份（`packages/preview/src/lib/textDecode.ts`），Windows 上的 UTF-16 与 GBK 文本不再乱码

关联：[共享预览层的样式归属与 Tailwind 边界](../architecture/2026-10-19-shared-preview-layer-ownership.md)、[右侧栏文档预览的格式保真重构](../feature/2026-10-13-preview-rendering-fidelity.md)

## Decision

**字节 → 文本的解码规则只有一份，住在共享前端层 `@hermix/preview` 里；Rust 端不再自己解码，只负责把原始字节送过来。**

两端的后端读文件命令都改成送 base64 的原始字节，文本 / 二进制的判定与编码解码统一由 `packages/preview/src/lib/textDecode.ts` 的 `decodeTextBytes` / `textResultFromBytes` 承担：

- viewer：`FileContent::Text` 的载荷从 `content: String` 改为 `data_base64: String`（`apps/viewer/src-tauri/src/commands.rs`）。此前是 `String::from_utf8_lossy` —— 非法字节被替换成 U+FFFD，中文正文整篇变成"��"。
- 桌面端：`ArtifactContent` 的载荷从 `kind: "text" | "binary" | "empty"` + `content: Option<String>` 改为 `kind: "bytes" | "empty"` + `data_base64: Option<String>`（`apps/desktop-tauri/src-tauri/src/commands.rs`）。此前是严格 `String::from_utf8`，解不出来就整份判成二进制 —— 同样一份 UTF-16 文件，桌面端显示"二进制文件"占位符，viewer 显示乱码。

解码判定顺序（先命中先返回）：

1. **BOM** → `utf-8` / `utf-16le` / `utf-16be`，BOM 本身被剥掉。
2. **无 BOM 的 UTF-16** → 靠奇偶位 NUL 分布嗅探（ASCII 文本在 LE 下 NUL 全落奇数位，BE 下全落偶数位）。
3. **含 NUL 字节**（前 4KB）→ 二进制。
4. **严格 UTF-8** → 文本。
5. **严格 GBK** → 文本。
6. **宽松 UTF-8** → 兜底，保证永远返回可渲染的字符串。

二进制判定单独走第 3 步的 NUL 嗅探，不靠"解不出来"——GBK 能解码的字节范围很宽，用它当二进制判据会把二进制文件显示成乱码而不是占位符。

触发这个改动的直接证据：用户在 `I:\` 下浏览的 `使用手册（简体中文中文）.txt` 首字节是 `FF FE`（UTF-16 LE 带 BOM），预览里整篇是乱码；同目录的 `Operation Manual.txt` 同类。

## Alternatives considered

**在两端 Rust 侧各写一份 BOM + GBK 嗅探。** 否决。那是两份平行复制：同一套判定规则要在两个进程、两种语言里各维护一次，改一处忘另一处就会出现"同一个文件在桌面端能看、在 viewer 里不能看"——这正是当前形态的翻版。共享前端层是唯一同时被两端消费的地方，规则放那里天然只一份。

**把解码放进 `hermix-core`（仓库里已有 `console_decode.rs` 处理 CP936 的控制台输出），让 viewer 依赖它。** 否决。viewer 是零安装的独立文件管理器（`apps/viewer` 的 `Cargo.toml` 里没有任何 `hermix-*` 依赖，刻意与主程序解耦），为了一条解码规则把 agent 核心 crate 拖进来不成比例；而且 `console_decode` 的场景是"进程 stdout"（OEM 代码页），与"磁盘文件"（BOM 优先）判据不同，硬塞进同一个模块会把两种语义混在一起。

**在 Rust 侧引入 `encoding_rs` 做 GBK。** 否决。Chromium 的 `TextDecoder` 自带 `gbk` / `gb18030` / `utf-16le` / `utf-16be`，前端侧零新依赖就能覆盖；Rust 侧引入编码 crate 只会多一份依赖与一份要同步的编码表。

**让前端把 Rust 已经解坏的字符串再解一次。** 否决。信息在 Rust 侧 `from_utf8_lossy` 那一刻就丢了（非法字节已被替换成 U+FFFD），前端无从恢复。要在前端解码，前端就必须拿到原始字节——这正是本次改 wire 协议的理由。

**只修 UTF-16，不修 GBK。** 讨论后一并修。GBK 与 UTF-16 是同一类问题的两个面（"Windows 上真实存在的文本编码不止 UTF-8"），只修一个等于让用户下一次在别的文件上再踩一次；多出的成本是第 5 步的五行。

**把桌面端"非 UTF-8 = 二进制占位符"的行为保留，只在 viewer 侧解。** 否决。两端对同一份文件给出不同结论（一边乱码、一边二进制）是契约不一致，不是端侧差异。

## Consequences

- **得到**：UTF-16 LE / UTF-16 BE（带或不带 BOM）、GBK、UTF-8（含 BOM）在两端都能正确显示；桌面端不再把 UTF-16 文本误判成二进制；两端对同一份文件的判定一致。
- **代价**：
  - **wire 协议变了**。viewer 的 `FileContent::Text.content` → `data_base64`；桌面端的 `ArtifactContent.kind` → `"bytes" | "empty"`、字段 → `dataBase64`。两端 Rust 与 TS 的读文件路径同步改（viewer `fileSource.ts`、桌面端 `artifactReader.ts` / `desktopBridge.ts`）。这是本次改动里唯一需要两端同步的接口。
  - viewer 的 `asReadBytes` 对文本类改为透传原始字节（此前是 `new TextEncoder().encode(c.content)`，等于把解坏的内容再洗一遍 UTF-8）。
  - UTF-32 文本文件判成二进制——现实中基本不存在，且 Chromium 没有 utf-32 解码器。
  - GBK 解码依赖运行时的编码表：Chromium / WebView2 自带；Node 在 small-icu 构建下没有 `gbk`，此时第 5 步回落为宽松 UTF-8（单测里对这种情况做了跳过）。
  - 桌面端 Rust 不再自己判"是不是文本"，Rust 侧只判"是不是空文件"；文本 / 二进制的判定完全交给前端层——这是刻意的：只有拿到字节流才能判 BOM。
- **测试**：`packages/preview/src/lib/textDecode.test.ts`（10 个用例）钉住 BOM / 无 BOM UTF-16 / NUL 嗅探 / GBK / 空文件 / 二进制各条路径。桌面端的 vitest 配置把 `node_modules/@hermix/preview/src/**/*.test.ts` 纳入了 include，所以共享包的用例跟着桌面端一起跑。
- **仍未收口**：没有门禁能发现"两端后端送出的载荷形状不一致"（比如某个 app 又开始自己解字符串）——本次靠两端共用同一个 `textResultFromBytes` 与 TypeScript 编译期类型收敛，但没有机械检查。
