//! 内置工具声明的判决主体（kernel-api `PermissionSubject`，shell-permissions.md §4.1）。F125 的 T3：以前判决链按工具名
//! 分派（policy `chain::builtin` 与 `files::tool_access`）；这张表就是那份分派，换成工具自己声明之后逐项对照。

use std::sync::Arc;

use rust_cli_kernel_api::permission::{FileAccess, PermissionSubject};
use rust_cli_kernel_api::testing::{FixedClock, FixedGate};
use rust_cli_kernel_api::tool::Tool;
use serde_json::{Value, json};

use crate::common::FileTools;
use crate::{
    BashTool, EditTool, GlobTool, GrepTool, MonitorTool, NotebookEditTool, ReadTool, WebFetchTool,
    WriteTool,
};

fn files() -> FileTools {
    FileTools::new(Arc::default(), Arc::new(FixedGate::passthrough()))
}

fn path(access: FileAccess, path: Option<&str>) -> Option<PermissionSubject> {
    Some(PermissionSubject::Path {
        access,
        path: path.map(str::to_owned),
    })
}

fn shell(command: &str) -> Option<PermissionSubject> {
    Some(PermissionSubject::Shell {
        command: command.to_owned(),
    })
}

#[test]
fn builtin_tools_declare_the_subjects_the_chain_used_to_pick_by_name() {
    let cases: Vec<(Arc<dyn Tool>, Value, Option<PermissionSubject>)> = vec![
        // Bash：总是 shell；没有 command 时是空命令。
        (
            Arc::new(BashTool::default()),
            json!({"command": "ls -la"}),
            shell("ls -la"),
        ),
        (Arc::new(BashTool::default()), json!({}), shell("")),
        // Monitor：有 command 字段时照 Bash（tools-more.md §19.4），ws 来源没有主体（工具自己询问）。
        (
            Arc::new(MonitorTool::new()),
            json!({"description": "d", "command": "tail -f log"}),
            shell("tail -f log"),
        ),
        (
            Arc::new(MonitorTool::new()),
            json!({"description": "d", "command": 1}),
            shell(""),
        ),
        (
            Arc::new(MonitorTool::new()),
            json!({"description": "d", "ws": {"url": "wss://example.com"}}),
            None,
        ),
        // 文件工具：读是 Read / Glob / Grep，写是 Edit / Write / NotebookEdit；空路径与缺路径都是 None。
        (
            Arc::new(ReadTool::new(files())),
            json!({"file_path": "a.rs"}),
            path(FileAccess::Read, Some("a.rs")),
        ),
        (
            Arc::new(ReadTool::new(files())),
            json!({"file_path": ""}),
            path(FileAccess::Read, None),
        ),
        (
            Arc::new(GlobTool::new()),
            json!({"pattern": "*.rs", "path": "src"}),
            path(FileAccess::Read, Some("src")),
        ),
        (
            Arc::new(GlobTool::new()),
            json!({"pattern": "*.rs"}),
            path(FileAccess::Read, None),
        ),
        (
            Arc::new(GrepTool::new()),
            json!({"pattern": "x", "path": "src"}),
            path(FileAccess::Read, Some("src")),
        ),
        (
            Arc::new(EditTool::new(files())),
            json!({"file_path": "a.rs"}),
            path(FileAccess::Write, Some("a.rs")),
        ),
        (
            Arc::new(WriteTool::new(files())),
            json!({"file_path": "a.rs"}),
            path(FileAccess::Write, Some("a.rs")),
        ),
        (
            Arc::new(NotebookEditTool::new(files())),
            json!({"notebook_path": "n.ipynb"}),
            path(FileAccess::Write, Some("n.ipynb")),
        ),
        // 只认工具自己的路径字段。
        (
            Arc::new(NotebookEditTool::new(files())),
            json!({"file_path": "n.ipynb"}),
            path(FileAccess::Write, None),
        ),
        // WebFetch：网址；没有时是空串。
        (
            Arc::new(WebFetchTool::new(None, Arc::new(FixedClock(0)))),
            json!({"url": "https://example.com/a", "prompt": "p"}),
            Some(PermissionSubject::Url {
                url: "https://example.com/a".to_owned(),
            }),
        ),
        (
            Arc::new(WebFetchTool::new(None, Arc::new(FixedClock(0)))),
            json!({"prompt": "p"}),
            Some(PermissionSubject::Url { url: String::new() }),
        ),
    ];
    for (tool, input, expected) in cases {
        assert_eq!(
            tool.permission_subject(&input),
            expected,
            "{} {input}",
            tool.name()
        );
    }
}
