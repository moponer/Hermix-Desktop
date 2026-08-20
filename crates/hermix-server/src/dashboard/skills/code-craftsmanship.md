---
name: code-craftsmanship
version: 1
description: >
  代码精炼标准。Hermix 与 WorkBuddy 全项目通用。
  六级决策阶梯（YAGNI / 标准库 / 原生平台 / 现有依赖 / 单行代码 / 最小实现）+ 不重复造轮子铁律。
  适用于所有落地代码（Rust / TypeScript / Python / 配置 / skill 文档），
  以及架构 / 设计 / bug 修复 / 重构等一切需要写代码或决策的场景。
tags:
  - code-quality
  - craftsmanship
  - yagni
  - reuse
  - hermix
  - workbuddy
---

# Code Craftsmanship（代码精炼标准）

## 核心铁律（不可妥协）

1. **单行可实现的不得啰嗦写多行** — 一行能说清的事, 不要拆三行
2. **须封装复用的必须拆分封装** — 同段逻辑出现 ≥2 次就该抽函数/常量/组件
3. **可复用优秀开源项目的坚决不重复造轮子** — 先搜 GitHub/Gitee/GitCode, 找不到再自研

## 六级决策阶梯（按优先级，缺一不可跳）

走完前 5 步都否决，才考虑第 6 步：

1. **YAGNI** — 不需要就跳过, 一句话说明即可; 不写"为以后预留"的脚手架
2. **标准库** — 优先用语言/运行时自带能力, 不引依赖 (`std`/`fetch`/`URLSearchParams`/`crypto.subtle`/)
4. **原生平台** — 浏览器/OS/DB 原生特性优先于库 (`<input type=date">` > 日期库; CSS > JS; DB 约束 > 应用代码; WebView2 原生桥 > 自写 IPC 包装)
5. **现有依赖** — 已安装的 crate/npm/pip 包优先; 不为几行逻辑加新依赖
6. **单行代码** — 能一行解决的绝不写多行 (`if let Some(x) = y else { return };` 优于 4 行 if 块; `a && b` 优于 `if a { b }`)
7. **最小实现** — 只有走到这步才写"能跑的最小代码"

## 不可削减项（豁免：质量底线）

- 信任边界校验（用户输入、IPC、外部 API 响应）
- 防数据丢失的错误处理（数据库、文件 IO、网络请求）
- 安全措施（路径穿越防护、SSRF 防护、权限校验）
- 无障碍基础（语义化 HTML、键盘导航）
- 用户明确要求的功能

## 注释与代码风格

- **注释只写功能/技术根因**, 禁日期、人物、Round、commit 引用、历史遗留等 changelog 体
- `////` 归一为 `//`, 不用 `///`（除非是 doc comment 给公共 API）
- 函数名/变量名表达意图, 不加冗余前缀（`is_*`/`has_*`/`can_*` 表明意图即可）
- 错误信息含具体路径/参数/原因, 不要 "失败" 这种泛词

## 不重复造轮子铁律

**写任何能力前先查**:

```
能力点 → GitHub 搜 (topic + language:rust / typescript / python)
       ↓ 不可达
       → Gitee 搜
       ↓ 不可达
       → GitCode 搜
       ↓ 都不可达
       → 自研 (记录原因: 搜了哪些关键词 + 为何现有方案都不适用)
```

**判定标准**：
- 项目 ≥ 100 stars + MIT/Apache/BSD license + 半年内有 commit + 维护活跃 = 可用
- 必须检查 License 与依赖兼容性 (GPL 污染整个项目, 不可用)
- 嵌入方式: 优先源码嵌入 (`include_str!` / 拷贝到 `vendor/`) > npm crate > 动态下载

**不允许**:
- 写 HTTP 客户端时, 已有 `reqwest` 但又手撸一个 TCP socket HTTP 解析
- 写日期格式化时, 已有 `chrono` 但又自己算 `seconds since epoch`
- 写 markdown 解析时, 已有 `pulldown-cmark` 但又自己 split 行

## 反模式（禁止）

- ❌ 50+ 行单函数（必须拆）
- ❌ "为以后扩展"留的 trait / 接口 / 配置项（YAGNI）
- ❌ 把 std::time 包装成 `now_millis()`/`now_seconds()`/`now_nanos()` 三个函数
- ❌ 写 README "可能以后会支持" 的特性列表
- ❌ 在 [dev-dependencies] 里放运行时用的包（污染构建图）
- ❌ 重复 import 同一个 crate 的不同子模块（用 `use crate::x::{a, b}`）
- ❌ `unwrap()` 在生产代码（用 `?` / `expect("xxx")` 带原因）

## 量化目标

- **长期把全项目源码总量控制在 10 万行以内**（当前基线 ≈14 万 ），node_modules/target 已排除
- `ci-gate.sh` 增加单文件 **> 2500 行即告警**, 防止上帝文件复发
- 拆分/重写时优先砍重复（平行复制的平台/provider/工具）与搬测试到独立文件, 而非单纯切文件

## 与其它规则的关系

- **AGENTS.md §核心原则**: 与第一性原理、对抗式审查、零 context 膨胀并列为顶层约束
- **AGENTS.md §代码简化纪律**: 即本 skill 的前身, 现在升级为独立可复用的 skill
- **WorkBuddy `ponytail` skill**: 上游 DietrichGebert/ponytail 的七级阶梯, 二者等价, 本 skill 是其 hermix 化精简版
- **i-have-adhd skill**: 不冲突, adhd 风格管输出形状, 本 skill 管代码决策

## References

- AGENTS.md §核心原则
- WorkBuddy `ponytail` skill: 上游原始版本
- DietrichGebert/ponytail: https://github.com/DietrichGebert/ponytail