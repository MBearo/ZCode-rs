//! 文件工具给模型看的短文本（docs/specs/rust-cli/prompts.md 规则 4）。
//!
//! 文字逐字固定。工具描述与 schema 不在这里，在
//! `rust-cli-prompts` 的数据文件里。

/// 当前目录说明的前缀。
pub const CWD_NOTE: &str = "Note: your current working directory is";

/// 写入前没有读过。
pub const NOT_READ: &str = "File has not been read yet. Read it first before writing to it.";

/// 读后被改过。
pub const MODIFIED_SINCE_READ: &str = "File has been modified since read, either by the user or by a linter. Read it again before attempting to write it.";

/// 写后状态已在上下文里。
pub const STATE_IS_CURRENT: &str =
    " (file state is current in your context \u{2014} no need to Read it back)";

/// Edit 结果里读后被改过、编辑仍套上时的说明。
pub const STALE_RECOVERED: &str = " (note: the file had been modified on disk since you last read it \u{2014} the edit applied cleanly, but the file contains other changes not in your context. Read it before edits that depend on surrounding content.)";

/// 重复读取未变的文件。
pub const FILE_UNCHANGED: &str = "Wasted call \u{2014} file unchanged since your last Read. Refer to that earlier tool_result instead.";

/// Read 的文本结果：空文件。
pub const EMPTY_FILE: &str =
    "<system-reminder>Warning: the file exists but the contents are empty.</system-reminder>";

/// Read 的文本结果：offset 超过文件长度。
pub fn shorter_than_offset(start_line: u64, total_lines: u64) -> String {
    format!(
        "<system-reminder>Warning: the file exists but is shorter than the provided offset ({start_line}). The file has {total_lines} lines.</system-reminder>"
    )
}

/// 路径解析后以空白结尾（errorCode 20）：Read / Write / Edit /
/// NotebookEdit 的 validateInput 最先检查（docs/specs/rust-cli/tools-files.md §3.2）。
pub fn unstable_path(tool: &str, field: &str) -> String {
    format!(
        "{tool} {field} ends in white space once its \".\" and \"..\" are resolved, and {tool} refuses such a path. If the white space is not meant, send the path without it. {tool} cannot open a file whose own name ends in white space."
    )
}

/// Read 的 validateInput errorCode 4。
pub fn binary_file(extension: &str) -> String {
    format!(
        "This tool cannot read binary files. The file appears to be a binary {extension} file. Use a skill for this file type if one is available, or a shell command or script that can read the format."
    )
}

/// Read 的 validateInput errorCode 9。
pub fn device_file(path: &str) -> String {
    format!("Cannot read '{path}': this device file would block or produce infinite output.")
}

/// 读目录时的报错（Node EISDIR 文本）。
pub fn is_directory(path: &str) -> String {
    format!("EISDIR: illegal operation on a directory, read '{path}'")
}

/// 文件太大。
pub fn too_large(size: &str, max: &str) -> String {
    format!(
        "File content ({size}) exceeds maximum allowed size ({max}). Use offset and limit parameters to read specific portions of the file, or search for specific content instead of reading the whole file."
    )
}

/// 文件的 token 数超过上限。
pub fn too_many_tokens(tokens: u64, max: u64) -> String {
    format!(
        "File content ({tokens} tokens) exceeds maximum allowed tokens ({max}). Use offset and limit parameters to read specific portions of the file, or search for specific content instead of reading the whole file."
    )
}

/// Edit 的 validateInput 的固定错误。
pub const EDIT_NO_CHANGES: &str =
    "No changes to make: old_string and new_string are exactly the same.";
pub const EDIT_FILE_EXISTS: &str = "Cannot create new file - file already exists.";

pub fn edit_notebook(notebook_tool: &str) -> String {
    format!("File is a Jupyter Notebook. Use the {notebook_tool} to edit this file.")
}

pub fn edit_not_found(old_string: &str) -> String {
    format!("String to replace not found in file.\nString: {old_string}")
}

/// 没有匹配时，若 old_string 里有 `\uXXXX` 或非 ASCII 字符，附加这句。
pub const EDIT_ESCAPE_HINT: &str = "\n(note: Edit also tried swapping \\uXXXX escapes and their characters; neither form matched, so the mismatch is likely elsewhere in old_string. Re-read the file and copy the exact surrounding text.)";

pub fn edit_ambiguous(count: usize, old_string: &str) -> String {
    format!(
        "Found {count} matches of the string to replace, but replace_all is false. To replace all occurrences, set replace_all to true. To replace only one occurrence, please provide more context to uniquely identify the instance.\nString: {old_string}"
    )
}

pub fn edit_too_large(size: &str, max: &str) -> String {
    format!("File is too large to edit ({size}). Maximum editable file size is {max}.")
}

/// Write 的 validateInput errorCode 17 / 18。
pub fn write_directory(path: &str) -> String {
    format!(
        "{path} is a directory, not a file. To create a file inside it, include the file name in file_path."
    )
}

pub fn write_not_regular(path: &str) -> String {
    format!(
        "{path} exists but is not a regular file (a device, FIFO or socket). Write only creates or overwrites regular files."
    )
}

/// Write：子代理不写报告文件（errorCode 5）。
pub const SUBAGENT_REPORT: &str = "Subagents should return findings as text, not write report files. Include this content in your final response instead.";

/// Glob / Grep 的搜索目录不存在。
pub fn directory_missing(path: &str, cwd: &str) -> String {
    format!("Directory does not exist: {path}. {CWD_NOTE} {cwd}.")
}

pub fn not_a_directory(path: &str) -> String {
    format!("Path is not a directory: {path}")
}

pub fn path_missing(path: &str, cwd: &str) -> String {
    format!("Path does not exist: {path}. {CWD_NOTE} {cwd}.")
}

/// rg 拒绝模式时 Grep 的报错前缀（执行管线对 rg 输出错误的说明）。
pub const RG_REJECTED: &str =
    "Search failed \u{2014} ripgrep rejected the pattern, glob, or file type without searching:\n";

/// NotebookEdit 的 validateInput / call 的错误。
pub const NOTEBOOK_ONLY: &str = "File must be a Jupyter notebook (.ipynb file). For editing other file types, use the FileEdit tool.";
pub const NOTEBOOK_INSERT_TYPE: &str = "Cell type is required when using edit_mode=insert.";
pub const NOTEBOOK_MISSING: &str = "Notebook file does not exist.";
pub const NOTEBOOK_BAD_JSON: &str = "Notebook is not valid JSON.";
pub const NOTEBOOK_NEED_ID: &str = "Cell ID must be specified when not inserting a new cell.";
/// notebook 不合法。
pub const NOTEBOOK_INVALID: &str = "Notebook file is not a valid Jupyter notebook (top-level \"cells\" must be an array of cell objects, each with a string or string-array \"source\").";

pub fn notebook_cell_missing(id: &str) -> String {
    format!("Cell with ID \"{id}\" not found in notebook.")
}

pub fn notebook_index_missing(index: usize) -> String {
    format!("Cell with index {index} does not exist in notebook.")
}

/// 输入不是合法 JSON 时的 InputValidationError 正文（JSON 解析失败的分支）：原文取前 200 个 UTF-16 单元，
/// `len` 是组装时记下的原文长度。
pub fn unparsed_input(tool: &str, raw: &str, len: u64) -> String {
    // 原文最多给出这么多个 UTF-16 单元。
    const SENT_UNITS: usize = 200;
    let sent = rust_cli_base::js::string::utf16_prefix(raw, SENT_UNITS);
    format!(
        "{tool} was called with input that could not be parsed as JSON.\nYou sent (first {} of {len} bytes): {sent}\nCommon causes: unescaped backslashes in file paths (use / or \\\\), unescaped control characters, or truncated output. Retry with valid JSON.",
        sent.encode_utf16().count()
    )
}
