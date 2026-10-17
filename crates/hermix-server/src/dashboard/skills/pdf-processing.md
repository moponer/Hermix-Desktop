---
name: "pdf-processing"
display_name: "PDF 文件处理"
category: "productivity"
description: "PDF 的读、提取与文件级操作：文本型/扫描件分流、按页提取、合并、拆分。用于：'读一下这个 PDF''把这两份 PDF 合并''抽出第 3-10 页''提取这个 PDF 里的图'。不适用于：多份 PDF 的内容对比摘要（走 summarize-research）。"
version: "0.1.0"
author: "Hermix Desktop"
source: "builtin"
security_level: "safe"
tags: ["productivity", "builtin", "pdf", "文档处理"]
---

# PDF 文件处理

## 分流（第一步，判错全盘皆输）

1. **文本型 PDF**：`markitdown`（转 Markdown，token 最省）或 `read_pdf`
   （pdftotext/lopdf/pdfminer 三层 fallback）。输出干净 → 文本型。
2. **扫描件/图片型 PDF**：`markitdown` 输出为空或乱码 → 走 `read_image`
   （视觉模型读图，**在 `delegate_task` 内执行**，防止 base64 进主上下文）。
3. **判不清**：先 markitdown，空/乱再降级 read_image，顺序不可反。

## 工具接线

- **读取**：短文档（< 5 页）主上下文直接读；长文档（≥ 5 页）必须
  `delegate_task`，task 指定"读第 X-Y 页，返回结论不贴原文"。
- **合并**：`code_execution`（Python pypdf / lopdf）执行，输出新文件，
  不覆盖原件。
- **拆分/按页提取**：`code_execution` 按页范围拆；先报总页数再拆。
- **提取图片**：`code_execution`（pymupdf/pypdf 提取内嵌图）。
- **加密/解密**：加密用代码加密码；解密仅在用户提供密码时进行，
  无密码的加密件如实说明做不到，不硬解。

## 流程纪律

1. **先报页数与类型**：文本型/扫描型、总页数、是否加密，再问用户要哪部分。
2. **页码即证据**：内容类回答标注页码（"第 4 页第 2 条"），用户要能回查。
3. **长文档禁止整本进上下文**：逐段 delegate 汇总，单次读 ≤ 10 页。

## 反例（不要这样做）

- ❌ 扫描件用 markitdown 硬转然后报告"内容为空"——先分流到视觉模型
- ❌ 主上下文直接读 100 页 PDF（3 万+ token，永久占用会话）
- ❌ 合并/拆分直接覆盖原文件
- ❌ 编造没读到的页码或内容

## 输出契约

文件操作类：报告输入页数 → 操作 → 输出文件完整路径 + 输出页数。
内容类：结论 + 页码标注。遵循系统提示 REPORT_SECTIONING 分节规则。

## 参考

- anthropics 官方 pdf 技能：merge/split 脚本化处理范式
