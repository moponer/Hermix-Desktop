# Decision Note: todo 状态在界面上可见（待执行 / 执行中 / 已完成 / 已失败）

Status: implemented — 每个 todo 行显示中文状态标签，「执行中」带呼吸动效

## Decision

todo 卡片此前**只有图标**表示状态：勾（completed）、叉（failed）、转圈
（in_progress）、空心圆（pending）。在 12px 下「转圈」和「空心圆」几乎分不出来，
而用户要看的是「现在在干什么」。

现在每行在标题右侧显示一个文字标签：

| status | 标签 | 颜色 | 动效 |
|---|---|---|---|
| `pending` | 待执行 | `--color-text-faint` | 无 |
| `in_progress` | 执行中 | `--color-text-warning` | 呼吸（1.6s） |
| `completed` | 已完成 | `--color-text-success` | 无 |
| `failed` | 已失败 | `--color-text-error` | 无 |

**只有「执行中」有动效**，因为它是唯一需要用户「等一等」的状态；已完成与待执行
是静态事实，动效只会分心。动效用 `opacity` 呼吸而不是缩放——缩放会抖动行高。
`prefers-reduced-motion: reduce` 下关闭动效，保留文字与颜色。

颜色一律用语义四色的**文字安全档**（`--color-text-*`），12px 下实测对比度均
≥4.5:1（见 `tokens.css` 注释里的实测值）。

### 状态完全由模型写入，前端不推断

用户明确要求「不校验，就靠模型自己确定」。因此前端**不做任何推断、不改写、不猜测
进度**：模型写 `in_progress` 就显示「执行中」，写 `pending` 就显示「待执行」。
未知状态值不显示标签（但行照常显示），而不是猜一个最接近的。

### 同时补上 in_progress 的使用说明

发现一个直接原因：`TODO_GUIDANCE` 的 API 说明写的是「`update` — change status to
`completed` or `failed`」——**完全没提 `in_progress`**。也就是说即使模型开了清单，
它也不知道有「执行中」这个状态可用，界面永远不会出现动效。

补成「`update` — change status to `in_progress`, `completed`, or `failed`」，并新增
一节说明：开始做某条时立刻标 `in_progress`；同一时刻只应有一条 `in_progress`；
失败重试时保持 `in_progress`，只有放弃该条才标 `failed`；状态是**如实报告而不是
优化展示**——为了面板好看而标 `completed` 比不开清单更糟。

## Alternatives considered

- **只加图标动画，不加文字**：用户要的是「显示各个状态」，图标需要辨认，文字不需要。
- **所有状态都加动效**：已完成的条目还在动，会让人以为还在跑。
- **用颜色区分而不写字**：违反无障碍基础（色觉障碍用户无法区分），且 `AGENTS.md`
  的 UI 纪律要求状态文案无歧义。
- **前端根据 `execution_count` / `last_result` 推断进度**：用户明确不要校验，推断就是
  一种校验，而且会与模型声明打架。
- **后端在 todo 状态变化时自动推断 in_progress**（比如有工具调用就标执行中）：
  同上，且会让「模型说了算」这条边界失效。

## Consequences

- 状态文案是前端写死的中文映射。新增状态值（如 `cancelled`）需要同步加标签，
  否则该状态不显示标签——这是有意的：宁可没有标签，也不要猜错。
- 「执行中」的呼吸动效是唯一的持续动画。若将来觉得干扰，删掉 `animation` 一行即可，
  文字与颜色不受影响。
- 后端 `status` 是自由字符串（schema 里是 enum，但存储层不校验），前端用 `Record`
  查表，未知值安全退化为无标签。
- **本改动不解决「模型根本不开清单」**。实测 `todos.json` 不存在、`todo` 工具调用
  次数为 0，所以现在界面不会显示任何东西。提示词已加「超过约 4 次工具调用或多交付物
  必须开清单」，但那是提示词层面，不保证生效。可观测指标：`todo` 调用次数应从 0 上升。
