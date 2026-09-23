# Rust 核心替换推进顺序

2026-09-22 用户明确调整：MCP、Skill、子代理、Goal，与 session 按需加载、重试回复、编辑重跑、分支、文件回退同为本阶段关键能力。此前把扩展执行统一排在 P2 的排序不再适用。权限仍仅 yolo，无 TUI；保留现有 App/Host/stdin-stdout 协议与身份隔离。

执行依赖顺序：session 按需加载 → MCP/Skill 真实发现与调用 → 子代理/Goal 生命周期 → 会话历史操作与文件回退。每项单独补充契约与故障验收后实现，完成一项不代表其他项已经交付。账号、模型、工具 IO 继续走现有 ports。

- MCP：沿用现有配置、工具命名、schema、stdio/HTTP transport、鉴权/取消/超时/重连与进程清理；不返回伪造的空目录。工具进入当前会话实际可用列表，结果提交后才继续模型。
- Skill：沿用工作区与用户/插件来源的发现、优先级、元数据和内容加载；Skill 调用影响实际模型上下文，冷恢复保留调用事实；不把所有技能正文预先塞进上下文。
- 子代理：父子 session 身份、执行/等待/消息/取消、结果归属与 App 投影同 TS；子代理使用同一模型与工具端口。父会话关闭、重启、迟到结果及工具预算必须有测试。
- Goal：沿用目标状态、推进/验证、暂停/恢复、预算和冷恢复；单一 Session owner 持有目标状态，不靠定时器伪造完成或无界重复回合。
- 会话操作：依据当前 TS fork-edit-retry/file-rewind handlers。retry/edit 针对当前允许的最新轮做同会话 active branch cut；fork 复制稳定边界，不复制运行队列和副作用；文件恢复有冲突检测和提交屏障。rowId/entityId/epoch/revision 必须同时验证，错误目标不能先停止当前轮。

session 加载首先按 [rust-session-loading.md](rust-session-loading.md) 收缩启动路径。其他功能实现前分别记录当前 TS 精确输入、状态机、错误码和 E2E 验收，不能仅依据本工作包摘要宣称等价。
