// 复制自 apps/zcode-cli-rust/crates/bash-parse/src/nodes.rs（07ab266d4b）；`crate::` 换成 `crate::parse::`。
//! 解析结果：只保留分析需要的简单命令信息（unbash `Command` / `Redirect` / `AssignmentPrefix`）。

use std::ops::Range;

use crate::parse::Operator;
use crate::parse::lexer::HereDocBody;

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
    /// 重定向（含 fd）在原文里的起点（工作线 F85：规则候选的“去掉重定向”按它截，shell-permissions.md §4.5.6）。
    pub start: usize,
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
    /// 这条命令是带 `time` 关键字的管道里的第一条（修复线 F20，guarded 的词）。
    pub timed: bool,
}

/// 不能静态检查的写法（工作线 F76，shell-permissions.md §4.5.4 的表）；询问的文字在 `bash_compound`。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Complex {
    /// 通用的说法：子 shell 里直接用 `;` / `&` 分隔、循环体或分支里用 `&` 分隔、coproc。
    Generic,
    /// 带重定向的子 shell。
    Subshell,
    /// 花括号组、`(( … ))`。
    Braces,
    /// 带重定向的 for。
    For,
    CountingFor,
    /// 带重定向的 while / until。
    While,
    /// 带重定向的 if。
    If,
    Case,
    Function,
    /// 带重定向的 `[[ … ]]`。
    Test,
    Select,
    /// 引用了循环变量，或 for 的词表里有变量展开。
    Variable,
    VariableBraces,
    BracePattern,
    /// for 的词表里有命令替换。
    Nested,
    /// for 的词表里有进程替换。
    AsFile,
    /// 循环变量是会影响执行或由 shell 维护的名字。
    LoopVariable(String),
}

/// 结构化解析记下的结构事实（工作线 F76）。
#[derive(Default)]
pub(crate) struct Shape {
    /// 有子 shell（任意层）。
    pub subshell: bool,
    /// 顶层有 `&`。
    pub background: bool,
    /// 不能拆的写法：（源码位置，种类）；取位置最前的一个。
    pub complex: Vec<(usize, Complex)>,
    /// for / select 的循环变量。
    pub loops: Vec<Loop>,
}

/// 一个 for / select 循环。
pub(crate) struct Loop {
    /// 循环变量的名字。
    pub name: Vec<u8>,
    /// `for` 的位置。
    pub pos: usize,
    /// `do` 之后的位置：从这里到命令结尾引用循环变量都算。
    pub body: usize,
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
    /// 结构化模式的结构事实（工作线 F76）。
    pub shape: Shape,
    /// 管道符（`|`、`|&`）在原文里的范围，任意层（工作线 F80：管道里一段被拒绝时文字取那一段）。
    pub pipes: Vec<Range<usize>>,
    /// `>` / `>>` 重定向（含 fd 与目标）在原文里的范围（工作线 F80：那一段的文字去掉它们）。
    pub outputs: Vec<Range<usize>>,
}
