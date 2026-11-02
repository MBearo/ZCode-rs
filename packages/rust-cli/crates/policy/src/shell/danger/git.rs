// 移植自 apps/zcode-cli/packages/core/src/tool/handlers/guarded/git.ts（修复线 F20）。
//! git 的危险子命令：reset --hard、clean -f、强推、丢弃工作区、stash clear、worktree remove -f。

use super::options::{COMMAND_SCAN, Hit, Table, WRAPPER_SCAN, enabled, parse, settle};
use super::word::Word;

// 前置参数走 abort：未知即整体放弃，所以要认识 -c / -p / -P / --exec-path 这类常见前置项；-c 只按取值跳过。
const FRONT: Table = Table {
    flags: "--no-pager -P --paginate -p --literal-pathspecs --glob-pathspecs --noglob-pathspecs --icase-pathspecs --bare --no-replace-objects --no-optional-locks",
    values: "-C --git-dir --work-tree --namespace -c --config-env --super-prefix",
    optional: "--exec-path",
};
const RESET: Table = Table::flags(
    "-h --help -q --quiet -v --verbose --hard --soft --mixed --merge --keep -p --patch -N --intent-to-add --no-refresh --refresh --recurse-submodules --no-recurse-submodules",
);
const CLEAN: Table = Table {
    flags: "-h --help -q --quiet -v --verbose -f --force -d -x -X -n --dry-run --no-dry-run -i --interactive",
    values: "-e --exclude",
    optional: "",
};
const PUSH: Table = Table {
    flags: "-h --help -q --quiet -v --verbose -f --force --no-force --mirror --no-mirror -n --dry-run --no-dry-run --no-force-with-lease --force-if-includes --no-force-if-includes --all --branches --tags --delete -d --prune --atomic --no-atomic --porcelain --follow-tags --no-follow-tags --set-upstream -u --thin --no-thin --progress --no-progress --ipv4 -4 --ipv6 -6",
    values: "--repo --receive-pack --exec -o --push-option",
    optional: "--force-with-lease --signed --recurse-submodules",
};
const CHECKOUT: Table = Table {
    flags: "-h --help -q --quiet -v --verbose -f --force --no-force --detach --ours --theirs -m --merge --conflict -p --patch --ignore-other-worktrees --recurse-submodules --no-recurse-submodules --no-overlay --overlay",
    values: "-b -B --orphan --conflict",
    optional: "",
};
const RESTORE: Table = Table {
    flags: "-h --help -q --quiet -v --verbose -W --worktree --no-worktree -S --staged --no-staged --ours --theirs -m --merge -p --patch --ignore-unmerged --overlay --no-overlay --recurse-submodules --no-recurse-submodules",
    values: "-s --source --conflict",
    optional: "",
};
const HELP: Table = Table::flags("-h --help");
const WORKTREE_REMOVE: Table = Table::flags("-h --help -f --force --no-force");
const FORCE: [&str; 2] = ["-f", "--force"];
const TREE: [&str; 3] = [".", "./", ":/"];

fn has_help(flags: &[String]) -> bool {
    flags.iter().any(|flag| flag == "--help" || flag == "-h")
}

fn tree(words: &[Word]) -> bool {
    words
        .iter()
        .any(|word| !word.dynamic && TREE.contains(&word.value.as_str()))
}

/// Node `matchGit`：No 是已支持且没有命中，Unknown 是超出当前的命令或选项文法。
pub(crate) fn matches(words: &[Word]) -> Hit {
    let front = parse(words, &FRONT, WRAPPER_SCAN);
    if front.unsupported || front.operands.first().is_some_and(|word| word.dynamic) {
        return Hit::Unknown;
    }
    let Some((head, args)) = front.operands.split_first() else {
        return Hit::No;
    };
    let table = match head.value.as_str() {
        "reset" => &RESET,
        "clean" => &CLEAN,
        "push" => &PUSH,
        "checkout" => &CHECKOUT,
        "restore" => &RESTORE,
        "stash" | "worktree" => {
            let sub = if head.value == "stash" {
                "clear"
            } else {
                "remove"
            };
            if args.first().is_some_and(|word| word.dynamic) {
                return Hit::Unknown;
            }
            if args.first().map(|word| word.value.as_str()) != Some(sub) {
                return Hit::No;
            }
            if sub == "clear" {
                let parsed = parse(&args[1..], &HELP, COMMAND_SCAN);
                let rule = (!has_help(&parsed.flags)).then_some("git-stash-clear");
                return settle(rule, &parsed);
            }
            let parsed = parse(&args[1..], &WORKTREE_REMOVE, COMMAND_SCAN);
            let rule = (!has_help(&parsed.flags)
                && enabled(&parsed.flags, &FORCE, &["--no-force"]))
            .then_some("git-worktree-force-remove");
            return settle(rule, &parsed);
        }
        _ => return Hit::Unknown,
    };
    let parsed = parse(args, table, COMMAND_SCAN);
    let flags = &parsed.flags;
    let operands = &parsed.operands;
    // git 的 -h / --help 是可靠的查询退出；未知选项不改变这一点，也不抹去已识别的 force / hard。
    if has_help(flags) {
        return Hit::No;
    }
    let dry_run = enabled(flags, &["-n", "--dry-run"], &["--no-dry-run"]);
    let force = enabled(flags, &FORCE, &["--no-force"]);
    let rule = match head.value.as_str() {
        "reset" => enabled(
            flags,
            &["--hard"],
            &["--soft", "--mixed", "--merge", "--keep", "-p", "--patch"],
        )
        .then_some("git-reset-hard"),
        "clean" => (!dry_run && force).then_some("git-clean-force"),
        "push" => {
            let refspecs = if flags.iter().any(|flag| flag == "--repo") {
                &operands[..]
            } else {
                operands.get(1..).unwrap_or_default()
            };
            // glob 会让整个词记为动态，但已知的 `+` 前缀仍是强推的证据。
            (!dry_run
                && (force
                    || enabled(flags, &["--force-with-lease"], &["--no-force-with-lease"])
                    || enabled(flags, &["--mirror"], &["--no-mirror"])
                    || refspecs.iter().any(|word| word.value.starts_with('+'))))
            .then_some("git-push-force")
        }
        "checkout" => (force || tree(operands)).then_some("git-discard-tree"),
        _ => {
            let staged_only = enabled(flags, &["-S", "--staged"], &["--no-staged"])
                && !enabled(flags, &["-W", "--worktree"], &["--no-worktree"]);
            (!staged_only && tree(operands)).then_some("git-discard-tree")
        }
    };
    settle(rule, &parsed)
}
