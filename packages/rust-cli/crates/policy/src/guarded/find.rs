// 移植自 apps/zcode-cli/packages/core/src/tool/handlers/guarded/find.ts 与 windows-delete.ts 的 robocopy
// （修复线 F20；CMD 的 rd / del 不移植：Bash 工具总用 bash）。
//! `find … -delete` 与 Git Bash 下的 `robocopy /MIR`。

use super::options::Hit;
use super::word::Word;
use crate::bfs_args::{Arg, classify, number_like};
use crate::consts::MIN_COPY_OPERANDS;

fn listed(list: &str, value: &str) -> bool {
    list.split(' ').any(|item| item == value)
}

/// Node `matchFindDelete`：有限的 find 表达式；arity 未知之后不把参数值猜成动作，不递归分析 exec 的载荷。
///
/// 工作线 F53（shell-permissions.md §4.5.1、§4.6.1）：find 换成内嵌的 bfs 之后，谓词与标志按 bfs 4.1.4 的表认
/// （`crate::bfs_args`，Node 的表是它的子集），`-rm` 等同 `-delete`；写在表达式后面的非选项词是 bfs 的起点，
/// 不再当作未知的文法（以前 `find . -name x /tmp -rm` 是 Unknown，不询问）；`--` 被 bfs 忽略。
pub(crate) fn find_delete(args: &[Word]) -> Hit {
    let mut expression = false;
    let mut i = 0;
    while i < args.len() {
        let word = &args[i];
        let value = word.value.as_str();
        let operand = !value.starts_with('-') || value == "-";
        if !expression && operand && value != "!" && value != "(" {
            i += 1;
            continue;
        }
        if word.dynamic {
            return Hit::Unknown;
        }
        if operand {
            // `(`、`!` 开始表达式；`)`、`,` 与其余的词（bfs 的起点）跳过。
            expression |= value == "(" || value == "!";
            i += 1;
            continue;
        }
        if value == "--" {
            i += 1;
            continue;
        }
        let present = |count: usize| args.len() > i + count;
        match classify(value) {
            Some(Arg::Flag { values }) => {
                if !present(values) {
                    return Hit::Unknown;
                }
                i += values;
            }
            Some(Arg::RootFlag) => {
                if !present(1) {
                    return Hit::Unknown;
                }
                i += 1;
            }
            Some(Arg::Operator) => expression = true,
            Some(Arg::Primary { values }) => {
                expression = true;
                if value == "-delete" || value == "-rm" {
                    return Hit::Rule("find-delete");
                }
                if !present(values) {
                    return Hit::Unknown;
                }
                i += values;
            }
            Some(Arg::OptionalNumber) => {
                expression = true;
                if args
                    .get(i + 1)
                    .is_some_and(|next| !next.dynamic && number_like(&next.value))
                {
                    i += 1;
                }
            }
            Some(Arg::Exec) => {
                expression = true;
                let mut closed = false;
                i += 1;
                while i < args.len() {
                    let token = &args[i];
                    if !token.dynamic
                        && (token.value == ";"
                            || (token.value == "+"
                                && args[i - 1].value == "{}"
                                && (value == "-exec" || value == "-execdir")))
                    {
                        closed = true;
                        break;
                    }
                    i += 1;
                }
                if !closed {
                    return Hit::Unknown;
                }
            }
            None => return Hit::Unknown,
        }
        i += 1;
    }
    Hit::No
}

const ROBOCOPY_FLAGS: &str = "/MIR /PURGE /L /S /E /Z /B /ZB /J /EFSRAW /SEC /COPYALL /NOCOPY /SECFIX /TIMFIX /CREATE /FAT /256 /PF /SJ /SL /NODCOPY /NOOFFLOAD /COMPRESS /A /M /XC /XN /XO /XX /XL /IM /IS /IT /XJ /FFT /DST /XJD /XJF /TBD /LFSM /X /V /TS /FP /BYTES /NS /NC /NFL /NDL /NP /ETA /TEE /NJH /NJS /UNICODE /NP /?";
const ROBOCOPY_VALUES: &str = "/LEV /COPY /DCOPY /A+ /A- /MON /MOT /RH /IPG /MT /IOMAXSIZE /IORATE /THRESHOLD /IA /XA /MAX /MIN /MAXAGE /MINAGE /MAXLAD /MINLAD /R /W /LFSM /LOG /LOG+ /UNILOG /UNILOG+";

/// Git Bash 的盘符路径 `/c/` 的字节数。
const GIT_BASH_DRIVE_LEN: usize = 3;

/// Git Bash 把 `/c/...` 转成盘符路径：盘符后要有 `/`，免得把 `/L` 这样的开关当成路径。
fn drive_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= GIT_BASH_DRIVE_LEN
        && bytes[0] == b'/'
        && bytes[1].is_ascii_alphabetic()
        && bytes[2] == b'/'
}

/// Node `matchRobocopyDelete`（Git Bash 方言）：值选项用冒号绑定；/XF、/XD 的列表遇到下一个开关结束。
pub(crate) fn robocopy_delete(args: &[Word]) -> Hit {
    if args.len() < MIN_COPY_OPERANDS {
        return Hit::No;
    }
    let mut deletion = false;
    let mut list_only = false;
    let mut paths = 0;
    let mut exclusions = false;
    let stop = |deletion: bool| {
        if deletion {
            Hit::Rule("robocopy-delete")
        } else {
            Hit::Unknown
        }
    };
    for word in args {
        let value = word.value.to_uppercase();
        if word.dynamic {
            return stop(deletion);
        }
        if !value.starts_with('/') || drive_path(&word.value) {
            if !exclusions {
                paths += 1;
            }
            continue;
        }
        exclusions = value == "/XF" || value == "/XD";
        if value == "/MIR" || value == "/PURGE" {
            deletion = true;
        } else if value == "/L" {
            list_only = true;
        } else if exclusions
            || listed(ROBOCOPY_FLAGS, &value)
            || value
                .split_once(':')
                .is_some_and(|(name, _)| listed(ROBOCOPY_VALUES, name))
        {
            continue;
        } else {
            // JOB 与未知 arity 之后不能给出豁免，也不撤销之前已定位的删除。
            return stop(deletion);
        }
    }
    if paths >= MIN_COPY_OPERANDS && deletion && !list_only {
        Hit::Rule("robocopy-delete")
    } else {
        Hit::No
    }
}
