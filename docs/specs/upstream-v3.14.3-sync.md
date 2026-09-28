# 同步上游 ZCode v3.14.3

## 范围与产品规则

- 先将 `rust-p0-p1` 的 `eb39660` 快进合入本地 `main`，保留全部 Rust 迁移提交，再合并 `zai-org/ZCode` 的 `29628c9`（v3.14.3）。
- 保留 Rust runtime 的启动入口、核心实现、测试与既有能力边界；接受上游客户端、TypeScript runtime、共享协议和发布元数据更新。
- README 保留 Rust 迁移目标，并单独记录上游版本更新。上游功能进入 TypeScript runtime 不代表 Rust 已实现相同能力。
- 本次完成本地合并与提交，不包含远端推送或发布。

## 所有者与接口

共享 TypeScript schema、prompt 与工具定义继续作为现有 Rust 生成资产的来源。生成资产发生漂移时，使用仓库生成脚本更新，不手工维护第二套定义。runtime 仍拥有会话、命令接纳与执行状态；Host 仍拥有进程和 attachment 路由，Renderer 读取既有投影。

本次生成资产差异为：V4 `sendText` 增加可选 `botDeliveryTarget` 的共享校验结构，以及 Node workflow 投影的 `predecessorRunId` 改为可选字段。使用 `generate-zcode-cli-rust-protocol-schema.mjs` 和 `generate-zcode-cli-rust-fixtures.mjs` 生成；后者沿用测试入口的 `TSX_TSCONFIG_PATH=packages/services/tests/tsconfig.zcode-cli-rust.json`，确保引用当前 TypeScript 源码。

```text
上游共享契约 → 现有生成脚本 → Rust schema / prompt / 工具资产
          └→ Host 转发 → 原有 runtime owner → 会话事实与订阅投影
```

同步不重新定义状态所有权、workspace identity、owner/lease 或事件顺序；保留桌面 `desktop-continuous` 与手机 `web-remote-replayable` 的既有语义。新增上游能力不得绕过 Rust 原有能力协商。

## 验收

- 最终 `main` 同时包含上述两个提交，保留原分支历史，无未解决冲突。
- `pnpm typecheck`、`pnpm lint`、`pnpm architecture:check --changed`；区分合并前既有 warning 与合并后新增问题。
- `pnpm test:zcode-cli-rust` 检查生成资产、Rust 单测、Node/Rust 存储互通与 Host 集成；按当前可用环境报告失败或未验证项。
- `git diff --check` 通过；未进行真实 App、手机或跨平台交互验证时，不将其报告为通过。
