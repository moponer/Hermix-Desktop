# Decision Note: 文档附件字节上 wire，服务端落盘换路径

Status: implemented — 拖放 / 点击选择 / 粘贴 / 工作台"加入对话"四条非图片附件入口全链路打通（desktop-tauri 前端 + hermix-transport + hermix-server + hermix-agent）。

## Decision

非图片文档（PDF/DOCX/…）以**原始字节直传**进入后端，服务端落盘后以路径交给 agent：

1. 前端三入口（Composer 拖放、VisionButton 文件选择、AppMentionInput 粘贴）都汇聚到 `ChatView.handleFilesAttach`；非图片文件保持 `File` 句柄在 state（`QueuedFile`），**发送时**才 `arrayBuffer()` 读字节（镜像 images 通道的转换时机），单文件上限 30MB。
2. wire 上新增 `SendMessageRequest.attached_files: Option<Vec<FileAttachment>>`（`{ name, mimeType, bytes }`，item camelCase 镜像 `ImageAttachment`；顶层键 snake_case 与其他扩展字段一致），以及 `attached_file_paths: Option<Vec<String>>`（客户端已持有真实路径的场景）。
3. `hermix-server::attachments` 把字节写到 `<hermix_home>/attachments/<millis>-<i>-<sanitize(name)>`，与 sessions 同一 home 解析（便携模式同步生效）；写入失败的附件跳过并记日志，不拖垮整轮。文件名过 `sanitize_file_name`（控制字符与 `<>:"/\|?*` → `_`、去首尾点/空格、120 字符截断）——原始 name 只作展示与扩展名线索，**永不作磁盘名**，唯一性由 millis-index 前缀保证。
4. `AgentOverrides.attached_file_paths`（持久化路径 + 直传路径合并）进入 agent loop：用户消息追加 `[Attached files: … — read them with read_file / read_pdf]`。注记随用户消息持久化，reload 后历史仍显示附件。

## Root cause（为什么此前"全都没反应"）

- WebView2（以及一切浏览器）的 JS `File` 对象**没有**磁盘路径属性；旧代码 `(f as File & {path?: string}).path` 恒为 `undefined`，非图片文件在 `handleFilesAttach` 被静默丢弃。Tauri v2 也不提供 Electron 式 `webUtils.getPathForFile`（wry/tauri crate 中无 `path_for_file` IPC，已核实 2.12.1 npm 包与 wry 0.57 源码）。
- 即使拿到路径：`attachedFilePaths` 只是前端 `ChatOverrides` 里的死字段——`SendMessageRequest` 与 `AgentOverrides` 均无对应字段，后端 serde 直接忽略。图片正常是因为 images 的 bytes 通道前后端完整。
- 拖放事件本身没有坏：`dragDropEnabled: false` 已于 2026-07-27 配置（commit 4345c92e），wry 0.57 在无 drop handler 时不装 `DragDropController`，HTML5 拖放事件正常到达 DOM。

## Alternatives considered

- **Tauri 命令落盘（前端 invoke 写 `get_data_dir()`）** — 只修桌面端，浏览器 Dashboard 的拖放仍然死路；且 plugin-fs 的 scope 只放行 `$HOME`，便携模式数据目录在安装根，需另开权限。被否：bytes-on-wire 对所有客户端成立。
- **Tauri dialog 插件选文件拿路径** — 修得了"点击上传"，修不了拖放与粘贴（两者在浏览器语义下永远只有无路径的 `File`）。保留为可行补充但非本修。
- **重开 `dragDropEnabled: true` 用 `onDragDropEvent` 原生事件拿路径** — 事件携带路径但会杀死 HTML5 拖放，前端已有完整拖放 UI 与粘贴/选择器三入口统一汇聚，改事件模型收益为负。否。
- **base64 编码代替 number[]** — JSON 体积更省（~1.37x vs ~4x），但与 images 通道双格式并存徒增复杂度；30MB 上限下 number[] 内存尖峰可接受（本地 WS）。维持镜像 images。
- **agent 读内容内联注入上下文** — 违反零上下文膨胀（PDF >5 页走子代理是既定规则）；落盘后由 agent 自行调 read_file/read_pdf，读法交给 LLM 与系统提示。

## Consequences

- 落盘文件**无自动清理**：会话历史里的路径引用必须长期有效，故 attachments 目录随数据目录存活。磁盘占用与历史引用一致，是取舍不是遗漏。
- 30MB 单文件上限在 `ChatView` 常量处（`MAX_DOC_MB`），超限 toast 报错并丢弃该文件；WS 为本地回环，服务端不再设上限。
- `read_file` 的 credential guard 对 `<hermix_home>/attachments/` 无特判（不在 PROTECTED_DIR_NAMES），落盘附件可被 agent 正常读取；文件名 sanitize 保证不逃逸目录。
- 后续若要做"附件过期回收"，先解决历史会话消息中的路径失效问题，否则 reload 后历史引用断链。
