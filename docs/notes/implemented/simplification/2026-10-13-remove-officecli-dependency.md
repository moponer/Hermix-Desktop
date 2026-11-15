# Decision Note: 移除 officecli 外部依赖

Status: implemented — 仓库与出包链路中已无任何 officecli 痕迹（二进制、工具、技能、预览路径）

## Decision

officecli（上游 iOfficeAI/OfficeCli 预编译二进制，32 MB，从未进 git）从项目中
彻底移除。一并删除所有依赖它的产品代码：

- `hermix-tools` 的 `create_presentation` 工具与 `ppt_themes` 主题表——它把 JSON
  规格翻译成 officecli 命令序列，没有 officecli 就没有实现路径
- 内置技能 `office/office-cli` 与 `builtin-skills/hermix/ppt-design`——两者都在
  教模型调用已不存在的工具
- 桌面端 `render_office_preview` 命令、`src-tauri/src/officecli.rs`、`build.rs`
  的存在性断言、`tauri.conf.json` 的 externalBin 条目、`release.sh` /
  `check-sidecar-freshness.sh` 的打包项、更新脚本的进程回收名单
- 前端预览的 `office` 载荷种类：`PreviewKind`、`officeLoader`、`PreviewSurface`
  分支与 `.preview-office*` 样式

老二进制 Office 格式（doc / ppt）的预览改走 markitdown 的文本表示：`resolveKind`
对 `legacy` 分组直接返回 `markdown`，不再有版式视图。

同时撤销"把 officecli 接成内置 MCP server 以吃满其 28 个命令"的方案——能力再全
也是外部二进制，与 zero-install 自包含的目标对冲。

## Alternatives considered

| 方案 | 为何否决 |
|---|---|
| 保留 officecli，只把两个工具名统一成一个（`office__officecli`） | 只治名字不治依赖。要求移除的是依赖本身，不是重命名 |
| 接 officecli 的 `mcp` 子命令为内置 MCP server，一次性拿到全部命令 | 能力面最广，但把外部二进制集成得更深，与去掉外部依赖的方向相反；且二进制不在 git 里，干净 checkout 上能力凭空消失 |
| 保留 officecli 仅作老格式版式预览，只删 PPT 生成 | 留下 32 MB 二进制和一条外部进程路径，换来的只是 doc / ppt 的版面还原；markitdown 文本表示已覆盖"能看到内容"这一底线需求 |
| 用 python-pptx / 自研 docx 写入替代 `create_presentation` | 重写成本远高于收益，且 PPT 生成无实际使用记录；属于典型的"为以后预留"脚手架 |

## Consequences

- **PPT 生成能力消失**。`create_presentation` 是唯一消费者，没有替代实现。
- **老二进制 Office 不再有版式视图**。`doc` / `ppt` 只能看 markitdown 抽取的
  文本——表格退化、图片丢失。`.xls` 不受影响（SheetJS 读 BIFF，仍走前端表格视图）。
- **markitdown 仍是外部 sidecar**。这次移除的是 officecli，不是"外部二进制"
  这一整类依赖；markitdown 继续承担文档→Markdown 转换。
- 出包体积减少约 32 MB。
- `essential::SOFT_ESSENTIAL_TOOLSETS` 去掉 `office` 条目——没有工具再带这个标签。

## Reintroduction conditions

出现以下任一证据时，应重新评估引入 Office 写入能力：

1. **有真实需求**：用户明确提出生成或编辑 docx / pptx，且 markitdown 的只读
   抽取无法满足。
2. **有自包含实现路径**：能用本仓库的 Rust crate（或体积可接受、可进 git 的
   资产）完成写入，不再依赖手工 `cp` 的第三方二进制。
3. **版式保真成为硬要求**：老二进制 Office 格式的版面还原从锦上添花变成验收
   标准，且纯前端方案确认无解。

重新引入时**不得**回到"手工 cp 二进制 + build.rs 存在性断言"的形态——历史上该
二进制曾在清理 `binaries/` 时被误删并导致 build.rs panic。
