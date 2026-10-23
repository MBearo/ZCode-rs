//! guarded 的危险命令识别（shell-permissions.md §4.6.1）：用例移植自 Node
//! `apps/zcode-cli/packages/core/tests/guarded-command.test.ts` 与 `guarded-deletion.test.ts`（POSIX 与 Git Bash；
//! CMD 方言不移植）。
use super::*;

fn status(command: &str, dialect: Dialect) -> Danger {
    dangerous(command, dialect)
}

fn assert_rule(command: &str, dialect: Dialect, rule: &str) {
    assert_eq!(
        status(command, dialect),
        Danger::Matched(format!("safety.bash.{rule}")),
        "{command}"
    );
}

fn assert_not_matched(command: &str, dialect: Dialect) {
    assert!(
        !matches!(status(command, dialect), Danger::Matched(_)),
        "{command}: {:?}",
        status(command, dialect)
    );
}

const BOTH: [Dialect; 2] = [Dialect::Posix, Dialect::GitBash];

#[test]
fn each_rule_matches_its_commands() {
    let table: &[(&str, &[&str])] = &[
        (
            "remove-critical-path",
            &[
                "rm -rf build",
                "rm -f -R build",
                "rm --recursive --force build",
                "rm -rf \"$target\"",
                "rm -rf -- -i",
                "rm -rfi build",
            ],
        ),
        (
            "system-storage",
            &[
                "dd if=in of=out",
                "mount /dev/disk /mnt",
                "umount /mnt",
                "mkfs /dev/disk",
                "mkfs.ext4 /dev/disk",
            ],
        ),
        (
            "git-reset-hard",
            &[
                "git reset --hard",
                "git -C repo reset --hard HEAD",
                "git --git-dir=repo reset --hard",
            ],
        ),
        (
            "git-clean-force",
            &[
                "git clean -fd",
                "git clean -dfx",
                "git clean -f file",
                "git clean -f -- --dry-run",
            ],
        ),
        (
            "git-push-force",
            &[
                "git push -f origin main",
                "git push --force-with-lease",
                "git push --mirror",
                "git push origin +HEAD:main",
                "git push --force origin 'refs/heads/*:refs/heads/*'",
                "git push --force --no-dry-run",
                "git push --force-with-lease --no-force",
                "git push origin +refs/heads/*:refs/heads/*",
                "git push origin \"+refs/heads/*:refs/heads/*\"",
                "git push origin \"+main\"",
                "git push --repo origin +refs/heads/*:refs/heads/*",
            ],
        ),
        (
            "git-discard-tree",
            &[
                "git checkout -f main",
                "git checkout HEAD -- .",
                "git restore .",
                "git restore --staged --worktree ./",
                "git restore --source=HEAD :/",
            ],
        ),
        ("git-stash-clear", &["git stash clear"]),
        (
            "git-worktree-force-remove",
            &[
                "git worktree remove -ff path",
                "git worktree remove --force path",
            ],
        ),
        (
            "rsync-delete",
            &[
                "rsync -a --delete src/ dest/",
                "rsync --del src/ dest/",
                "rsync --delete-before src/ dest/",
                "rsync --delete-during src/ dest/",
                "rsync --delete-delay src/ dest/",
                "rsync --delete-after src/ dest/",
                "rsync --delete-excluded src/ dest/",
            ],
        ),
    ];
    for (rule, commands) in table {
        for command in *commands {
            for dialect in BOTH {
                assert_rule(command, dialect, rule);
            }
        }
    }
}

#[test]
fn wrappers_and_command_lists_are_looked_through() {
    for command in [
        "env A=b rm -rf build",
        "env -u A rm -rf build",
        "time -o file rm -rf build",
        "/usr/bin/time --output=file rm -rf build",
        "sudo -u root rm -rf build",
        "env -C repo time -f '%e' sudo --user=root rm -rf build",
        "echo hello; rm -rf build",
        "false || rm -rf build",
        "true && rm -rf build",
        "echo hi | rm -rf build",
        "\"/bin/rm\" -rf build",
        "r\\m -rf build",
    ] {
        assert_rule(command, Dialect::Posix, "remove-critical-path");
    }
}

/// D12：解析在第一个复合结构处停下，后面的命令不识别（Node 识别）。复合结构里面的命令两边都不识别。
#[test]
fn commands_after_a_compound_command_are_not_recognized() {
    assert_eq!(
        status("for x in a; do echo hi; done; rm -rf build", Dialect::Posix),
        Danger::Unsupported
    );
    assert_eq!(
        status("for x in a; do rm -rf build; done", Dialect::Posix),
        Danger::Unsupported
    );
}

#[test]
fn harmless_commands_are_not_matched() {
    for command in [
        "rm -f file",
        "rm -- -rf",
        "rm --help",
        "rm --version",
        "mount",
        "dd --help",
        "mount --version",
        "mkfs --help",
        "git reset --soft HEAD",
        "git reset -- --hard",
        "git clean -nfd",
        "git clean -fd --dry-run",
        "git clean -fd -e --dry-run --dry-run",
        "git push -nf",
        "git push --force --no-force",
        "git push --force-if-includes",
        "git push --force-with-lease --no-force-with-lease",
        "git push --mirror --no-mirror",
        "git push --repo +not-a-refspec",
        "git push --push-option=--force",
        "git restore --staged .",
        "git restore file",
        "git checkout -b branch",
        "git checkout -b .",
        "git stash drop",
        "git worktree remove path",
        "rsync --delete -n src/ dest/",
        "rsync --exclude --delete src/ dest/",
        "rsync --filter=--delete src/ dest/",
        "echo 'rm -rf build'",
        "echo hi # rm -rf build",
        "echo hi > 'rm -rf build'",
        "time -o 'rm -rf build' echo ok",
        "sudo -u rm echo -rf build",
        "env A='rm -rf build' echo ok",
    ] {
        assert_eq!(
            status(command, Dialect::Posix),
            Danger::NotMatched,
            "{command}"
        );
    }
    for command in [
        "git push --dry-run origin +refs/heads/*:refs/heads/*",
        "git push -n origin +refs/heads/*:refs/heads/*",
        "git push origin refs/heads/*:refs/heads/*",
        "git push --push-option \"+refs/heads/*:refs/heads/*\" origin main",
        "git push --repo \"+refs/heads/*:refs/heads/*\" main",
        "git push \"+refs/heads/*:refs/heads/*\" main",
    ] {
        for dialect in BOTH {
            assert_not_matched(command, dialect);
        }
    }
}

#[test]
fn exemption_words_stay_apart_from_option_values() {
    for (dangerous_, exempt, rule) in [
        (
            "git clean -fd -e --dry-run",
            "git clean -fd --dry-run",
            "git-clean-force",
        ),
        (
            "git clean -nfd --no-dry-run",
            "git clean -fd --no-dry-run -n",
            "git-clean-force",
        ),
        (
            "git push -f --push-option --dry-run",
            "git push -f --dry-run",
            "git-push-force",
        ),
        (
            "git push -nf --no-dry-run",
            "git push -f --no-dry-run -n",
            "git-push-force",
        ),
        (
            "git push --mirror --no-force",
            "git push --mirror --no-mirror",
            "git-push-force",
        ),
        (
            "rsync --delete --exclude --help src/ dest/",
            "rsync --delete --help",
            "rsync-delete",
        ),
        (
            "rsync --delete --filter --dry-run src/ dest/",
            "rsync --delete -n src/ dest/",
            "rsync-delete",
        ),
    ] {
        assert_rule(dangerous_, Dialect::Posix, rule);
        assert_eq!(
            status(exempt, Dialect::Posix),
            Danger::NotMatched,
            "{exempt}"
        );
    }
}

#[test]
fn unsupported_boundaries_are_labelled() {
    let long = "x".repeat(10_001);
    for command in [
        "bash -c 'rm -rf build'",
        "powershell -Command 'rm -rf build'",
        "eval 'rm -rf build'",
        "$exe -rf build",
        "git -c alias.wipe='reset --hard' wipe",
        "env -S 'rm -rf build'",
        "time --unknown rm -rf build",
        "for x in a; do rm -rf build; done",
        "rm 'unterminated",
        long.as_str(),
        "rm --totally-unknown build",
        "rsync --unknown-flag src/ dest/",
        "git push --totally-unknown origin main",
        "git reset --totally-unknown",
    ] {
        assert_eq!(
            status(command, Dialect::Posix),
            Danger::Unsupported,
            "{command}"
        );
    }
}

#[test]
fn recognized_dangerous_options_survive_unknown_options() {
    for (command, rule) in [
        ("rm -rfx build", "remove-critical-path"),
        ("rm -rfP build", "remove-critical-path"),
        ("rm -rf -W build", "remove-critical-path"),
        ("rm -rf --preserve-root=all build", "remove-critical-path"),
        ("rm -rf --totally-unknown build", "remove-critical-path"),
        (
            "rsync -a --delete --info=progress2 src/ dest/",
            "rsync-delete",
        ),
        ("rsync -a --delete --stats src/ dest/", "rsync-delete"),
        ("rsync -ai --delete src/ dest/", "rsync-delete"),
        (
            "rsync -a --delete --partial-dir=.rsync-partial src/ dest/",
            "rsync-delete",
        ),
        ("rsync --unknown-flag --delete src/ dest/", "rsync-delete"),
        ("git push --force --no-verify origin main", "git-push-force"),
        (
            "git push --force --no-recurse-submodules origin main",
            "git-push-force",
        ),
        (
            "git push --force --totally-unknown origin main",
            "git-push-force",
        ),
        (
            "git -c core.pager=cat reset --hard HEAD~1",
            "git-reset-hard",
        ),
        ("git -p reset --hard", "git-reset-hard"),
        ("git --exec-path=/opt/git reset --hard", "git-reset-hard"),
        ("git reset --hard --totally-unknown", "git-reset-hard"),
        ("git checkout -f -t origin/main", "git-discard-tree"),
        ("git checkout -f --guess main", "git-discard-tree"),
        ("git stash clear --totally-unknown", "git-stash-clear"),
        (
            "git worktree remove --force --totally-unknown path",
            "git-worktree-force-remove",
        ),
    ] {
        assert_rule(command, Dialect::Posix, rule);
    }
}

#[test]
fn rm_recursion_ignores_help_position_and_wrappers() {
    for dialect in BOTH {
        for flags in ["-rf", "-f -R", "--recursive --force"] {
            for query in ["--help", "--version"] {
                for args in [
                    format!("{flags} target {query}"),
                    format!("{query} {flags} target"),
                    format!("{flags} {query}"),
                    format!("{flags} -- {query}"),
                ] {
                    for wrapper in ["", "env A=b ", "time -o timing.txt ", "sudo -u root "] {
                        assert_rule(
                            &format!("{wrapper}rm {args}"),
                            dialect,
                            "remove-critical-path",
                        );
                    }
                }
            }
        }
        for command in ["rm --help", "rm -- -rf"] {
            assert_eq!(status(command, dialect), Danger::NotMatched, "{command}");
        }
    }
}

#[test]
fn rm_batches_and_globs_are_dangerous() {
    for command in [
        "rm first --help",
        "rm first --version",
        "rm first -f",
        "rm first --",
    ] {
        assert_rule(command, Dialect::Posix, "remove-critical-path");
    }
    for dialect in BOTH {
        for command in [
            "rm -r dir",
            "rm -R dir",
            "rm --recursive dir",
            "rm -ri dir",
            "rm -r --help",
            "rm a b",
            "rm \"one file\" \"two files\"",
            "rm -- -r -f",
            "rm *.log",
            "rm file?.txt",
            "rm [ab].txt",
            "rm \"prefix\"*.log",
            "rm -f *.log",
            "rm -r --unknown dir",
            "env A=b rm -r dir",
            "time -o timing.txt rm a b",
            "sudo -u root rm *.log",
            "echo ready; rm a b",
            "true && rm -r dir",
            "false || rm *.log",
            "rm a b | cat",
            "rm a b > output",
            "rm a b # keep comment",
        ] {
            assert_rule(command, dialect, "remove-critical-path");
        }
        for command in [
            "rm one",
            "rm -f one",
            "rm \"one file\"",
            "rm -- -r",
            "rm --help",
            "rm \"*.log\"",
            "rm '*.log'",
            "rm \\*.log",
            "rm file\\?.txt",
            "rm \\[ab].txt",
            "rm $TARGET",
            "rm \"$TARGET\"",
            "rm {a,b}",
            "rm $(echo target)",
            "rm $(echo *)",
            "rm ${TARGET:-*}",
            "echo \"rm a b\"",
            "echo ok # rm a b",
            "echo ok > rm",
            "rmdir empty",
            "xargs rm -r",
            "npx rimraf dir",
            "sh -c 'rm -r dir'",
        ] {
            assert_not_matched(command, dialect);
        }
    }
}

#[test]
fn find_delete_follows_the_expression_grammar() {
    for flags in ["-d", "-x", "-d -x"] {
        for command in [
            format!("find {flags} . -type f -delete"),
            format!("find {flags} \"space dir\" another -delete"),
            format!("time -o timing.txt find {flags} . -delete"),
        ] {
            assert_rule(&command, Dialect::Posix, "find-delete");
        }
        for command in [
            format!("find {flags} . -type f -print"),
            format!("find {flags} . -name '-delete'"),
            format!("find {flags} . -exec echo -delete \\;"),
        ] {
            assert_eq!(
                status(&command, Dialect::Posix),
                Danger::NotMatched,
                "{command}"
            );
        }
        let unknown = format!("find {flags} . -unknown -delete");
        assert_eq!(status(&unknown, Dialect::Posix), Danger::Unsupported);
    }
    for dialect in BOTH {
        for command in [
            "find . -delete",
            "find . -type f -name '*.log' -delete",
            "find . \\( -name a -o -name b \\) -delete",
            "find . -false -a -delete",
            "find . -exec echo {} \\; -delete",
            "find . -execdir echo {} + -delete",
            "find . -ok echo {} \\; -delete",
            "find . -okdir echo {} \\; -delete",
            "find -L . -maxdepth 2 -mtime +1 -delete",
            "find . -delete -unknown value",
            "time -o file find . -delete",
        ] {
            assert_rule(command, dialect, "find-delete");
        }
        assert_rule(
            "find . -unknown value; rm -r dir",
            dialect,
            "remove-critical-path",
        );
        for command in [
            "find . -name '-delete'",
            "find . -path -delete",
            "find . -printf '-delete'",
            "find . -fprintf out '-delete'",
            "find . -fprint -delete",
            "find . -exec echo -delete \\;",
            "find . -exec rm -rf {} +",
            "find . -okdir echo -delete \\;",
            "find . -print",
            "find . -name '*.log'",
        ] {
            assert_not_matched(command, dialect);
        }
        for command in [
            "find . -unknown -delete",
            "find . -name",
            "find . -exec echo -delete",
        ] {
            assert_eq!(status(command, dialect), Danger::Unsupported, "{command}");
        }
    }
}

#[test]
fn robocopy_is_only_read_in_git_bash() {
    for command in [
        "robocopy /c/src /c/dst /MIR",
        "env MSYS2_ARG_CONV_EXCL=/MIR robocopy /c/src /c/dst /MIR",
        "robocopy \"/c/space src\" \"/d/space dst\" /PURGE",
        "robocopy /MIR /c/src /d/dst /R:0 /W:0",
        "robocopy src dst /MIR",
        "robocopy /MIR src dst",
        "robocopy src /PURGE dst",
        "ROBOCOPY.EXE src dst /purge",
        "robocopy src dst /MIR /LOG:out",
        "robocopy src dst /MIR /unknown /L",
        "robocopy src dst /MIR /JOB:job",
        "robocopy src dst /XF /MIR",
    ] {
        assert_rule(command, Dialect::GitBash, "robocopy-delete");
    }
    for command in [
        "robocopy /c/src /c/dst /MIR /L",
        "robocopy /c/src /c/dst /PURGE /XD /c/excluded /L",
        "robocopy /c/src /c/dst /LOG:/MIR",
        "robocopy /c/src /MIR",
        "robocopy src dst /E",
        "robocopy src dst /l /PURGE",
        "robocopy src dst /MIR /XF /L",
        "robocopy /MIR dst",
    ] {
        assert_not_matched(command, Dialect::GitBash);
    }
    for command in [
        "robocopy /unknown /c/src /c/dst /MIR",
        "robocopy /JOB:job /c/src /c/dst /MIR",
    ] {
        assert_eq!(
            status(command, Dialect::GitBash),
            Danger::Unsupported,
            "{command}"
        );
    }
    // POSIX 不认 robocopy（Node 只在 CMD 与 Git Bash 认）。
    assert_eq!(
        status("robocopy src dst /MIR", Dialect::Posix),
        Danger::NotMatched
    );
}

#[test]
fn only_guarded_bash_calls_apply() {
    let input = serde_json::json!({"command": "rm -rf build"});
    assert!(applies(
        PermissionMode::Guarded,
        "Bash",
        &input,
        PathStyle::Posix
    ));
    // plan 开时判决用的模式是 plan；yolo 与其他模式都不询问。
    for mode in [
        PermissionMode::Plan,
        PermissionMode::BypassPermissions,
        PermissionMode::Default,
    ] {
        assert!(!applies(mode, "Bash", &input, PathStyle::Posix), "{mode:?}");
    }
    assert!(!applies(
        PermissionMode::Guarded,
        "Write",
        &input,
        PathStyle::Posix
    ));
    let harmless = serde_json::json!({"command": "ls -la"});
    assert!(!applies(
        PermissionMode::Guarded,
        "Bash",
        &harmless,
        PathStyle::Posix
    ));
}

/// 工作线 F53 阶段 0（shell-permissions.md §4.5.1、§4.6.1）：find 换成 bfs 之后，`-rm` 等同 `-delete`；bfs 的谓词
/// 与写在表达式后面的起点按 bfs 的语法跳过，不再当作未知的文法。
#[test]
fn find_delete_knows_the_bfs_grammar() {
    for dialect in BOTH {
        for command in [
            "find . -rm",
            "find . -type f -name '*.log' -rm",
            "find . \\( -name a -o -name b \\) -rm",
            // bfs 的起点可以写在表达式后面。
            "find -name x /tmp -delete",
            "find . -name x /tmp -rm",
            "find \\( -name x \\) /tmp -delete",
            // bfs 自己的谓词与标志。
            "find . -hidden -rm",
            "find -S dfs -regextype findutils-default . -rm",
            "find -Sdfs -j4 -O3 . -delete",
            "find . -limit 3 -since 2026-01-01 -rm",
            "find -E . -regex '.*[.]tmp' -rm",
            "find . -exclude -name .git -rm",
            "find . -depth 2 -rm",
            "find -f /tmp -rm",
        ] {
            assert_rule(command, dialect, "find-delete");
        }
        for command in [
            "find . -name -rm",
            "find . -hidden -print",
            "find -name x /tmp -print",
            "find . -exec echo -rm \\;",
        ] {
            assert_eq!(status(command, dialect), Danger::NotMatched, "{command}");
        }
        // 真正不认识的谓词仍然放弃（Node 的“arity 未知之后不猜”）。
        assert_eq!(status("find . -unknown -rm", dialect), Danger::Unsupported);
    }
}
