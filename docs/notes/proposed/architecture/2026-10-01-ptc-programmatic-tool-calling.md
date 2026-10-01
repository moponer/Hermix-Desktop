# Decision Note: PTC —— 模型写代码组合工具调用（批次 F-3 立项）

Status: proposed — 待评审；依赖 F-1 事件溯源先落地第 1-2 步

## Decision

新增 **PTC（Programmatic Tool Calling）模式**：模型写一段 TypeScript 程序
组合多轮工具调用，沙箱进程执行，只回传打印值——对标 dsh
`2026-06-15-ptc` + `2026-07-20-ptc-typed-tool-returns`。动机（dsh 实测
结论）：LLM 写代码比逐轮 JSON tool call 更可靠；循环/分支/并行在代码里
完成，中间结果不占父上下文（零膨胀原则的代码形态）。

核心设计（对标 dsh）：
- **工具暴露为类型化 SDK**：从注册表 schema 自动生成 `.d.ts`，工具返回
  带类型签名（编译期保证 execute 与对模型承诺一致，dsh
  `typed-tool-returns`）。
- **执行边界 fail-closed**：PTC 程序内可调用的工具 = 本轮 allowed 白名单
  交集，模式收敛做在**执行解析层**且在审批管线之前——wire 层藏 schema
  不算强制（dsh `ptc-executor-collapse` 的事故教训：模型仍可直调原生
  工具绕过管线；hermix 已有 ApprovalFallback 必填机制，同一原则复用）。
- **审批语义**：PTC 内每次工具调用仍走既有审批面（危险调用在程序执行中
  弹卡，程序暂停等待——不是"跑代码就免审批"）。
- **超时与资源**：程序执行有独立总时长预算 + 静默超时（对齐已落地的
  D.4 drain 语义），产物回传走既有 spill 机制。
- **落地形态**：新增 `ptc` 工具（`program: string` 参数），执行器挂
  hermix-tools；运行时用 Node 侧车（现有 hx-shell 通道可复用）——Rust 侧
  不内嵌 JS 引擎，避免引 deno_core/boa 的重量。

## Alternatives considered

- **Rust 内嵌 JS 引擎（boa/deno_core）** —— 否决：编译依赖重、MSVC 工具链
  风险大；Node 侧车已是项目既有模式（stdio 多路复用已上线）。
- **只做 prompt 层"把工具文档给模型让它写脚本调 CLI"** —— 否决：无类型
  检查、无审批接入、无超时边界，等于没有执行契约。
- **先做 Dynamic Workflows（dsh 的 JS 编排脚本）** —— 否决：PTC 是其
  子集且更小，先验证"模型写代码调工具"的价值再扩编排原语
  （agent()/parallel()），防止一次引入两个新概念。
- **不写类型签名，直接给 JSON schema** —— 否决：dsh 实测类型化返回
  是 PTC 可靠性的关键一环，省掉等于重踩。

## Consequences

- 放弃：无 Node 环境的极简部署不可用 PTC（桌面端始终带 Node，无损；
  server headless 需检测降级）。
- 代价：执行器 + SDK 生成 + 审批桥接约 1-2 周；每次 PTC 调用有 Node 进程
  冷启动（可常驻池化后置）。
- 风险：模型在程序里写死循环 → 总时长预算 + 静默超时兜底；程序打印超大
  输出 → 走 spill，不入父上下文。
- 收益：批量文件改写/多步数据整理类任务的 token 成本与轮次显著下降
  （dsh 把它列为核心架构差异）；与既有 allowed 白名单/审批/spill 全部
  正交复用，无新权限面。
- **依赖**：F-1 事件溯源第 1-2 步先行——PTC 的程序执行轮次需要可回放
  的事件流做调试，否则排障只能靠打印。
