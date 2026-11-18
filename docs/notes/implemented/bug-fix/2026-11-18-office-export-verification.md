# Decision Note: 办公文档能力端到端验证（xlsx / docx / pptx）

Status: implemented — 三种格式全部用独立读取器验证通过；修复 docx 标题样式丢失缺陷

## Decision

`office_export` 此前**从未被调用过**（全量日志里 0 次），所以「能不能用」没有任何证据。
这次做了一次真实的端到端验证：生成三种格式，再用**独立实现**读回来核对内容。

### 验证方法：独立读取器，不是自我验证

关键原则：**写入器和它的配套读取器互相认同，不能证明文件是对的。** 所以验证用的是
Python 的 `openpyxl` / `python-docx` / `python-pptx`——三个与 Rust 实现完全无关的库。

同时补了两个 Rust 侧测试：

- `e2e_writes_all_three_formats`：生成三种格式，断言 ZIP 魔数、OOXML 成员名、文件大小下限。
- `docx_declares_heading_styles`：直接从 `styles.xml` 里断言 `Heading1/2/3` 存在。

### 发现并修复的真实缺陷：docx 标题样式全部丢失

**症状**：生成的 docx 里，`heading1/2/3` 块在 Word 中显示为**普通正文**，导航窗格空白，
无法生成目录。

**根因**：`docx-rs` 的 `Paragraph::style("Heading1")` 只是**引用一个样式 id**，它不自带
标题样式定义。原实现没有调用 `add_style`，于是生成的 `styles.xml` 里只有 `Normal`。

**证据**：修复前 python-docx 报告 `len(doc.styles) == 1`，所有标题段落的 `p.style` 是
`None`；修复后 `== 4`，`Heading 1` / `Heading 2` / `Heading 3` 正确应用。

**修复**：显式 `add_style` 三个标题样式，并带 `outline_lvl(0/1/2)`——这个属性是 Word
导航窗格和目录识别标题层级的依据，只声明样式名不够。

### 验证结果

| 格式 | 独立读取器 | 结果 |
|---|---|---|
| xlsx | openpyxl | ✅ 两个 sheet（`Sales` / `汇总`），中文表名、数字、浮点、布尔、`null` 空单元格全部正确 |
| docx | python-docx | ✅ 4 个样式，Heading 1/2/3 正确应用，中文正常 |
| pptx | python-pptx | ✅ 2 页，标题与要点文本正确 |

## Alternatives considered

- **只看 `ok: true` 和文件大小**：正是这个思路导致缺陷长期存在——工具返回成功、文件非空，
  但内容结构是错的。必须用独立读取器。
- **用仓库自己的 `markitdown` 读回来验证**：同一代码库的实现，有共同假设；且 markitdown
  走的是文本提取路径，看不出标题**样式**是否生效。
- **加 `zip` crate 作为 dev-dependency**：为读回自己写的 ZIP 而加依赖不值得。改用 30 行的
  最小读取器（只处理 stored 条目——docx-rs 就是这么写的），并在注释里说明为什么。
- **用 `flate2` 解压**：同上。实际检查发现 docx-rs 用 method=0（未压缩），deflate 分支是
  死代码，删掉更诚实。

## Consequences

- docx 现在有正确的标题层级，可用于 Word 导航窗格与自动目录。**这是行为变化**：
  之前所有 docx 产出都没有层级。
- 标题样式只定义了层级和名称，**没有定义字号/字重**——`docx-rs` 不会自动套用 Word 内置
  标题的视觉样式，实际渲染取决于 Word 的默认模板。要精确控制字号需要额外设置 `size`/`bold`。
  当前选择不做：默认模板的标题外观是合理的，硬编码字号反而会在不同主题下变丑。
- pptx 没有单独的封面页：`title` 参数只用于文档属性，幻灯片就是 `slides` 数组。若需要
  封面页，调用方自己加一个 slide。这是有意的——封面版式因场景差异太大，工具不该猜。
- 回归测试 `docx_declares_heading_styles` 断言的是 `styles.xml` 的**字节内容**，
  如果将来换成别的 docx 库，这个测试会失败——那时应该更新测试，而不是删掉它。
### 最终验证：用真正的 Microsoft Office 打开

Python 库只能证明「文件结构符合规范」，不能证明「Office 认得它」。所以最后一层验证
用 COM 接口驱动**真实的 Microsoft Office 应用**打开文件并读回内容：

```
Word  : 标题 1 | 季度报告
        标题 2 | 销售概况
        正文   | 本季度总销售额 4730.5，环比增长 12%。
        正文   | 北区领先，贡献 1450.5
        正文   | 南区稳定，贡献 1100
        标题 3 | 风险
        正文   | 东区两个季度均为零，需要排查。

Excel : Sheets=2  [Sales, 汇总]
        A1="Region"  B2=1200.0  C2=1450.5  D2=true

PowerPoint: SlideCount=2
        "销售概况 / 总销售额 4730.5 / 环比 +12%"
        "风险 / 东区连续两季为零 / 需要排查渠道"
```

注意 Word 报的是**本地化样式名**（`标题 1` / `正文`），这正说明样式被 Word 真正识别了
——它按自己的语言映射了 `Heading1` / `Normal`，而不是原样回显我们写的 id。

三层验证的分工是刻意的：Rust 测试保证**结构**、Python 库保证**规范符合性**、
Office COM 保证**真实应用能打开**。任何一层单独都不够。
