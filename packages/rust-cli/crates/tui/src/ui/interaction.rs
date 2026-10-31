// 改写自 apps/zcode-host/crates/tui/src/ui/interaction.rs（feat/rs 79b6691c55）：输入换成
// `InteractionRequest`，回答换成 kernel-api 的回答类型（docs/specs/rust-cli/tui.md §5.2）。
//! 审批、提问与计划审批：内容来自视图的待处理交互，这里只保存选择状态。弹出后 150 ms 内的按键不生效
//! （防止连按误批）。
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent};
use rust_cli_kernel_api::ids::InteractionId;
use rust_cli_kernel_api::orchestration::{
    InteractionRequest, PermissionAnswer, PlanApprovalAnswer, QuestionAnnotation, QuestionAnswer,
};
use rust_cli_kernel_api::permission::{PermissionMode, PermissionRule};
use serde_json::Value;

use super::keyboard::Typing;

pub use super::interaction_view::View;

/// 弹出后忽略按键的时长。
const GUARD: Duration = Duration::from_millis(150);
/// 权限询问：Yes、No 两行；有建议规则时中间多一行“不再询问”。
const YES_NO_ROWS: usize = 2;
const WITH_SEED_ROWS: usize = YES_NO_ROWS + 1;

/// 权限询问的行数：Yes、No；有建议规则时中间多一行"不再询问"。
pub(super) fn permission_rows(suggestions: &[PermissionRule]) -> usize {
    if suggestions.is_empty() {
        YES_NO_ROWS
    } else {
        WITH_SEED_ROWS
    }
}
/// 计划审批的选项：自动编辑、手动审批、继续计划（输入）。
/// 计划审批的行数（修复线 F20：批准 / 继续计划；原来是三行，两种批准各换一个模式）。
pub(super) const PLAN_ROWS: usize = 2;

pub struct Question {
    pub text: String,
    pub header: String,
    /// （标签，说明）。回答的值就是标签（AskUserQuestion）。
    pub options: Vec<(String, String)>,
    /// 每个选项的预览（markdown 原文），与 `options` 一一对应。
    pub previews: Vec<Option<String>>,
    pub multi: bool,
}

impl Question {
    /// 预览视图：单选且至少一个选项有预览（工作线 F72，tools-more.md §1.1）；这种题没有 `Other` 行，可以写备注。
    pub fn preview_mode(&self) -> bool {
        !self.multi && self.previews.iter().any(Option::is_some)
    }

    /// 选项的预览。
    pub fn preview_of(&self, label: &str) -> Option<&str> {
        let index = self.options.iter().position(|(value, _)| value == label)?;
        self.previews.get(index)?.as_deref()
    }
}

/// AskUserQuestion 输入里的问题。
pub fn questions(request: &InteractionRequest) -> Vec<Question> {
    let InteractionRequest::Question { questions, .. } = request else {
        return Vec::new();
    };
    questions
        .as_array()
        .into_iter()
        .flatten()
        .map(|q| Question {
            text: q["question"].as_str().unwrap_or_default().to_owned(),
            header: q["header"].as_str().unwrap_or_default().to_owned(),
            options: q["options"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|o| {
                    (
                        o["label"].as_str().unwrap_or_default().to_owned(),
                        o["description"].as_str().unwrap_or_default().to_owned(),
                    )
                })
                .collect(),
            previews: q["options"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|o| o["preview"].as_str().map(str::to_owned))
                .collect(),
            multi: q["multiSelect"] == true,
        })
        .collect()
}

#[derive(Default, Clone)]
pub struct Draft {
    pub values: Vec<String>,
    /// `Other` 行输入的回答（发送时排在选中的值之后）。
    pub notes: String,
}

impl Draft {
    pub fn answers(&self) -> Vec<String> {
        let mut answers = self.values.clone();
        if !self.notes.trim().is_empty() {
            answers.push(self.notes.trim().to_owned());
        }
        answers
    }
}

pub struct Interaction {
    pub id: InteractionId,
    /// 焦点行。
    pub cursor: usize,
    /// 提问的当前标签页；等于题数时是确认页（多于一题时才有）。
    pub question: usize,
    pub drafts: Vec<Draft>,
    /// 正在输入：审批的意见、计划的反馈或提问的 `Other`。
    pub typing: bool,
    /// 审批的意见与计划的反馈。
    pub text: String,
    /// 弹出时会话的权限模式：计划审批的 `Approve{mode}` 填它（只作记录，退出 plan 不改权限模式，修复线 F20）。
    pub mode: PermissionMode,
    /// Bash 询问“不再询问”一行的可编辑前缀（工作线 F72，shell-permissions.md §4.10）；没有时这一行列出建议规则。
    pub seed: Option<String>,
    /// 当前工作目录：没有种子时“不再询问”一行里显示（远端的调用是那台机器的工作目录，F98）。
    pub workspace: String,
    /// MCP 工具绑定的远端机器（F98，remote-exec.md §25.4.3）：询问里没有 `machine` 时标题照它写“在 dev-a 上执行”。
    pub bound_machine: Option<String>,
    /// 旧会话的询问（父会话已关闭，agents.md §4.1.2，工作线 F72）：那个会话的标题（可能为空）；当前会话的询问为 None。
    pub origin: Option<String>,
    /// 预览题的焦点在这道题上动过（写完备注按 enter 时选焦点所在的选项，工作线 F72）。
    pub moved: bool,
    /// 提问的空闲倒计时（秒）：快到期时界面显示；值来自 `App::afk`，事件循环在视图变化与每次检查后复制过来
    /// （tools-more.md §1.1）。
    pub countdown: Option<u64>,
    shown: Instant,
}

fn json(value: impl serde::Serialize) -> Option<Value> {
    serde_json::to_value(value).ok()
}

impl Interaction {
    pub fn new(id: InteractionId, request: &InteractionRequest, now: Instant) -> Self {
        Self {
            id,
            cursor: 0,
            question: 0,
            drafts: vec![Draft::default(); questions(request).len()],
            typing: false,
            text: String::new(),
            mode: PermissionMode::Default,
            seed: remember::seed(request),
            workspace: String::new(),
            bound_machine: None,
            origin: None,
            moved: false,
            countdown: None,
            shown: now,
        }
    }

    /// 焦点在可编辑的前缀上（“不再询问”一行，没有在输入意见）。
    pub(super) fn editing_seed(&self, suggestions: &[PermissionRule]) -> bool {
        !self.typing
            && self.seed.is_some()
            && permission_rows(suggestions) == WITH_SEED_ROWS
            && self.cursor == 1
    }

    /// 粘贴进正在输入的那一行：审批的意见、计划的反馈或提问的 `Other`。
    pub fn paste(&mut self, request: &InteractionRequest, text: &str) {
        let text = text.replace(['\r', '\n'], " ");
        if let InteractionRequest::Permission { suggestions, .. } = request
            && self.editing_seed(suggestions)
            && let Some(seed) = self.seed.as_mut()
        {
            seed.push_str(&text);
            return;
        }
        if !self.typing {
            return;
        }
        match request {
            InteractionRequest::Question { .. } => {
                if let Some(draft) = self.drafts.get_mut(self.question) {
                    draft.notes.push_str(&text);
                }
            }
            InteractionRequest::Permission { .. } | InteractionRequest::PlanApproval { .. } => {
                self.text.push_str(&text);
            }
        }
    }

    /// 按键；作答时返回回答（`ResolveInteraction` 的 `answer`）。`input` 按平台判断文字输入（Windows 的 AltGr，
    /// tui-input.md §9.1）。
    pub fn on_key(
        &mut self,
        request: &InteractionRequest,
        key: KeyEvent,
        now: Instant,
        input: Typing,
    ) -> Option<Value> {
        if now.saturating_duration_since(self.shown) < GUARD {
            return None;
        }
        match request {
            InteractionRequest::Permission { suggestions, .. } => {
                self.permission_key(key, suggestions, input)
            }
            InteractionRequest::PlanApproval { .. } => self.plan_key(key, input),
            InteractionRequest::Question { .. } => self.question_key(request, key, input),
        }
    }

    /// 输入一行文字（意见、反馈）：返回 true 表示按键已处理。
    fn edit_text(&mut self, key: KeyEvent, input: Typing) -> bool {
        match key.code {
            KeyCode::Backspace => {
                self.text.pop();
            }
            KeyCode::Char(c) if input.is_text(&key) => self.text.push(c),
            _ => return false,
        }
        true
    }

    fn deny(&self) -> Option<Value> {
        let feedback = Some(self.text.trim().to_owned()).filter(|text| !text.is_empty());
        json(PermissionAnswer::Deny { feedback })
    }

    /// 权限询问：1. Yes；（有建议规则时）2. Yes，不再询问；最后一行 No。任何一行按 tab 都补充意见
    /// （焦点移到 No 行），esc 拒绝。
    fn permission_key(
        &mut self,
        key: KeyEvent,
        suggestions: &[PermissionRule],
        input: Typing,
    ) -> Option<Value> {
        let rows = permission_rows(suggestions);
        let last = rows - 1;
        if self.typing {
            match key.code {
                KeyCode::Enter => return self.deny(),
                KeyCode::Esc | KeyCode::Tab => self.typing = false,
                _ => {
                    self.edit_text(key, input);
                }
            }
            return None;
        }
        // 焦点在可编辑的前缀上：键入的字（含数字）改前缀（工作线 F72）。
        if self.editing_seed(suggestions)
            && let Some(seed) = self.seed.as_mut()
        {
            match key.code {
                KeyCode::Backspace => {
                    seed.pop();
                    return None;
                }
                KeyCode::Char(c) if input.is_text(&key) => {
                    seed.push(c);
                    return None;
                }
                _ => {}
            }
        }
        let allow = |remember: &[PermissionRule]| {
            json(PermissionAnswer::Allow {
                remember: remember.to_vec(),
            })
        };
        let choose = |this: &Self, row: usize| match row {
            0 => allow(&[]),
            row if row == last => this.deny(),
            // 有种子时记住种子（可能改过）得出的规则，否则记住全部建议规则。
            _ => match &this.seed {
                Some(seed) => allow(&remember::rules(seed)),
                None => allow(suggestions),
            },
        };
        match key.code {
            KeyCode::Up => self.cursor = (self.cursor + last) % rows,
            KeyCode::Down => self.cursor = (self.cursor + 1) % rows,
            KeyCode::Tab => {
                self.cursor = last;
                self.typing = true;
            }
            KeyCode::Esc => return self.deny(),
            KeyCode::Char(digit @ '1'..='3') => {
                let row = usize::from(digit as u8 - b'1');
                if row < rows {
                    return choose(self, row);
                }
            }
            KeyCode::Enter => return choose(self, self.cursor),
            _ => {}
        }
        None
    }

    /// 计划审批（修复线 F20，按 Node 的计划审批）：1. 批准（只关掉 plan，权限模式不变）；2. 继续计划（这一行直接是
    /// 输入）。
    fn plan_key(&mut self, key: KeyEvent, input: Typing) -> Option<Value> {
        let approve = |mode| json(PlanApprovalAnswer::Approve { mode });
        let mode = self.mode;
        let keep = |this: &Self| {
            let feedback = Some(this.text.trim().to_owned()).filter(|text| !text.is_empty());
            json(PlanApprovalAnswer::KeepPlanning { feedback })
        };
        let last = PLAN_ROWS - 1;
        match key.code {
            KeyCode::Up => self.cursor = (self.cursor + last) % PLAN_ROWS,
            KeyCode::Down => self.cursor = (self.cursor + 1) % PLAN_ROWS,
            KeyCode::Char('1') if !self.typing => return approve(mode),
            KeyCode::Char('2') if !self.typing => self.cursor = last,
            KeyCode::Enter if self.cursor == 0 => return approve(mode),
            KeyCode::Enter | KeyCode::Esc => return keep(self),
            _ if self.typing => {
                self.edit_text(key, input);
            }
            _ => {}
        }
        self.typing = self.cursor == last;
        None
    }

    pub(super) fn decline(&self) -> Option<Value> {
        json(QuestionAnswer::Declined)
    }

    /// 提交提问的回答：问题文字 → 选中的标签（多选用 `, ` 连接）。
    pub(super) fn submit(&self, list: &[Question]) -> Option<Value> {
        self.submit_with(list, None)
    }

    /// 提交；预览题另带选中选项的预览与备注（`annotations`），空闲到期时带超时的毫秒数（工作线 F72）。
    fn submit_with(&self, list: &[Question], afk_timeout_ms: Option<u64>) -> Option<Value> {
        let mut answers = BTreeMap::new();
        let mut annotations = BTreeMap::new();
        for (question, draft) in list.iter().zip(&self.drafts) {
            if !question.preview_mode() {
                let values = draft.answers();
                if !values.is_empty() {
                    answers.insert(question.text.clone(), values.join(", "));
                }
                continue;
            }
            if !draft.values.is_empty() {
                answers.insert(question.text.clone(), draft.values.join(", "));
            }
            let preview = draft
                .values
                .first()
                .and_then(|label| question.preview_of(label))
                .map(str::to_owned);
            let notes = Some(draft.notes.trim().to_owned()).filter(|notes| !notes.is_empty());
            if preview.is_some() || notes.is_some() {
                annotations.insert(question.text.clone(), QuestionAnnotation { preview, notes });
            }
        }
        json(QuestionAnswer::Answered {
            answers,
            annotations,
            response: None,
            follow_up: false,
            afk_timeout_ms,
        })
    }

    /// 提问空闲到期：按当时已选的答案与备注提交（tools-more.md §1.1）。
    pub fn idle_answer(&self, request: &InteractionRequest, timeout_ms: u64) -> Option<Value> {
        matches!(request, InteractionRequest::Question { .. })
            .then(|| self.submit_with(&questions(request), Some(timeout_ms)))
            .flatten()
    }
}

#[path = "interaction_question.rs"]
pub(super) mod question;

#[path = "interaction_remember.rs"]
pub(super) mod remember;

#[path = "interaction_preview.rs"]
mod preview;

#[cfg(test)]
#[path = "interaction_tests.rs"]
mod tests;
