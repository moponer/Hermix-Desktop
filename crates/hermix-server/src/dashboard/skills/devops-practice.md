---
name: "devops-practice"
display_name: "DevOps 实践"
category: "devops"
description: "用于：Docker、Kubernetes、CI/CD 流水线、GitOps、监控与基础设施即代码等 DevOps 实践。"
version: "0.1.0"
author: "Hermes Desktop"
source: "builtin"
security_level: "safe"
tags: ["devops", "builtin"]
---

# DevOps 实践 Skill

## 概述
DevOps 是开发和运维的协作实践，目标是快速、可靠地交付软件。

## 核心实践
1. **持续集成**：频繁合并代码，自动构建和测试
2. **持续交付**：随时可以发布到生产环境
3. **基础设施即代码**：用代码管理基础设施
4. **监控和日志**：实时监控系统状态

## 工具栈
- 容器化：Docker
- 编排：Kubernetes
- CI/CD：GitHub Actions, GitLab CI
- 监控：Prometheus, Grafana
- 日志：ELK Stack

## 何时激活
- 搭建 / 优化 CI/CD 流水线时
- 容器化应用、上 K8s、做 GitOps 时
- 排查发布失败、环境不一致、监控盲区时

## 实践指南
1. 一切走代码（IaC），环境用同一份定义
2. 流水线分层：构建→测试→安全扫描→部署
3. 小步快跑，主干开发 + 特性开关
4. 可观测三件套：指标 / 日志 / 链路

## 注意事项
- 别把密钥提交进仓库（用 Secret 管理）
- 别手动改线上环境（配置漂移之源）
- 监控要有关键告警，不是只堆面板

## 集成
- 与 `architecture-design` 衔接：部署拓扑是架构输出
- 与 `code-review` / `security-audit` 配合：流水线里做卡点

## 参考
- 《Site Reliability Engineering》（Google）
- Docker / Kubernetes 官方文档
- CNCF 技术雷达

## 代码样例

```yaml
jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - run: cargo test --workspace
      - run: pnpm tsc --noEmit   # 编译+类型检查走 CI，本地只跑快路径
```

