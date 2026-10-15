# Decision Note: 手机端语音输入（桌面配置 provider，手机继承直发）

Status: implemented — 服务端配置存储 + WS 密封帧下发 + 手机端接线全部落地（cargo test 415 passed；桌面/手机 tsc 全绿）

## Decision

手机端语音输入按「**桌面配置一个云识别 provider，手机继承直发**」落地：

- **桌面端**（设置 → 管理 → 手机语音识别，`VoiceTab.tsx`）配置 provider：
  `{enabled, endpoint, api_key, model, language, audio_mime, extra_headers}`，
  持久化在 `$HERMIX_HOME/voice_stt.json`（`dashboard/voice_stt.rs`），
  经 `GET/PUT /api/voice/stt` 读写。provider 无关：endpoint 只要收 POST
  音频、返回 `{text}` 即可（MiniMax / OpenAI 兼容 / SiliconFlow 等）。
- **下发**：每条 WS 会话建立时（`ws::handle_ws_stream`，紧跟 Connected
  hydrate），配置作为 `voice_stt_config` 帧发出，经 `mobile_protocol`
  映射为 `voice.stt_config` 密封帧——relay 只见密文。
- **手机端**（`pages/chat/index.vue`）：收到配置帧后构建 `CloudAsr`
  （`core/voice.ts`，provider 无关的整轮上传实现）并持久化到本地，
  断线重连间隙沿用旧值。长按说话 = `uni.getRecorderManager` 录 MP3
  （16kHz/mono/≤60s）→ 松手读临时文件整体 POST 给 provider → 文字
  回显为可确认气泡 → 经既有加密 WS 通道发送。音频直连 provider，
  不经 relay、不经 PC；文字走密封通道。
- **未配置**：不发配置帧，手机端长按说话 toast
  「桌面端未配置语音识别，仅支持文字发送」——未配置状态必须显式可见，
  不能静默没有按钮。

隐私边界：音频对所选云 provider 可见（与"打字发给 LLM"同一信任档），
relay 全程零可见；api_key 只存桌面本机与手机端，经 E2EE 下发。

## Alternatives considered

- **手机直连 SiliconFlow 免费 ASR（原 P0 提案）**：被否决。主人明确
  不接受"客户端自己填配置直连云"的形态——provider 的选择权在桌面端，
  手机端不该有自己的识别配置面；桌面配置天然继承了 key 的分发与撤销。
- **录音传 PC、PC 本地 FunASR/whisper 识别**：架构最私密但被搁置——
  REST 面在 relay 模式下尚未打通（见下），而桌面已配置的云 provider
  是零新依赖的路径。`/v1/voice/transcribe` 与其权限闸保留为将来切换点。
- **手机端本地模型（sherpa-onnx 端侧 zipformer）**：技术可行（zh-14M
  int8 约 52MB 手机 CPU 实时）但 uni-app 原生插件集成成本高，且与
  "桌面统一配置"的拍板形态不符；保留为通话流式（P2）的候选。
- **讯飞/百度/腾讯一句话识别**：免费额度琐碎、企业实名、非通用形状，
  接入即死代码，否决。

## Consequences

- 音频出设备到所选云 provider（HTTPS 直连，不落 relay）。provider
  的隐私政策即音频的隐私边界——这是拍板接受的交换。
- `voice_stt.json` 内含 api_key，属本机敏感文件；本模块不打印任何
  含 key 的日志。桌面设置页以密码框回显。
- 录音上限 60s（`uni.getRecorderManager` duration 硬编码），更长的
  语音输入（口述文档等）不在本方案覆盖内。
- **遗留缺口（影响本方案的可用性验证）**：手机 REST 面（tasks/
  approvals/voice transcribe 等）在 relay 模式下仍打不到 PC——relay
  的 `/v1/*` 无转发路径（404）。已拍板修 E2EE HTTP 桥（HTTP 请求整体
  作为密封 payload 走既有 route 帧，relay 保持全盲），落地前本方案的
  "未配置"判定与文字回程不受影响（都走 WS 帧），但涉及 REST 的功能
  （审批列表、任务列表）在真机仍不可用，需真机验证时先行修桥。
- 通话页流式（P2）暂不立项， sherpa-onnx 端侧方案保留为将来候选。
