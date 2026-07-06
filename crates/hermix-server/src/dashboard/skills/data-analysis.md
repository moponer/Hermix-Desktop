---
name: "data-analysis"
display_name: "数据分析"
category: "office"
description: "分析数据集，生成可视化图表和洞察报告。"
version: "0.1.0"
author: "Hermes Desktop"
source: "builtin"
security_level: "safe"
tags: ["office", "builtin"]
---

# 数据分析 Skill

## 触发条件
当用户要求以下操作时使用此 Skill:
- "数据分析" / "data analysis"
- "统计" / "statistics"
- "数据可视化" / "visualization"
- "分析数据" / "analyze data"

## 执行步骤
1. 确认数据格式和分析目标
2. 加载和清洗数据
3. 执行分析（描述/诊断/预测）
4. 输出分析报告和图表

## 支持的数据格式
- CSV / TSV、Excel (xlsx/xls)、JSON、数据库查询结果

## 分析方法
### 描述性分析
- 统计摘要、分布分析、趋势分析

### 诊断性分析
- 异常检测、相关性分析、根因分析

### 预测性分析
- 趋势预测、模式识别

## 输出格式
```
## 数据分析报告
### 数据概览
- 数据量: [行数] x [列数]
- 时间范围: [起始] ~ [结束]

### 关键发现
- [发现 1]
- [发现 2]

### 统计摘要
| 指标 | 值 |
|------|----|
| [指标] | [值] |

### 建议
- [建议 1]
```
