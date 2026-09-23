//! 解析结果：只保留分析需要的简单命令信息（unbash `Command` / `Redirect` / `AssignmentPrefix`）。

use crate::Operator;
use crate::lexer::HereDocBody;

pub(crate) struct WordRef {
    /// `WordImpl.text`：普通单词是源码切片；重定向目标是扫描时的去引号文本（可能是陈旧值）。
    pub text: Vec<u8>,
    pub pos: usize,
    pub end: usize,
}

pub(crate) struct AssignmentNode {
    pub name: Option<Vec<u8>>,
    /// 数组赋值 `a=(...)` 没有 value，其元素也不参与动态判定。
    pub value: Option<WordRef>,
}

pub(crate) struct RedirectNode {
    pub operator: &'static str,
    pub fd: Option<u32>,
    pub target: WordRef,
    pub body: Option<HereDocBody>,
}

pub(crate) struct CommandNode {
    pub pos: usize,
    pub end: usize,
    pub name: Option<WordRef>,
    pub prefix: Vec<AssignmentNode>,
    pub suffix: Vec<WordRef>,
    /// 下标指向 `Script::redirects`。
    pub redirects: Vec<usize>,
}

#[derive(Clone, Copy)]
pub(crate) enum OpBefore {
    /// 语句中的第一条命令继承语句级的 operator（首条语句无，其余为 "sequence"）。
    Inherit,
    Op(Operator),
}

pub(crate) struct ParsedCommand {
    pub statement: usize,
    pub op: OpBefore,
    pub node: CommandNode,
}

#[derive(Default)]
pub(crate) struct Script {
    pub commands: Vec<ParsedCommand>,
    pub redirects: Vec<RedirectNode>,
    pub backgrounds: Vec<bool>,
    /// 首个遇到的复合命令节点类型（Node `unsupportedNodeTypes` 中的名字）。
    pub bail: Option<&'static str>,
    pub lexer_errors: usize,
    /// unbash 会死循环：Node 拿不到任何结果。
    pub hung: bool,
    /// 解析阶段嵌套过深：Node 抛出 RangeError，被 `analyzeBashCommand` 捕获为 parse error。
    pub overflow: bool,
}
