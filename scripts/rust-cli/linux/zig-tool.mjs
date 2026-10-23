#!/usr/bin/env node
// 包装脚本的实际入口（docs/specs/rust-cli/linux.md §3、§3.1）：
//   node zig-tool.mjs <zig> cc <zig 目标> <参数…>   cc-rs 的 C 编译器与 rustc 的链接器
//   node zig-tool.mjs <zig> c++ <zig 目标> <参数…>  cc-rs 的 C++ 编译器（内嵌的 ugrep，embedded-search.md §7.2）
//   node zig-tool.mjs <zig> ar <zig 目标> <参数…>   cc-rs 的归档器
// 由 build-linux.mjs 生成的 target/zig/<triple>/{cc,ar} 调用；参数改写的规则在 linker-args.mjs。
import { spawn } from "node:child_process";
import { readFile, writeFile } from "node:fs/promises";

import { formatResponseFile, parseResponseFile, rewriteCcArgs } from "./linker-args.mjs";

const [zig, tool, zigTarget, ...rest] = process.argv.slice(2);

/** 展开 `@文件`（rustc 在命令行过长时用）；没有时原样返回。 */
async function expand(args) {
  const out = [];
  let found = false;
  for (const arg of args) {
    if (arg.startsWith("@") && arg.length > 1) {
      out.push(...parseResponseFile(await readFile(arg.slice(1), "utf8")));
      found = arg.slice(1);
    } else {
      out.push(arg);
    }
  }
  return { args: out, responseFile: found };
}

async function zigArgs() {
  if (tool === "ar") return ["ar", ...rest];
  if (tool !== "cc" && tool !== "c++") throw new Error(`zig-tool：不认识的工具 ${tool}`);
  const { args, responseFile } = await expand(rest);
  const rewritten = rewriteCcArgs(args);
  if (!responseFile) return [tool, "-target", zigTarget, ...rewritten];
  // 原来就过长：改写后的参数仍写成 response file（zig 的读法见 formatResponseFile）。
  const next = `${responseFile}.zig`;
  await writeFile(next, formatResponseFile(rewritten));
  return [tool, "-target", zigTarget, `@${next}`];
}

try {
  const child = spawn(zig, await zigArgs(), { stdio: "inherit" });
  child.on("error", (error) => {
    process.stderr.write(`zig-tool：无法运行 ${zig}：${error.message}\n`);
    process.exit(127);
  });
  child.on("exit", (code, signal) => process.exit(code ?? (signal ? 128 : 1)));
} catch (error) {
  process.stderr.write(`${error.message}\n`);
  process.exit(2);
}
