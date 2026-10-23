//! 交互、计划模式、ToolSearch 与 Web 工具给模型看的短文本（docs/specs/rust-cli/prompts.md 规则 4、
//! docs/specs/rust-cli/tools-more.md）。文字逐字固定；
//! 工具描述与 schema 在 `prompts/tools/`。

/// AskUserQuestion 的权限询问文本。
pub(crate) const ANSWER_QUESTIONS: &str = "Answer questions?";

/// ExitPlanMode 的权限询问文本。
pub(crate) const EXIT_PLAN_MODE: &str = "Exit plan mode?";

/// AskUserQuestion：问题或选项不唯一。
pub(crate) const QUESTIONS_NOT_UNIQUE: &str =
    "Question texts must be unique, option labels must be unique within each question";

/// AskUserQuestion 校验错误前的引导语（非扩展分支）。
pub(crate) const SINGLE_OPTION_STEER: &str = "This call included a question with fewer than 2 options, so it was rejected and the person never saw it. A question with a single option has no decision in it. Do not retry this call and do not invent a filler second option. Instead, state the one path you were going to offer as the approach you are taking, then continue with the task. If this call also contained questions with 2 to 4 options (each with distinct labels), you may re-ask those questions alone in a new call. Ask a question only when the person has at least two genuinely distinct choices.";

/// AskUserQuestion 结果：全部答案都落在选项里。
pub(crate) fn questions_answered(answers: &str) -> String {
    format!(
        "Your questions have been answered: {answers}. You can now continue with these answers in mind."
    )
}

/// AskUserQuestion 结果：有答案不在选项里（Other 文本等）。
pub(crate) fn user_answered(answers: &str) -> String {
    format!(
        "The user answered: {answers}. Read the answers carefully \u{2014} they may request clarification, changes, or that you not proceed \u{2014} and follow what they actually say."
    )
}

/// AskUserQuestion 结果：没有任何答案。
pub(crate) const NO_ANSWER: &str = "The user did not answer the questions.";

/// EnterPlanMode 的 validateInput。
pub(crate) const PLAN_MODE_MAIN_ONLY: &str = "Only the main session can enter plan mode.";

/// EnterPlanMode 的结果。
pub(crate) const ENTERED_PLAN_MODE: &str = "Entered plan mode. You should now focus on exploring the codebase and designing an implementation approach.

In plan mode, you should:
1. Thoroughly explore the codebase to understand existing patterns
2. Identify similar features and architectural approaches
3. Consider multiple approaches and their trade-offs
4. Use AskUserQuestion if you need to clarify the approach
5. Design a concrete implementation strategy
6. When ready, use ExitPlanMode to present your plan for approval

Remember: DO NOT write or edit any files yet. This is a read-only exploration and planning phase.";

/// ExitPlanMode 的 validateInput。
pub(crate) const NOT_IN_PLAN_MODE: &str = "You are not in plan mode. To enter plan mode, call the EnterPlanMode tool first. If your plan was already approved, continue with implementation.";

/// ExitPlanMode 结果：子代理。
pub(crate) const AGENT_PLAN_APPROVED: &str = "User has approved the plan. There is nothing else needed from you now. Please respond with \"ok\"";

/// ExitPlanMode 结果：没有计划。
pub(crate) const PLAN_EXIT_APPROVED: &str =
    "User has approved exiting plan mode. You can now proceed.";

/// ExitPlanMode 结果：有计划（agent teams 关闭时没有团队一段）。
pub(crate) fn plan_approved(path: &str, plan: &str) -> String {
    format!(
        "User has approved your plan. You can now start coding. Start with updating your todo list if applicable

Your plan has been saved to: {path}
You can refer back to it if needed during implementation.

## Approved Plan:
{plan}"
    )
}

/// ToolSearch 没有匹配。
pub(crate) const NO_DEFERRED_MATCH: &str = "No matching deferred tools found";
