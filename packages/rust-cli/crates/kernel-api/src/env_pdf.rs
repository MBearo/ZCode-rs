//! 执行环境端口里读 PDF 的两项操作（docs/specs/rust-cli/tools-files.md §1.5，修复线 F24）。
//!
//! 外部程序是 poppler-utils 的 `pdfinfo` 与 `pdftoppm`，在执行环境里跑（内核不起进程）。端口只给原始结果：
//! 页数、页面的 JPEG、失败时的退出码与 stderr；给模型的文字（按失败的种类）在内核的
//! tools 里拼。文件内容经 stdin 交给两个程序（参数是 `-`）。

use serde::{Deserialize, Serialize};

use crate::env::EnvPath;

/// 读 PDF 的页数（read）：`pdfinfo -`。结果是 `Pages:` 一行的页数；找不到程序、失败、超时或没有这一行时为
/// None（这几种都按“页数未知”往下走）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PdfPageCount {
    pub path: EnvPath,
}

/// 把 PDF 的几页渲染成 JPEG（read）：`pdftoppm -jpeg -r 100 -f <first_page> [-l <last_page>] - <输出前缀>`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderPdfPages {
    pub path: EnvPath,
    /// 首页，从 1 起。
    pub first_page: u64,
    /// 末页；None 时到文档结尾（不传 `-l`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_page: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PdfRender {
    /// `pdftoppm -v` 跑不起来（没装 poppler-utils）。
    Unavailable,
    /// pdftoppm 没有成功退出：退出码（超时或被信号结束时为 None）与 stderr 原文。
    Failed {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
        stderr: String,
    },
    /// 渲染出的页面：输出目录里的 `.jpg` 按文件名排序（即页序），每项是一页的 JPEG 字节。
    Rendered { pages: Vec<Vec<u8>> },
}
