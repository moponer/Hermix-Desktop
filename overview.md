# 自定义应用模块体检 + 一次性交付加固

## 体检结论：机制本就健康
- **工具发现**：`custom_app_manager` 注册在 `custom_app` toolset（HARD essential 强制启用），LLM 永不"看不到工具"。
- **提示词触发**：`SkillLoader::match_message` 走 CJK tag 全词 + bigram 命中，`run_loop.rs` 每轮 pre-turn 注入 skill。原 tags 偏弱（缺"软件/开发一个/写个应用"等口语）→ 已补强。
- **冒烟测试**：`smoke` action 先 `package_plugin` 再启动 exe，自打包，无"未找到产物不适用"。
- **运行语义**：原 `run` 走打包单 exe，与 Desktop"运行"按钮(dev)不一致 → 已统一为测试版。

## 落地改动（cargo check + 测试全绿）
1. **validate.rs — L3 前端质量关卡**（提交 `d579224`）
   - error：CDN 外链（unpkg/jsdelivr/cdn.tailwindcss 等）、`input file`/`webkitdirectory`
   - warning：缺自定义标题栏、custom scheme fetch、emoji
   - 交付前强制离线可用、无蓝紫/emoji、带标题栏。

2. **custom_app_manager.rs**（提交 `b4a765d`）
   - `run` action 改 `run_dev_plugin`（dev 测试版，hx-shell --html 加载，与 Desktop 运行按钮一致）；返回 dev_html 路径 + 运行/打包/更新提示。
   - 清理 4 处过时 "tail-inject" 描述 → PE 资源节(RT_RCDATA)嵌入。
   - tool schema 强化触发语义（用户说做/开发/写个 工具/应用/软件 直接判定）+ 一次性到位交付清单。

3. **custom-app-development.md skill**（提交 `b4a765d`）
   - description/tags 强化触发词（软件/开发一个/写个应用/小软件…）。
   - 运行时/应用模型"尾部注入"→PE 资源节。
   - agent 开发流改为 7 步一次性到位：识别→分析前后端→写→`validate` 循环到 ok→`run` 测试版→`smoke`→交付（去哪运行 / 如何打包 / 如何回来 update 继续）。

## 验证
- `cargo check -p hermix-plugin -p hermix-tools`：通过
- `cargo test -p hermix-plugin validate`：11 passed
- `cargo test -p hermix-tools`：653 passed / 3 ignored

## 一次性到位交付闭环
用户:"做个/开发一个/写个 工具/应用/软件" → skill 直接命中 → agent 走 7 步 → validate 卡死 error 直到 ok → run 测试版即时预览 → 交付说明（运行入口 / package 出单 exe / update 回来迭代）。
