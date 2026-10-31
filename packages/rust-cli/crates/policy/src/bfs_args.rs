//! bfs 4.1.4 的谓词与标志（工作线 F53 阶段 0，shell-permissions.md §4.6.1）：Bash 里的 `find` 换成内嵌的 bfs 之后，
//! guarded 的 `find-delete` 按 bfs 的表认谓词的值（`-rm` 等同 `-delete`）。出处 bfs `src/parse.c`：`parse_table`（谓词与
//! 它们的值）、`is_flag_group`（`-HLf` 这样的单字母标志组）、`parse_f`（`-f PATH`）、`parse_depth_n` / `parse_exit`。
//! 路径检查不用这里：find 的路径参数另有规则（`bash_paths.rs`，用户决定 2026-10-22）。

use crate::consts::{NEWER_XY_LETTERS, SHORT_OPTION_LEN};

/// 一个以 `-` 开头的词在 bfs 里的意思。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Arg {
    /// 标志（`-H`、`-L`、`-E` 等，`BFS_FLAG`）：不开始表达式；`values` 是后面跟着的值的个数。
    Flag { values: usize },
    /// 以 `-f` 结尾的标志（`-f`、`-Hf`）：下一个词是起点。
    RootFlag,
    /// 运算符 `-a` / `-and` / `-o` / `-or` / `-not` / `-exclude`。
    Operator,
    /// 谓词（测试、选项、动作）：开始表达式，后面跟 `values` 个值。
    Primary { values: usize },
    /// `-exec` / `-execdir` / `-ok` / `-okdir`：值到 `;` 为止，或到前一个词是 `{}` 的 `+` 为止。
    Exec,
    /// `-depth` / `-exit`：后面是数字时它是值（`parse_depth_n` 的 `looks_like_icmp`、`parse_exit` 的整数）。
    OptionalNumber,
}

/// `-fprintf FILE FORMAT` 后面的值的个数。
const FPRINTF_VALUES: usize = 2;

/// 不带值的谓词。
const NULLARY: [&str; 40] = [
    "--help",
    "--version",
    "-acl",
    "-capable",
    "-color",
    "-daystart",
    "-delete",
    "-empty",
    "-executable",
    "-false",
    "-follow",
    "-help",
    "-hidden",
    "-ignore_readdir_race",
    "-ls",
    "-mount",
    "-nocolor",
    "-noerror",
    "-nogroup",
    "-nohidden",
    "-noignore_readdir_race",
    "-noleaf",
    "-nouser",
    "-nowarn",
    "-print",
    "-print0",
    "-printx",
    "-prune",
    "-quit",
    "-readable",
    "-rm",
    "-sparse",
    "-status",
    "-true",
    "-unique",
    "-version",
    "-warn",
    "-writable",
    "-xattr",
    "-xdev",
];

/// 带一个值的谓词。
const UNARY: [&str; 53] = [
    "-Bmin",
    "-Bnewer",
    "-Bsince",
    "-Btime",
    "-amin",
    "-anewer",
    "-asince",
    "-atime",
    "-cmin",
    "-cnewer",
    "-context",
    "-csince",
    "-ctime",
    "-files0-from",
    "-flags",
    "-fls",
    "-fprint",
    "-fprint0",
    "-fstype",
    "-gid",
    "-group",
    "-ilname",
    "-iname",
    "-inum",
    "-ipath",
    "-iregex",
    "-iwholename",
    "-limit",
    "-links",
    "-lname",
    "-maxdepth",
    "-mindepth",
    "-mmin",
    "-mnewer",
    "-msince",
    "-mtime",
    "-name",
    "-newer",
    "-path",
    "-perm",
    "-printf",
    "-regex",
    "-regextype",
    "-samefile",
    "-since",
    "-size",
    "-type",
    "-uid",
    "-used",
    "-user",
    "-wholename",
    "-xattrname",
    "-xtype",
];

/// 单字母标志（`flag_lookup`）：`prefix` 是值可以直接接在后面的（`-D`、`-O`、`-S`、`-j`），`separate` 是值也可以
/// 写成下一个词的（`parse_prefix_flag` 的 `allow_separate`：`-D`、`-S`）。
struct Letter {
    letter: u8,
    prefix: bool,
    separate: bool,
}

const LETTERS: [Letter; 13] = [
    Letter::plain(b'E'),
    Letter::plain(b'H'),
    Letter::plain(b'L'),
    Letter::plain(b'P'),
    Letter::plain(b'X'),
    Letter::plain(b'd'),
    Letter::plain(b'f'),
    Letter::plain(b's'),
    Letter::plain(b'x'),
    Letter::prefix(b'D', true),
    Letter::prefix(b'O', false),
    Letter::prefix(b'S', true),
    Letter::prefix(b'j', false),
];

impl Letter {
    const fn plain(letter: u8) -> Self {
        Self {
            letter,
            prefix: false,
            separate: false,
        }
    }

    const fn prefix(letter: u8, separate: bool) -> Self {
        Self {
            letter,
            prefix: true,
            separate,
        }
    }
}

fn letter(byte: u8) -> Option<&'static Letter> {
    LETTERS.iter().find(|entry| entry.letter == byte)
}

/// `-newerXY`（`parse_newerxy`，前缀匹配）。
fn newer_xy(word: &str) -> bool {
    word.strip_prefix("-newer").is_some_and(|rest| {
        rest.len() == NEWER_XY_LETTERS && rest.bytes().all(|b| b"aBcmt".contains(&b))
    })
}

/// 单字母标志与标志组（`-H`、`-Sdfs`、`-HLf`；`is_flag_group` 要求组里至少一个大写字母，单个字母的标志直接
/// 查表）。不是标志时返回 None。
fn flag(word: &str) -> Option<Arg> {
    let bytes = word.as_bytes();
    if bytes.len() < SHORT_OPTION_LEN || bytes[0] != b'-' {
        return None;
    }
    // `table_lookup` 先按前缀匹配 `-D`、`-O`、`-S`、`-j`（`-Sdfs`、`-j4`、`-O3`），再看标志组。
    if let Some(entry) = letter(bytes[1]).filter(|entry| entry.prefix) {
        return match (bytes.len() > SHORT_OPTION_LEN, entry.separate) {
            (true, _) => Some(Arg::Flag { values: 0 }),
            (false, true) => Some(Arg::Flag { values: 1 }),
            (false, false) => None,
        };
    }
    let group = &bytes[1..];
    if group.len() > 1 && !group.iter().any(u8::is_ascii_uppercase) {
        return None;
    }
    for (index, byte) in group.iter().enumerate() {
        let entry = letter(*byte)?;
        let last = index + 1 == group.len();
        if entry.prefix {
            // 其余的字符是值；没有时值是下一个词（只有允许分开写的）。
            return match (last, entry.separate) {
                (false, _) => Some(Arg::Flag { values: 0 }),
                (true, true) => Some(Arg::Flag { values: 1 }),
                (true, false) => None,
            };
        }
        if entry.letter == b'f' {
            // 带值的标志只能在最后。
            return last.then_some(Arg::RootFlag);
        }
    }
    Some(Arg::Flag { values: 0 })
}

/// 一个以 `-` 开头的词（不含单独的 `-` 与 `--`）在 bfs 里的意思；bfs 不认识时返回 None（bfs 报错、不执行）。
pub(crate) fn classify(word: &str) -> Option<Arg> {
    match word {
        "-a" | "-and" | "-o" | "-or" | "-not" | "-exclude" => return Some(Arg::Operator),
        "-exec" | "-execdir" | "-ok" | "-okdir" => return Some(Arg::Exec),
        "-depth" | "-exit" => return Some(Arg::OptionalNumber),
        "-fprintf" => {
            return Some(Arg::Primary {
                values: FPRINTF_VALUES,
            });
        }
        _ => {}
    }
    if NULLARY.contains(&word) {
        return Some(Arg::Primary { values: 0 });
    }
    if UNARY.contains(&word) || newer_xy(word) {
        return Some(Arg::Primary { values: 1 });
    }
    flag(word)
}

/// `-depth N` 的 `looks_like_icmp` 与 `-exit N` 的整数：以可选的 `+` / `-` 开头、接着是数字。
pub(crate) fn number_like(word: &str) -> bool {
    word.trim_start_matches(['+', '-'])
        .bytes()
        .next()
        .is_some_and(|b| b.is_ascii_digit())
}

#[cfg(test)]
#[path = "bfs_args_tests.rs"]
mod tests;
