//! 规则文件的两种加载（context.md §11.1、§11.6）：会话开始时的全部无条件规则，
//! 与某个被触及文件的子目录规则和命中的条件规则。

use rust_cli_base::glob::Globs;
use rust_cli_base::names::{DOT_DIR, LOCAL_RULES_FILE, RULES_FILE, managed_dir};
use rust_cli_kernel_api::env_files::FileKind;
use rust_cli_kernel_api::env_files::GIT_DIR;
use rust_cli_kernel_api::env_git::{gitdir_pointer, marks_git_root};
use rust_cli_memory::entrypoint::ENTRYPOINT;

use crate::rules::{Kind, RULES_DIR, RuleFile, Scope, Walk, unconditional};

/// 工作区根是主仓库里面的 git worktree 时：`(worktree 根, 主仓库根)`。
pub(crate) struct Worktree {
    root: String,
    main: String,
}

impl Scope<'_> {
    fn managed(&self, platform: &str) -> String {
        self.style.normalize(managed_dir(platform))
    }

    /// 主仓库内、worktree 外的目录：不加载这些层的 Project 文件。
    fn outside_worktree(&self, dir: &str, worktree: Option<&Worktree>) -> bool {
        worktree.is_some_and(|tree| {
            self.style.is_within(dir, &tree.main) && !self.style.is_within(dir, &tree.root)
        })
    }

    /// git 根与主仓库根：`.git` 是文件、指向主仓库
    /// `worktrees/<name>`、且回指本目录时，主仓库根是 commondir 的上一层。
    pub(crate) async fn worktree(&self) -> Option<Worktree> {
        let style = self.style;
        let mut dir = Some(self.cwd.clone());
        let (root, kind) = loop {
            let current = dir?;
            let dot_git = style.join(&current, &[GIT_DIR]);
            match self.stat_kind(&dot_git).await {
                Some((kind, _)) if marks_git_root(kind) => break (current, kind),
                _ => dir = style.parent(&current),
            }
        };
        if kind != FileKind::File {
            return None;
        }
        let dot_git = style.join(&root, &[GIT_DIR]);
        let pointer = self.read(&dot_git).await?;
        // 缺陷修复（F58）：以前这里自己去掉 `gitdir:`，指针是 UNC（`\\…`、`//…`）时照读、可能认成 worktree；
        // 跨根的不安全路径应拒绝，cli 与 runtime 也拒绝。改用 kernel-api 的 `gitdir_pointer`（唯一定义）。
        let gitdir = style.resolve(&root, gitdir_pointer(&pointer)?, None);
        let common = self.read(&style.join(&gitdir, &["commondir"])).await?;
        let common = style.resolve(&gitdir, common.trim(), None);
        if style.parent(&gitdir).map(|dir| style.key(&dir))
            != Some(style.key(&style.join(&common, &["worktrees"])))
        {
            return None;
        }
        let back = self.read(&style.join(&gitdir, &["gitdir"])).await?;
        if style.key(&style.resolve(&gitdir, back.trim(), None)) != style.key(&dot_git) {
            return None;
        }
        let main = if style.file_name(&common) == GIT_DIR {
            style.parent(&common)?
        } else if self
            .stat_kind(&style.join(&common, &[GIT_DIR]))
            .await
            .is_some()
        {
            return None;
        } else {
            common
        };
        (style.key(&main) != style.key(&root) && style.is_within(&root, &main))
            .then_some(Worktree { root, main })
    }

    /// 会话开始时的全部无条件规则，按加载顺序（越靠后越优先）。
    pub(crate) async fn startup(&self, platform: &str) -> Vec<RuleFile> {
        let style = self.style;
        let managed = self.managed(platform);
        let worktree = self.worktree().await;
        let mut walk = Walk::new(self);
        let mut out = walk
            .file(style.join(&managed, &[RULES_FILE]), Kind::Managed, false)
            .await;
        let rules = style.join(&managed, &[DOT_DIR, RULES_DIR]);
        out.extend(unconditional(walk.rules(rules, Kind::Managed, false).await));
        if let Some(home) = &self.home {
            let user = style.join(home, &[DOT_DIR]);
            let file = style.join(&user, &[RULES_FILE]);
            out.extend(walk.file(file, Kind::User, true).await);
            let rules = style.join(&user, &[RULES_DIR]);
            out.extend(unconditional(walk.rules(rules, Kind::User, true).await));
        }
        for dir in style.ancestors(&self.cwd) {
            if !self.outside_worktree(&dir, worktree.as_ref()) {
                for file in [
                    style.join(&dir, &[RULES_FILE]),
                    style.join(&dir, &[DOT_DIR, RULES_FILE]),
                ] {
                    out.extend(walk.file(file, Kind::Project, false).await);
                }
                let rules = style.join(&dir, &[DOT_DIR, RULES_DIR]);
                out.extend(unconditional(walk.rules(rules, Kind::Project, false).await));
            }
            let local = style.join(&dir, &[LOCAL_RULES_FILE]);
            out.extend(walk.file(local, Kind::Local, false).await);
        }
        out
    }

    /// 自动记忆的 MEMORY.md（`AutoMem`，排在全部规则之后；memory.md §4）：内容按
    /// 行数与长度上限截断，空的不宣布。`dir` 是本机存储里的记忆目录，经本机的 `Files` 读：`Scope` 的端口由 runtime
    /// 从注册表的本机（`local_machine()`）取，不跟着工具调用的机器走（remote-exec.md §3、§15.8.1）。
    pub(crate) async fn auto_memory(&self, dir: &str) -> Option<RuleFile> {
        let style = self.style;
        let path = style.join(dir.trim_end_matches(['/', '\\']), &[ENTRYPOINT]);
        let raw = self.read(&path).await?;
        let content = rust_cli_memory::entrypoint::truncate(&raw);
        (!content.is_empty()).then_some(RuleFile {
            path,
            kind: Kind::AutoMem,
            content,
            globs: None,
            parent: None,
        })
    }

    /// 规则目录里命中 `target` 的条件规则：路径相对 `base`，在它外面不命中。
    fn matching(&self, files: Vec<RuleFile>, base: &str, target: &str) -> Vec<RuleFile> {
        let Some(relative) = self
            .style
            .relative(base, target)
            .map(|rel| rel.replace('\\', "/"))
            .filter(|rel| !rel.is_empty())
        else {
            return Vec::new();
        };
        files
            .into_iter()
            .filter(|file| {
                file.globs
                    .as_deref()
                    .is_some_and(|globs| !globs.is_empty() && Globs::new(globs).matches(&relative))
            })
            .collect()
    }

    /// 被触及的文件 `target` 带出的规则，按产生顺序。
    pub(crate) async fn nested(
        &self,
        platform: &str,
        target: &str,
        worktree: Option<&Worktree>,
    ) -> Vec<RuleFile> {
        let style = self.style;
        let mut walk = Walk::new(self);
        let mut out = Vec::new();
        // Managed、User 规则目录里的条件规则，路径相对工作区根。
        let managed = style.join(&self.managed(platform), &[DOT_DIR, RULES_DIR]);
        let files = walk.rules(managed, Kind::Managed, false).await;
        out.extend(self.matching(files, &self.cwd, target));
        if let Some(home) = &self.home {
            let files = walk
                .rules(style.join(home, &[DOT_DIR, RULES_DIR]), Kind::User, true)
                .await;
            out.extend(self.matching(files, &self.cwd, target));
        }
        // 触及文件所在目录到工作区根之间（不含工作区根）的每一层，从上往下。
        let mut nested = Vec::new();
        let mut dir = style.parent(target);
        while let Some(current) = dir {
            if style.key(&current) == style.key(&self.cwd) {
                break;
            }
            if style.is_within(&current, &self.cwd) {
                nested.push(current.clone());
            }
            dir = style.parent(&current);
        }
        for dir in nested.into_iter().rev() {
            for (file, kind) in [
                (style.join(&dir, &[RULES_FILE]), Kind::Project),
                (style.join(&dir, &[DOT_DIR, RULES_FILE]), Kind::Project),
                (style.join(&dir, &[LOCAL_RULES_FILE]), Kind::Local),
            ] {
                out.extend(walk.file(file, kind, false).await);
            }
            let files = walk
                .rules(
                    style.join(&dir, &[DOT_DIR, RULES_DIR]),
                    Kind::Project,
                    false,
                )
                .await;
            let (conditional, plain): (Vec<RuleFile>, Vec<RuleFile>) =
                files.into_iter().partition(|file| file.globs.is_some());
            out.extend(plain);
            out.extend(self.matching(conditional, &dir, target));
        }
        // 从根的下一层到工作区根：各层 `.zcode/rules` 里命中的条件规则。
        for dir in style.ancestors(&self.cwd) {
            if self.outside_worktree(&dir, worktree) {
                continue;
            }
            let files = walk
                .rules(
                    style.join(&dir, &[DOT_DIR, RULES_DIR]),
                    Kind::Project,
                    false,
                )
                .await;
            out.extend(self.matching(files, &dir, target));
        }
        out
    }
}

#[cfg(test)]
#[path = "rules_tests.rs"]
mod tests;
