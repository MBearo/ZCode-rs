//! 入口的命令行输出（从 main.rs 拆出，F58：main.rs 接近 800 行）。

use crate::help;

/// 帮助的折行宽度（commander `helpWidth`）：stdout 是终端时取它的列数，否则 80（cli-args.md §3）。
pub(crate) fn help_width() -> usize {
    use std::io::IsTerminal;
    if !std::io::stdout().is_terminal() {
        return help::DEFAULT_WIDTH;
    }
    rust_cli_tui::terminal_columns().map_or(help::DEFAULT_WIDTH, usize::from)
}

/// 写一行，写不进去时丢掉（终端已经关掉）。
pub(crate) fn write_lossy(mut out: impl std::io::Write, text: &str) {
    let _ = writeln!(out, "{text}");
    let _ = out.flush();
}

#[expect(clippy::print_stdout, reason = "入口 crate 的命令行输出")]
pub(crate) fn print_line(text: &str) {
    println!("{text}");
}

#[expect(clippy::print_stderr, reason = "入口 crate 的命令行错误输出")]
pub(crate) fn eprint_line(text: &str) {
    eprintln!("{text}");
}

/// 原样写到 stderr（文本自带换行）。
#[expect(clippy::print_stderr, reason = "入口 crate 的命令行错误输出")]
pub(crate) fn eprint_raw(text: &str) {
    eprint!("{text}");
}

/// 原样写到 stdout（文本自带换行）。
#[expect(clippy::print_stdout, reason = "入口 crate 的命令行输出")]
pub(crate) fn print_raw(text: &str) {
    print!("{text}");
}
