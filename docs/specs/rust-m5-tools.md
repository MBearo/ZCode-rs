# Rust M5：编码工具对齐

依据：`apps/zcode-cli/packages/core/src/tool/{edit-matchers.ts,handlers/edit.ts}`、`packages/contracts/src/tools/edit.ts`。本文件按子项逐步补充；未列出的工具行为保持 `rust-coding-tools.md` 的现状。

## 1. Edit 匹配（M5.1）

### 1.1 所有者

匹配是纯函数（domain `edit_match`），输入为已统一为 `\n` 行尾的文件内容、`old_string`、`replace_all`；Edit 适配器（tools crate）负责读取、读后编辑检查、写回与结果投影。

### 1.2 匹配顺序（`findEditMatch`）

1. `exact`：非重叠子串匹配。
2. 依次尝试，首个产生候选的策略决定结果；`replace_all` 时跳过 3 个宽泛策略（`line_trimmed`、`indentation_flexible`、`block_anchor`）：
   - `quote_normalized`：弯引号 `‘’“”` 与直引号视为相同，候选取文件中的原片段；
   - `line_number_prefix_stripped`：每行都带 Read 行号前缀（`^\d+: ` 或 `^\d+\t`，其后内容不含行终止符）时去掉前缀再匹配；
   - `escape_normalized`：`\n` `\t` `\r` `\"` `\'` `` \` `` `\\` `\$` 反转义后匹配；
   - `unicode_escape_normalized`：`\uXXXX` 解码（`\\` 保持原样）后匹配；解码出不成对代理项时无候选；
   - `line_trimmed`：逐行按 JS `trim` 比较（忽略末尾空行）；
   - `indentation_flexible`：至少 2 行，去掉公共缩进（只计 tab 与空格）后相等；
   - `block_anchor`：至少 3 行，首尾行 trim 后相等，中间行的平均相似度（`1 - 编辑距离 / 较长长度`，按 UTF-16 码元计算）不低于 0.8。
3. 候选去重后只有一个值为 `matched`（带候选数），多个值为 `ambiguous`，没有候选为 `not_found`。

字符串语义与 JS 一致：`trim` 使用 JS 的空白集合（含 U+FEFF，不含 U+0085），编辑距离与长度按 UTF-16 码元，字母判断为 `\p{L}`。

### 1.3 替换文本

- `escape_normalized` 命中时，`new_string` 也按同样规则反转义。
- 引号风格：命中片段与 `old_string` 不同时，若片段含弯双引号则把替换文本中的 `"` 按上下文换成 `“` / `”`；含弯单引号时同理处理 `'`，两侧都是字母的 `'` 视为撇号 `’`。开引号上下文为：位于开头，或前一字符是空白、`(`、`[`、`{`、`—`、`–`。

### 1.4 Edit 处理顺序与文本（`handlers/edit.ts`）

失败以 `ToolFailure { code, message }` 返回（Node `ToolHandlerFailure`，`EditErrorCode`）；模型可见的包装在结果管线统一处理（M5.2），本节只定义 message。

1. `old_string === new_string`（原始字符串）：`1`，`No changes to make: old_string and new_string are exactly the same.`
2. `file_path` 为空：`13`，`Tool path must not be empty`
3. 文件不存在且 `old_string` 非空：`4`，`File does not exist. Note: your current working directory is <cwd>.`，同目录有同名不同扩展名的文件，或文件名编辑距离（UTF-16）不超过 3 的文件时追加 ` Did you mean <name>?`（按名称排序取第一个）。文件不存在且 `old_string` 为空时直接创建。
4. `old_string` 为空但文件内容（JS `trim` 后）非空：`3`，`Cannot create new file - file already exists.`
5. `.ipynb`：`5`，`File is a Jupyter Notebook. Use the NotebookEdit to edit this file.`
6. 读后编辑：没有读取记录或上次读取为部分视图：`6`，`File has not been read yet. Read it first before writing to it.`；读取后内容变化：`7`，`File has been modified since read, either by the user or by a linter. Read it again before attempting to write it.`
7. 匹配（第 1.2 节）：未找到：`8`，`String to replace not found in file.\nString: <old_string>`；歧义或精确命中多于 1 处且未设 `replace_all`：`9`，`Found <n> matches of the string to replace, but replace_all is false. To replace all occurrences, set replace_all to true. To replace only one occurrence, please provide more context to uniquely identify the instance.\nString: <old_string>`（`n` 为 0 时为 `old_string is not unique in the file. Provide more surrounding context or set replace_all to true.`）。
8. 替换：`new_string` 为空且 `old_string` 不以换行结尾、文件含 `old_string + "\n"` 时连同换行一起删除；替换文本按字面插入。
9. 写回：已有文件按多数行尾（CRLF 多于 LF 才用 CRLF）写回，新建文件为 LF；保留 BOM。
10. 成功结果：`The file <file_path> has been updated successfully. (file state is current in your context — no need to Read it back)`；`replace_all` 时为 `The file <file_path> has been updated. All occurrences were successfully replaced. (file state is current in your context — no need to Read it back)`。`file_path` 为模型给出的原值。结构化结果带 `matchStrategy` 与 `matchCandidateCount`。

与 Node 的差异：只支持 UTF-8（Node 还识别 GBK 等旧编码）；可编辑文件上限沿用 8 MiB（Node 为 1 GB）；新鲜度按内容哈希判断（Node 按 mtime 与大小，内容相同的完整读取例外），两者在内容不变时结论一致。

### 1.5 验收

- 集成测试（`zcode-cli-rust-edit.test.ts`）：弯引号回退写回、删除行连同换行、CRLF 保持、第 1.4 节的失败文本与成功文本。
- 夹具：生成脚本调用 Node 的 `findEditMatch`、`normalizeReplacementForMatch`、`preserveQuoteStyle`，覆盖每种策略、歧义、`replace_all` 跳过、UTF-16 与代理项、弯引号上下文；Rust 逐条比较状态、策略、候选数、命中片段与替换文本。
