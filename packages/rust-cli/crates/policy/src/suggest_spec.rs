//! 询问框种子的规格细化：按命令的规格与深度表取更长的前缀（docs/specs/rust-cli/shell-permissions.md §4.10，
//! 工作线 F72）。只有 7 个内置规格（外部规格库在单个可执行文件里不加载），都没有子命令。

use crate::parse::analyze;
use crate::suggest::is_assignment;

/// 规格里的一个参数。
#[derive(Clone, Copy)]
struct Arg {
    command: bool,
    variadic: bool,
}

/// 规格的参数：一个，或按位置的一组（取深度时两者算法不同）。
#[derive(Clone, Copy)]
enum Args {
    One(Arg),
    Many(&'static [Arg]),
}

impl Args {
    fn list(&self) -> &[Arg] {
        match self {
            Self::One(arg) => std::slice::from_ref(arg),
            Self::Many(list) => list,
        }
    }
}

struct Spec {
    name: &'static str,
    /// 带值的选项（内置规格里没有值是命令或模块的选项；不带值的选项与规格里没有的选项一样处理）。
    valued: &'static [&'static str],
    args: Args,
}

const COMMAND: Arg = Arg {
    command: true,
    variadic: false,
};
/// 必填的普通参数（timeout 的时长、sleep 的时长）。
const REQUIRED: Arg = Arg {
    command: false,
    variadic: false,
};
/// 可选、可变的参数（alias 的定义、pyright 的文件）。
const OPTIONAL_MANY: Arg = Arg {
    command: false,
    variadic: true,
};

const SPECS: [Spec; 7] = [
    Spec {
        name: "pyright",
        valued: &[
            "--project",
            "-p",
            "--createstub",
            "--typeshedpath",
            "-t",
            "--verifytypes",
            "--pythonpath",
            "--pythonplatform",
            "--pythonversion",
            "--venvpath",
            "-v",
            "--level",
            "--threads",
        ],
        args: Args::One(OPTIONAL_MANY),
    },
    Spec {
        name: "timeout",
        valued: &[],
        args: Args::Many(&[REQUIRED, COMMAND]),
    },
    Spec {
        name: "sleep",
        valued: &[],
        args: Args::One(REQUIRED),
    },
    Spec {
        name: "alias",
        valued: &[],
        args: Args::One(OPTIONAL_MANY),
    },
    Spec {
        name: "nohup",
        valued: &[],
        args: Args::One(COMMAND),
    },
    Spec {
        name: "time",
        valued: &[],
        args: Args::One(COMMAND),
    },
    Spec {
        name: "srun",
        valued: &["-n", "--ntasks", "-N", "--nodes"],
        args: Args::One(COMMAND),
    },
];

/// 深度表：`<命令> <子命令>` 或 `<命令>`（小写）给出前缀的词数。
const DEPTHS: [(&str, usize); 11] = [
    ("rg", 2),
    ("pre-commit", 2),
    ("gcloud", 4),
    ("gcloud compute", 6),
    ("gcloud beta", 6),
    ("aws", 4),
    ("az", 4),
    ("kubectl", 3),
    ("docker", 3),
    ("dotnet", 3),
    ("git push", 2),
];

/// 包装最多套 3 层。
const MAX_DEPTH: usize = 2;

/// `^\d+$`。
fn is_digits(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit())
}

fn spec(name: &str) -> Option<&'static Spec> {
    SPECS.iter().find(|spec| spec.name == name)
}

fn table(key: &str) -> Option<usize> {
    DEPTHS
        .iter()
        .find(|(name, _)| *name == key)
        .map(|(_, depth)| *depth)
}

/// 选项带不带值（规格里没有这个选项时不带）。
fn takes_value(flag: &str, spec: Option<&Spec>) -> bool {
    spec.is_some_and(|spec| spec.valued.contains(&flag))
}

/// 第一个操作数：跳过选项与它们的值（内置规格没有子命令，第一个不以 `-` 开头的参数就是）。
fn first_operand<'a>(args: &'a [String], spec: Option<&Spec>) -> Option<&'a str> {
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        if arg.starts_with('-') {
            if takes_value(arg, spec) {
                index += 1;
            }
        } else if !arg.is_empty() {
            return Some(arg);
        }
        index += 1;
    }
    None
}

/// 前缀的词数：深度表优先，没有规格时 2，其余按规格参数的结构。
fn depth(name: &str, args: &[String], spec: Option<&Spec>) -> usize {
    let lower = name.to_lowercase();
    let keyed =
        first_operand(args, spec).map(|operand| format!("{lower} {}", operand.to_lowercase()));
    if let Some(depth) = keyed.as_deref().and_then(table).or_else(|| table(&lower)) {
        return depth;
    }
    let Some(spec) = spec else {
        return 2;
    };
    let list = spec.args.list();
    if let Some(at) = list.iter().position(|arg| arg.command) {
        return match spec.args {
            Args::One(_) => 2,
            Args::Many(_) => (2 + at).min(3),
        };
    }
    // 没有子命令时：参数可变 1，其余 2（第一个参数必填 2、参数危险 3，内置规格里没有危险的参数）。
    if list.iter().any(|arg| arg.variadic) {
        1
    } else {
        2
    }
}

/// 像文件或网址的参数：前缀到它之前为止。
fn looks_like_a_path(arg: &str) -> bool {
    let extension = arg
        .rfind('.')
        .is_some_and(|dot| dot > 0 && dot < arg.len() - 1 && !arg[dot + 1..].contains(':'));
    arg.contains('/')
        || extension
        || ["http://", "https://", "ftp://"]
            .iter()
            .any(|scheme| arg.starts_with(scheme))
}

/// 不是包装的命令：从命令名起逐个接上参数，到了深度、遇到选项或像路径的参数就停。
fn build(name: &str, args: &[String], spec: Option<&Spec>) -> String {
    let limit = depth(name, args, spec);
    let mut words = vec![name];
    for arg in args {
        if arg.is_empty() || words.len() >= limit {
            break;
        }
        if arg.starts_with('-') || looks_like_a_path(arg) {
            break;
        }
        words.push(arg);
    }
    words.join(" ")
}

/// 包装（`nice`、参数是命令的规格）：命令那一项之前的普通参数照抄，从命令起的部分递归。
fn wrapped(name: &str, args: &[String], level: usize) -> Option<String> {
    if let Some(at) =
        spec(name).and_then(|spec| spec.args.list().iter().position(|arg| arg.command))
    {
        let mut words = vec![name.to_owned()];
        for (index, arg) in args.iter().enumerate().take(at + 1) {
            if index == at {
                if let Some(inner) = prefix_at(&args[index..].join(" "), level + 1) {
                    words.push(inner);
                    return Some(words.join(" "));
                }
                break;
            }
            if !arg.is_empty() && !arg.starts_with('-') && !is_assignment(arg) {
                words.push(arg.clone());
            }
        }
    }
    let Some(start) = args
        .iter()
        .position(|arg| !arg.starts_with('-') && !is_digits(arg) && !is_assignment(arg))
    else {
        return Some(name.to_owned());
    };
    if args[start].is_empty() {
        return Some(name.to_owned());
    }
    let inner = prefix_at(&args[start..].join(" "), level + 1)?;
    Some(format!("{name} {inner}"))
}

/// 命令开头的变量赋值（原文）。
pub(crate) fn leading_assignments(text: &str) -> Vec<&str> {
    crate::suggest::words(text)
        .into_iter()
        .take_while(|word| is_assignment(word))
        .collect()
}

fn prefix_at(command: &str, level: usize) -> Option<String> {
    if level > MAX_DEPTH {
        return None;
    }
    let analysis = analyze(command);
    let first = analysis.commands.first()?;
    // 管道前的 `time` 是关键字，解析结果的 argv 里没有它（词里有）；规格把它当包装。
    let mut argv = first.argv.clone();
    if first.words.first().is_some_and(|word| word.text == "time")
        && argv.first().is_none_or(|name| name != "time")
    {
        argv.insert(0, "time".to_owned());
    }
    let (name, args) = argv.split_first()?;
    if name.is_empty() {
        return None;
    }
    let wrapper = name == "nice"
        || spec(name).is_some_and(|spec| spec.args.list().iter().any(|arg| arg.command));
    let prefix = if wrapper {
        wrapped(name, args, level)?
    } else {
        build(name, args, spec(name))
    };
    if prefix.is_empty() {
        return None;
    }
    let assignments = leading_assignments(&first.command_text);
    Some(if assignments.is_empty() {
        prefix
    } else {
        format!("{} {prefix}", assignments.join(" "))
    })
}

/// 第一条简单命令按规格与深度表的前缀（开头的变量赋值原样放在前面）。
pub(crate) fn spec_prefix(command: &str) -> Option<String> {
    prefix_at(command, 0)
}

#[cfg(test)]
#[path = "suggest_spec_tests.rs"]
mod tests;
