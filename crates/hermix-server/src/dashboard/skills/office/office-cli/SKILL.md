---
name: "office-cli"
display_name: "OfficeCLI 文档处理"
category: "office"
description: "用于：AI 智能体创建、读取、编辑 Word/Excel/PPT 文件。OfficeCLI 是 Hermix Desktop 内置的 Office 命令行套件，无需安装 Office。"
version: "0.1.0"
author: "Hermes Desktop"
source: "builtin"
security_level: "safe"
tags: ["office", "builtin"]
---

# officecli

AI-friendly CLI for .docx, .xlsx, .pptx. Single binary, no dependencies, no Office installation needed.

## 何时激活

当智能体需要**创建、读取或编辑** Office 文档（.docx / .xlsx / .pptx）时启用本技能，并调用内置 `office_cli` 工具执行。典型场景：

- 生成报告、合同、简历等 Word 文档
- 生成或更新表格、预算、数据透视表等 Excel 文档
- 制作演示文稿、幻灯片
- 在预览界面渲染或改写已有 Office 文档

## Install

The `officecli` binary is bundled with Hermix Desktop. Verify with:

```bash
officecli --version
```

If not found, the binary is at `<app_data>/binaries/officecli-x86_64-pc-windows-msvc.exe`.

---

## 核心概念

**L1 (read) → L2 (DOM edit) → L3 (raw XML)**. Always prefer higher layers. Add `--json` for structured output.

---

## Help System

**When unsure about property names, value formats, or command syntax, ALWAYS run help instead of guessing.**

```bash
officecli help                                  # All commands + global options
officecli help docx                             # List all docx elements
officecli help docx paragraph                   # Full schema: properties, aliases, examples
officecli help docx set paragraph               # Verb-filtered: only props usable with `set`
```

Format aliases: `word`→`docx`, `excel`→`xlsx`, `ppt`/`powerpoint`→`pptx`.

---

## Performance: Resident Mode

Every command auto-starts a resident on first access (60s idle timeout). Explicit `open`/`close` for longer sessions:

```bash
officecli open report.docx       # keep in memory
officecli set report.docx ...    # no file I/O overhead
officecli close report.docx      # save and release
```

---

## 实践指南

**PPT:**
```bash
officecli create slides.pptx
officecli add slides.pptx / --type slide --prop title="Q4 Report" --prop background=1A1A2E
officecli add slides.pptx '/slide[1]' --type shape --prop text="Revenue grew 25%" --prop x=2cm --prop y=5cm --prop font=Arial --prop size=24 --prop color=FFFFFF
```

**Word:**
```bash
officecli create report.docx
officecli add report.docx /body --type paragraph --prop text="Executive Summary" --prop style=Heading1
officecli add report.docx /body --type paragraph --prop text="Revenue increased by 25% year-over-year."
```

**Excel:**
```bash
officecli create data.xlsx
officecli set data.xlsx /Sheet1/A1 --prop value="Name" --prop bold=true
officecli set data.xlsx /Sheet1/A2 --prop value="Alice"
```

---

## L1: Create, Read & Inspect

```bash
officecli create <file>               # Create blank .docx/.xlsx/.pptx
officecli view <file> <mode>          # outline | stats | issues | text | annotated | html
officecli get <file> <path> --depth N # Get a node and its children [--json]
officecli query <file> <selector>     # CSS-like query
officecli validate <file>             # Validate against OpenXML schema
```

### view modes

| Mode | Description |
|------|-------------|
| `outline` | Document structure |
| `stats` | Statistics (pages, words, shapes) |
| `issues` | Formatting/content/structure problems |
| `text` | Plain text extraction |
| `annotated` | Text with formatting annotations |
| `html` | Static HTML snapshot |
| `screenshot` | PNG via headless browser |

---

## L2: DOM Operations

### set — modify properties

```bash
officecli set <file> <path> --prop key=value [--prop ...]
```

**Value formats:**

| Type | Format | Examples |
|------|--------|---------|
| Colors | Hex, named, RGB, theme | `FF0000`, `#FF0000`, `red`, `rgb(255,0,0)`, `accent1` |
| Spacing | Unit-qualified | `12pt`, `0.5cm`, `1.5x`, `150%` |
| Dimensions | EMU or suffixed | `914400`, `2.54cm`, `1in`, `72pt`, `96px` |

### add — add elements

```bash
officecli add <file> <parent> --type <type> [--prop ...]
officecli add <file> <parent> --type <type> --after <path> [--prop ...]
officecli add <file> <parent> --type <type> --before <path> [--prop ...]
officecli add <file> <parent> --from <path>   # clone existing element
```

**Element types:**

| Format | Types |
|--------|-------|
| **pptx** | slide, shape, picture, chart, table, row, connector, group, video, audio, equation, notes, comment, animation, transition, paragraph, run, zoom, placeholder, model3d |
| **docx** | paragraph, run, table, row, cell, image, header, footer, section, bookmark, comment, footnote, chart, equation, field, hyperlink, style, toc, watermark, textbox, shape |
| **xlsx** | sheet, row, col, cell, chart, image, comment, table, namedrange, pivottable, sparkline, validation, autofilter, shape, textbox |

### batch — multiple operations in one save cycle

```bash
officecli batch data.xlsx --input updates.json --force --json
```

---

## L3: Raw XML

Use when L2 cannot express what you need.

```bash
officecli raw <file> <part>                          # view raw XML
officecli raw-set <file> <part> --xpath "..." --action replace --xml '<w:p>...</w:p>'
```

---

## 注意事项

| Pitfall | Correct Approach |
|---------|-----------------|
| `--name "foo"` | Use `--prop name="foo"` |
| Unquoted `[N]` in shell | Always quote: `'/slide[1]'` |
| Guessing property names | Run `officecli help <format> <element>` |
| `\n` in shell strings | Use `\\n` for newlines |
| `$` in shell text | Use single quotes: `--prop text='$15M'` |

---

## Notes

- Paths are **1-based**: `'/body/p[3]'` = third paragraph
- `--index` is **0-based**: `--index 0` = first position
- After modifications, verify with `validate` and/or `view issues`
- **When unsure**, run `officecli help <format> <element>` instead of guessing

## 集成

- 后端通过内置工具 `office_cli` 执行所有命令（在 `register_builtins.rs` 注册，`essential::ESSENTIAL_TOOLSETS["office"]` 依赖）。
- 桌面端预览：后端 `render_office_preview` 命令 shell out 到 `officecli` 二进制；前端 `RightSidebar.tsx` / `SpreadsheetView.tsx` 渲染 docx/xlsx/pptx 预览。
- LLM 调用方式：本技能向模型描述可用命令，模型生成 `office_cli` 工具调用参数（命令 + 文件路径 + `--prop`）。

## 参考

- 命令行帮助（最权威的属性名/取值参考）：`officecli help`、`officecli help docx`、`officecli help docx paragraph`
- OpenXML 规范与文档结构见 `officecli` 自带 help 输出
- 二进制位置：`<app_data>/binaries/officecli-x86_64-pc-windows-msvc.exe`（随 Hermix Desktop 捆绑）
