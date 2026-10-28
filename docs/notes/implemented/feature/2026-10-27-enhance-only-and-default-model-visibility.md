# Decision Note: 输入框只留「增强」，并把「默认模型」摆到明面上

Status: implemented — 润色按钮与 `/api/polish/chat` 已移除；增强的客户端死线对齐后端；默认模型在聊天弹层与阿黎顶栏可见

## Decision

三件互相独立的收口，同一批落地：

1. **移除「润色」，只留「增强」。** 两个按钮解决的是同一类需求（「这句话帮我弄好一点」），
   差别只在`同义替换 / 扩写补结构`，用户每次都得先分辨该点哪个。`PolishButton`、
   `api/polish.ts::polishChat`、`prompts/polish.chat.ts`、后端 `/api/polish/chat` 路由与
   `CHAT_POLISH_PROMPT` 一并删除。`/api/polish/automation`（新建自动化页的 PROMPT 改写）
   与 `/api/polish/enhance` 保留。
2. **增强不再必然报错：超时预算对齐 + 用当前选中的模型。** 增强是**非流式的一整轮生成**
   （150–600 字），而后端 `POLISH_TIMEOUT` 是 12s、前端 `apiInvoke` 的全局死线只有 10s ——
   客户端比后端先放弃，慢源上用户看到的就是 `request timed out after 10000ms`。
   现在：后端 `POLISH_TIMEOUT = 90s`，前端给 `fetchJSON` 加了 per-call `timeoutMs`，
   增强传 90s（stdio 桥的单请求上限是 120s，仍在预算内）。其它接口的 10s 不变。
   同时增强请求带上输入框**当前选中的模型**（`EnhanceRequest.model` → `AgentOverrides.model`），
   不再隐式回落到 `config.yaml` 的全局默认 —— 后者失效时会出现「聊天正常、增强每次报错」
   这种最难自查的错位。
3. **默认模型可见。** 聊天输入框的模型弹层给默认那条挂「默认」标（`Composer.defaultModel`，
   数据来自 `useModelSelector().defaultModel`），模型按钮的 tooltip 也带上默认模型名；
   阿黎顶栏新增「模型 X」——她**没有独立模型设置**，`twin.send` 不下发 `model`，
   后端按 `overrides.model → config 默认模型` 解析，所以她跑的就是全局默认。

## Alternatives considered

- **保留润色、只是把它藏进二级菜单**：否决。重复能力换个位置还是重复，用户仍然要先判断
  「同义替换」和「扩写」的区别才敢点。真需要「只改措辞不加结构」时，用户在草稿里说一句
  「这句话通顺点」比点按钮更快。
- **只把客户端死线调大、后端保持 12s**：否决。后端 12s 到点自己返回 502 `polish timed out`，
  用户只是把「客户端超时」换成「服务端超时」。
- **给增强改流式**（`send_message_stream` + 逐段回填）：本轮不做。它改善的是「等待期间有进度」，
  不是「能不能出结果」；而改动跨 transport（stdio 桥的 http 帧不带流），风险与收益不成比例。
- **给阿黎加独立模型选择器**：否决（本轮）。她的定位是「值守的那一个」，跟随默认模型是应有语义；
  先让它**可见**。真要分开配，得先回答「默认模型漂移时谁说了算」，那是另一个决策。
- **把默认模型同时写进聊天触发按钮的可见文本**（如「默认 · MiniMax-M3.1」）：否决。
  按钮显示的是「这一轮用哪个」，和「默认是哪个」是两件事，挤在一行会让两者都读不清。

## Consequences

- 增强最坏要等 90s。等待期间按钮是 spinner + 禁用态（不会重复触发），超时提示改成中文并给出
  下一步（换更快的模型 / 缩短草稿）。
- 增强用的模型 = 输入框当前选中（`provider:model`）；只有选中为空时才落到后端默认模型。
  自动化改写（`/api/polish/automation`）仍走默认模型 —— 那个页面没有模型选择器。
- 默认模型取自 `localStorage` 镜像（`useModelSelector` 与后端 `/api/model/list` 的
  `default_model` 对齐后写回）。若后端 `config.yaml` 的 `model` 与 selector 漂移，
  界面显示的是镜像值 —— 这是既有债（`useCurrentModel` 注释里已记录），本次不扩大。
- 阿黎用的模型可见但不可改。
- `Ctrl+/` 随润色一起消失，`Ctrl+Shift+/`（增强）保留。

## Reintroduction conditions

- 用户明确要「轻量改写」（只顺句、不要扩写、不要补占位符）时：`/api/polish/chat` 与
  `CHAT_POLISH_PROMPT` 可从 git 历史原样取回，前端只需恢复 `polishChat` + 一个按钮。
- 增强的 90s 仍被慢源打穿时：改流式（见上）而不是继续加预算 —— 再加也只是把「超时」
  换成「更长的超时」。
- 需要给阿黎单独指定模型时：在 `TwinSendOptions` 加 `model`，并先定义它与默认模型的优先级。
