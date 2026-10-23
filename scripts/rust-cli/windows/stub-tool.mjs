#!/usr/bin/env node
// Windows 交叉 clippy 的桩编译器与桩归档器（docs/specs/rust-cli/performance.md §8）：
//   node stub-tool.mjs cc <参数…>   cc-rs 的 C / C++ 编译器（cl.exe 的写法）
//   node stub-tool.mjs ar <参数…>   cc-rs 的归档器（lib.exe 的写法）
// 不编译、不归档：只写出空的输出文件，让 build.rs 走完。clippy 只检查 Rust 代码，不链接，所以 C 代码的产物用不到。
// 由 clippy.mjs 生成的 target/msvc-stub/{cc,ar} 调用；规则在 cross.mjs 的 stubOutputs。
import { writeFile } from "node:fs/promises";

import { stubOutputs } from "./cross.mjs";

const [tool, ...args] = process.argv.slice(2);

try {
  // 预处理（cc-rs 探测编译器族的 `-E`）：什么都不输出。`-?` 退出码 0：cc-rs 据此认作 cl 的写法（MSVC 族）。
  for (const path of stubOutputs(tool, args)) await writeFile(path, "");
  process.exitCode = 0;
} catch (error) {
  process.stderr.write(`stub-tool：${error.message}\n`);
  process.exitCode = 2;
}
