# 品牌色豁免登记（EXCEPTIONS）

规范：组件内禁止裸 `#hex`；但**品牌识别色**是数据不是装饰，允许在登记后使用。
新增品牌色必须先在本文件登记（名称 + 用途 + 色值来源），再写代码。

## 已登记

| 文件 | 常量 | 用途 | 说明 |
|---|---|---|---|
| `src/components/ProviderIcon.tsx` | `BRAND_COLOR`（26 项） | 厂商 logo 软底/前景 | lobehub/官方站点主色；软底已改 `color-mix` 派生（2026-10-16），禁拼 hex8 |
| `src/components/FileTypeIcon.tsx` | 按扩展名的色表（97 处） | 文件类型图标底色 | 待分批令牌化（R4 剩余），新增色必须同时登记 |
| `src/components/agents/types.ts` | 8 处 hex | 专家头像色板 | 后续并入 ColorPicker 档位 |

## 使用纪律

1. 品牌色只许做**图标前景 / 软底**，不许做按钮、边框、大面积底色
2. 软底一律 `color-mix(in srgb, <色> 12%, transparent)`，禁止字符串拼 hex8
3. 与语义色（success/warning/error/info）冲突时语义色优先
