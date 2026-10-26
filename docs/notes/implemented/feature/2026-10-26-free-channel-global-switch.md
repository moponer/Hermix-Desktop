# Decision Note: 社区免费模型总开关（模型配置页）

Status: implemented — 2026-10-26 交付：设置→模型新增总开关；关=不拉取并移除已拉取的免费条目，开=重建并后台拉社区清单；开关值以后端 config.yaml 为准；同轮补齐**网关地址披露 + 逐条免责 + 网络可达性提示**

## Decision

给「社区免费模型」（内置免费渠道 `custom-free-*`，Kilo 网关等）加一个**整体开关**，
放在「设置 → 模型」页：**开 = 拉取**，**关 = 不拉取，且把已拉取的移除**。

### 为什么「加个开关」不是加个 UI 就完事

`free_channel.enabled` 这个字段早就在配置里（`FreeChannelConfig.enabled`，默认 true），
但它有两个致命缺口：

1. **没有入口** —— 前端从来没有读写它的地方，用户只能改 `config.yaml`；
2. **不即时生效** —— 唯一消费它的地方是**启动路径**
   （`hermix-server/src/state.rs`：`!enabled` → `free_channel::strip(&mut config)`）。
   进程起来之后把开关改成 false，**免费模型照样在跑、后台照样在拉**，
   非要重启进程才生效 —— 这正是「没有整体开关」的体感来源。

所以这次交付的是「运行时开关」，不是「加一个复选框」。

### 后端

新增 `GET/PUT /api/free-channel`（`dashboard/config.rs` + `free_channel.rs`）：

- **关** → `hermix_config::free_channel::strip` 摘除已拉取的免费条目
  （provider 目录 + `model_selector.enabled` + 指向它们的默认模型），之后启动阶段
  不再 seed、后台也不再拉；
- **开** → `ensure_seeded` 重建渠道条目，并把「上下文最大的 N 个免费模型」写回
  启用清单（`defaults_seeded` 一次性标记要清掉，否则重开后只有目录行、
  聊天下拉里一条都没有），随后**后台**拉一次社区清单（网络 IO 不阻塞响应）；
- 落盘后 `service.set_config(...)` 热推送，运行中的 agent 立刻看到新配置。

配置层改动抽成纯函数 `free_channel::apply_switch(&mut cfg, enabled) -> (摘除数, 写回数)`，
可单测（不碰磁盘、不联网）。

**默认模型顺延**：`strip` 会把默认模型清空。关掉免费源不该让用户停在
「无模型可用」，所以有剩余启用项时指到第一条（`repoint_default`）。

### 前端（设置 → 模型）

- 开关用共享原语 `SettingsGroup` / `SettingsRow` / `Switch`；
- **开关值来自后端**（`GET /api/free-channel`），不记 localStorage ——
  那是本仓库反复踩过的「双权威漂移」；
- **不做乐观更新**：写完回读再渲染。写失败时回读真实状态 + 报错，
  不能让界面显示一个没落盘的开关；
- 后端读不到（老后端没这个接口）时，开关禁用并说明「需更新后端」——
  把「未知」画成「关」等于骗用户；
- 免费源是当前**唯一**可用模型时，关闭前弹确认（关掉就没有模型可用了）。

### 「数据与输入框的联动」

开关联动的下游数据有三处，全部由一次 `load()` 收口：

1. **模型列表**（设置页行）—— 免费条目立即消失 / 出现；
2. **默认模型** —— 后端顺延后，聊天端的默认模型跟着换；
3. **聊天输入框上的模型下拉** —— `load()` 里已有的 `syncModelConfigLocal`
   会把后端权威清单镜像进 localStorage 并广播 `MODEL_CONFIG_CHANGED_EVENT`，
   `ModelSelector` 收到后 refresh + reconcile → 免费模型立刻从下拉里消失。

## Alternatives considered

- **只在前端加个开关写 config.yaml**：不即时生效（摘除只在启动路径），
  也不热推送运行中的 agent —— 等于把「开关无效」换个地方复现，不采纳。
- **前端用 `PUT /api/model/list` 清空启用清单**：只清 `enabled`，
  provider 的模型目录和 `config.model` 还在，启动时的后台刷新还会把免费模型
  拉回来 —— 不是真正的「关」，不采纳。
- **把开关状态写 localStorage**：双权威漂移（后端 config.yaml 与本地缓存各说各话），
  本仓库已有成套教训，不采纳。
- **关掉时不确认**：用户唯一的模型就是免费源时，关完应用就没模型可用，
  而他完全不知道为什么 —— 加一步确认，不采纳「直接关」。
- **在「高级」页用通用 config 编辑器暴露这个字段**：那里是给专家调参用的，
  普通用户不会去；开关要放在「模型」页它才有意义。

## Consequences

- 关掉 = 免费 provider 目录、启用清单里的免费条目、指向它们的默认模型**当场**被摘除；
  再打开 = seed 模型立刻回来，社区清单在后台拉（几秒后列表变完整）。
- 本机当前默认模型正是免费模型（`custom-free-kilo:kilo-auto/free`），
  所以第一次关这个开关会触发确认弹窗（关完没有可用模型）。
- 新增 `GET/PUT /api/free-channel`：桌面端若连到旧后端，开关显示为禁用并提示更新，
  不会假装可用。
- `free_channel.enabled` 的语义没变（默认 true），只是终于有了入口与运行时语义；
  存量用户升级后行为不变（启动路径照旧生效）。

## 补：网关地址、免责与网络可达性（同一轮追加）

用户复核后指出：界面上**没有写明免费模型连的是哪个网关**，也没有把
「数据流向谁」「部分网络访问不了」讲清楚。实测确认三件事：

1. **上游每个免费模型条目都自带逐条免责**。实测 Kilo 网关
   （`https://api.kilo.ai/api/gateway/models`，401 条、其中 `isFree=true` 17 条）：
   `kilo-auto/free` 的 description 写着 "Prompts may be logged by the upstream
   provider and used to improve their services. Not suitable for production or
   sensitive data workloads."；NVIDIA 的 `:free` 端点写明 "Trial use only"。
   **这些条款此前只存在于上游清单里，我们的 UI 一句都没显示** ——
   用户等于在不知道自己同意了谁的条款的情况下把对话发出去。
2. **免费池里没有 GitHub 模型的影子**（清单里没有 `github/` 前缀条目），
   但这不影响结论：凡是社区/第三方/海外端点，地址、免责、可达性三样都得写。
3. 本机直连该网关是通的（能拉到清单）—— 但**部分网络环境**（企业网、内网直连、
   严格防火墙）连不上，这是社区端点的固有风险。

据此追加三件事：

- **地址披露**：`FreeChannelState.endpoints` 下发各渠道的**实际**端点
  （config 覆盖 > 环境变量 > 内置；用户自建的 `custom-free-*` 用它自己 entry 的
  base_url），设置页逐条显示推理网关与模型清单地址，点开用系统浏览器打开。
- **逐条免责**：上游 `description` 落盘到新的 `LlmProviderConfig.model_notes`
  （压平空白 + 截断 400 字符），随 `/api/model/list` 的 `note` 字段下发，
  免费模型行下方以警示色单行显示（全文在 title 里）。
- **可达性提示**：设置页固定说明「部分网络环境访问不到社区网关，表现为测试连接
  超时或失败 —— 那是网络不可达，不是模型坏了」；免费模型的「测试连接」在
  **status 0（传输层就失败）** 时改说「连不上社区网关（网络不可达）」，
  不再误报成「网关可达但探测失败」。

## 补：网关地址一节的备选与后果

- **只放一句通用免责**：上游条款是**逐条**的（NVIDIA 的试用条款 ≠ OpenRouter 的
  训练条款），一句通用文案替代不了逐条说明 —— 落盘 + 逐行显示。
- **把 description 全文落盘**：单条可达 2000+ 字符，17 个模型就是几十 KB，
  config.yaml 会被撑大；压平 + 截断 400 字符足够表达条款。
- **在模型页给一个「端点」输入框让用户自己填**：本轮只做**披露**（写清连的是谁
  + 给出 config.yaml 的覆盖路径 `free_channel.base_url` / `models_url`），
  输入框属于新增可编辑能力，等确认需求再做。

