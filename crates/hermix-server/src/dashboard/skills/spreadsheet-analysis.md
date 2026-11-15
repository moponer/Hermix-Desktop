---
name: "spreadsheet-analysis"
display_name: "表格数据处理"
category: "productivity"
description: "对 Excel/CSV 做数据活：合并、透视、清洗、校验、生成新表，产出处理后的 .xlsx/.csv 文件而非口头描述。用于：'把这两个表合并''做个透视''清洗脏数据''生成月度汇总表'。不适用于：把数据整理成可读报告排版（走 document-authoring 画布表格）。"
version: "0.1.0"
author: "Hermix Desktop"
source: "builtin"
security_level: "safe"
tags: ["productivity", "builtin", "excel", "xlsx", "csv", "数据", "表格"]
---

# 表格数据处理

## 工具接线

1. **读结构**：`markitdown` 转 Markdown 看列名 + 前 5 行，确认 schema；
   大文件只转换一次，后续用 `code_execution` 直接处理原文件。
2. **处理**：`code_execution`（Python）跑 pandas / openpyxl 脚本。
   用**代码改数据**，不用 `write_file` 手拼 CSV——手拼丢格式、丢类型、
   丢公式。
3. **产出**：写回同目录新文件（如 `汇总-2026-10.xlsx`），`reply` 中给
   完整路径。原始文件不覆盖——用户没让覆盖就不覆盖。
4. **校验**：写完用代码读回抽查：行数对账（输入 N 行 → 输出 M 行 + 原因）、
   随机抽 3 行人工核对关键列、数值列合计对账。

## 流程纪律

1. **先报 schema 再动手**：列名、行数、数据类型、发现的异常（重复行/空列/
   混合类型）先告诉用户，确认理解一致再处理。列语义拿不准（"这个'金额'
   含税吗"）先问，不猜。
2. **改动可追溯**：清洗类操作（去重/填空/格式修正）逐项报告"改了多少行、
   规则是什么"，禁止静默改数据。
3. **类型保持**：日期是日期、金额保留小数位、编号保留前导零
   （用 dtype=text 处理，防 Excel 吞零）。
4. **公式 vs 值**：用户要"可继续编辑的表"时写公式（openpyxl），要"定稿
   数据"时写值。拿不准写值 + 说明。

## 反例（不要这样做）

- ❌ 手写 CSV 字符串当 xlsx 交付——格式/类型全丢
- ❌ 静默去重或填空——每一处数据变更都要报告
- ❌ 处理完不读回校验就交付——行数对不上是最低级错误
- ❌ 编号列被 Excel 吞成数字（前导零消失）——没做类型保持

## 输出契约

`## Schema 确认`（列/行数/异常）→ `## 处理规则`（逐项）→ `## 校验结果`
（行数对账 + 抽查）→ 文件完整路径。遵循系统提示 REPORT_SECTIONING 分节规则。

## 参考

- anthropics 官方 xlsx 技能（AgentSkills 开发榜第 2）：代码处理而非手改的范式
