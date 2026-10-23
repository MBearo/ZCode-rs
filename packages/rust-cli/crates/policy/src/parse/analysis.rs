// 复制自 apps/zcode-cli-rust/crates/bash-parse/src/analysis.rs（07ab266d4b）；`crate::` 换成 `crate::parse::`。
//! Node `collectStatementCommands` / `collectSimpleCommand` / `freezeAnalysis` 的移植。
//!
//! 求值顺序与 Node 一致：先计算全部 argv、环境变量值和重定向目标（Node 对它们调用 `map`），
//! 动态判定则短路求值；heredoc 正文只在短路到达时才扫描。这决定了 unbash 死循环是否会被触发：
//! 触发时 Node 拿不到结果，这里记为 parse error（必然不安全）。

use std::collections::HashMap;

use crate::parse::js::{lossy, slice};
use crate::parse::lexer::RawSub;
use crate::parse::nodes::{CommandNode, OpBefore, RedirectNode, Script, WordRef};
use crate::parse::parts::{Structure, heredoc_structure, word_structure};
use crate::parse::{
    Analysis, EnvAssignment, Invocation, Operator, Redirect, Substitution, SubstitutionKind,
};

struct Resolved {
    value: Vec<u8>,
    dynamic: bool,
    /// 有结构化部件（unbash `Word.parts` 不为空）。
    structured: bool,
    /// 词里的替换（修复线 F68）。
    subs: Vec<RawSub>,
}

struct Collector<'a> {
    source: &'a [u8],
    hung: bool,
    has_dynamic_words: bool,
    has_redirects: bool,
    unsupported: Vec<String>,
    /// 按 `[pos, end)` 缓存重新扫描的结果：大量 heredoc 可能共用同一段陈旧目标范围。
    structures: HashMap<(usize, usize), Structure>,
}

pub(crate) fn collect(source: &[u8], script: Script) -> Analysis {
    if script.overflow {
        // Node：parse() 抛出 RangeError → catch 分支返回空分析 + hasParseErrors。
        return Analysis {
            has_parse_errors: true,
            ..Analysis::default()
        };
    }
    let mut collector = Collector {
        source,
        hung: script.hung,
        has_dynamic_words: false,
        has_redirects: false,
        unsupported: Vec::new(),
        structures: HashMap::new(),
    };
    let mut commands = Vec::with_capacity(script.commands.len());
    let mut parsed = script.commands.iter().peekable();
    for (statement, &background) in script.backgrounds.iter().enumerate() {
        if background {
            collector.mark_unsupported("background");
        }
        let inherited = (statement > 0).then_some(Operator::Sequence);
        while let Some(command) = parsed.next_if(|command| command.statement == statement) {
            let op = match command.op {
                OpBefore::Inherit => inherited,
                OpBefore::Op(op) => Some(op),
            };
            let mut invocation = collector.simple_command(&command.node, op, &script.redirects);
            if command.timed {
                invocation.words.insert(
                    0,
                    crate::parse::WordText {
                        text: "time".to_owned(),
                        structured: false,
                    },
                );
            }
            commands.push(invocation);
        }
    }
    if let Some(kind) = script.bail {
        collector.mark_unsupported(kind);
    }
    Analysis {
        commands,
        has_dynamic_words: collector.has_dynamic_words,
        has_parse_errors: script.lexer_errors > 0 || collector.hung,
        has_redirects: collector.has_redirects,
        has_unsupported_syntax: !collector.unsupported.is_empty(),
        unsupported_node_types: collector.unsupported,
    }
}

impl Collector<'_> {
    fn mark_unsupported(&mut self, kind: &str) {
        if !self.unsupported.iter().any(|existing| existing == kind) {
            self.unsupported.push(kind.to_owned());
        }
    }

    fn word(&mut self, word: &WordRef) -> Resolved {
        let source = self.source;
        let structure = self
            .structures
            .entry((word.pos, word.end))
            .or_insert_with(|| word_structure(source, word.pos, word.end));
        self.hung |= structure.hung;
        Resolved {
            structured: structure.value.is_some(),
            value: structure.value.clone().unwrap_or_else(|| word.text.clone()),
            dynamic: structure.dynamic,
            subs: structure.subs.clone(),
        }
    }

    fn simple_command(
        &mut self,
        node: &CommandNode,
        op: Option<Operator>,
        arena: &[RedirectNode],
    ) -> Invocation {
        let words: Vec<Resolved> = node
            .name
            .iter()
            .chain(&node.suffix)
            .map(|word| self.word(word))
            .collect();
        let values: Vec<Option<Resolved>> = node
            .prefix
            .iter()
            .map(|assignment| assignment.value.as_ref().map(|value| self.word(value)))
            .collect();
        let redirects: Vec<&RedirectNode> =
            node.redirects.iter().map(|&index| &arena[index]).collect();
        let targets: Vec<Resolved> = redirects
            .iter()
            .map(|redirect| self.word(&redirect.target))
            .collect();

        let mut has_dynamic_words = words.iter().any(|word| word.dynamic)
            || values.iter().flatten().any(|value| value.dynamic);
        if !has_dynamic_words {
            for (redirect, target) in redirects.iter().zip(&targets) {
                if target.dynamic {
                    has_dynamic_words = true;
                    break;
                }
                if let Some(body) = redirect.body {
                    let body = heredoc_structure(self.source, body.pos, body.end);
                    self.hung |= body.hung;
                    if body.dynamic {
                        has_dynamic_words = true;
                        break;
                    }
                }
            }
        }
        self.has_dynamic_words |= has_dynamic_words;
        self.has_redirects |= !redirects.is_empty();

        let substitutions = self.substitutions(node, &words, &values, &redirects, &targets);
        let name = if node.name.is_some() {
            lossy(&words[0].value)
        } else {
            String::new()
        };
        let texts = node
            .name
            .iter()
            .chain(&node.suffix)
            .zip(&words)
            .map(|(word, resolved)| crate::parse::WordText {
                text: lossy(slice(self.source, word.pos, word.end)),
                structured: resolved.structured,
            })
            .collect();
        Invocation {
            words: texts,
            argv: words.iter().map(|word| lossy(&word.value)).collect(),
            command_text: lossy(slice(self.source, node.pos, node.end)),
            env_assignments: node
                .prefix
                .iter()
                .zip(&values)
                .map(|(assignment, value)| EnvAssignment {
                    name: assignment.name.as_deref().map(lossy),
                    value: value.as_ref().map(|value| lossy(&value.value)),
                })
                .collect(),
            has_assignment_prefix: !node.prefix.is_empty(),
            has_dynamic_words,
            has_redirects: !redirects.is_empty(),
            name,
            operator_before: op,
            redirects: redirects
                .iter()
                .zip(&targets)
                .map(|(redirect, target)| Redirect {
                    file_descriptor: redirect.fd,
                    operator: redirect.operator.to_owned(),
                    target: lossy(&target.value),
                })
                .collect(),
            substitutions,
        }
    }

    /// 修复线 F68（shell-permissions.md §4.5.2）：命令里的替换按出现的顺序。双引号里的命令替换、整个赋值的值就是
    /// 一个命令替换时能拆成单独的子命令；其余（不在双引号里的参数、重定向目标、heredoc 正文）不能拆。
    fn substitutions(
        &self,
        node: &CommandNode,
        words: &[Resolved],
        values: &[Option<Resolved>],
        redirects: &[&RedirectNode],
        targets: &[Resolved],
    ) -> Vec<Substitution> {
        let convert = |sub: &RawSub, whole: bool| Substitution {
            kind: sub.kind,
            splittable: sub.kind == SubstitutionKind::Command && (sub.quoted || whole),
            inner: lossy(&sub.inner),
        };
        let mut found = Vec::new();
        for (assignment, value) in node.prefix.iter().zip(values) {
            let (Some(word), Some(value)) = (&assignment.value, value) else {
                continue;
            };
            let text = slice(self.source, word.pos, word.end);
            found.extend(value.subs.iter().map(|sub| convert(sub, sub.raw == text)));
        }
        for word in words {
            found.extend(word.subs.iter().map(|sub| convert(sub, false)));
        }
        for (redirect, target) in redirects.iter().zip(targets) {
            found.extend(target.subs.iter().map(|sub| Substitution {
                splittable: false,
                ..convert(sub, false)
            }));
            // 不加引号的 heredoc 正文里的 `$(` 与反引号同样执行（正文没有加引号的规则，按原文找，`\$(` 不算）。
            if let Some(body) = redirect.body
                && heredoc_runs_a_command(slice(self.source, body.pos, body.end))
            {
                found.push(Substitution {
                    kind: SubstitutionKind::Hidden,
                    splittable: false,
                    inner: String::new(),
                });
            }
        }
        found
    }
}

/// 不加引号的 heredoc 正文里有没有被转义的 `$(` 或反引号。
fn heredoc_runs_a_command(body: &[u8]) -> bool {
    let mut escaped = false;
    for (index, &byte) in body.iter().enumerate() {
        if escaped {
            escaped = false;
            continue;
        }
        match byte {
            b'\\' => escaped = true,
            b'`' => return true,
            b'$' if body.get(index + 1) == Some(&b'(') => return true,
            _ => {}
        }
    }
    false
}
