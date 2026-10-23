// zig 作 C 编译器与链接器时的参数改写（docs/specs/rust-cli/linux.md §3.1）。纯函数，包装脚本 zig-tool.mjs 调用。

/** rustc 自带的 musl 启动文件（`…/self-contained/crt1.o`、`rcrt1.o`、`crti.o`、`crtbeginS.o` 等）。 */
const SELF_CONTAINED_CRT = /[\\/]self-contained[\\/][^\\/]*crt[^\\/]*\.o$/;

/**
 * 交给 `zig cc` 之前去掉的参数（cc-rs 的编译命令与 rustc 的链接命令共用）：
 * - `--target=<Rust triple>`：cc-rs 认出 clang 后加的；zig 不认 Rust 的写法，目标由包装脚本的 `-target` 给出；
 * - rustc 自带的启动文件与 `-lc`：zig 自己加启动文件与 musl libc，两份同时在时 `_start` 重复定义；
 * - `-static-pie`：zig 0.15 不支持（只报 unused），去掉让结果确定：不带 PIE 的静态可执行文件。
 */
export function rewriteCcArgs(args) {
  return args.filter(
    (arg) =>
      !arg.startsWith("--target=") &&
      arg !== "-lc" &&
      arg !== "-static-pie" &&
      !SELF_CONTAINED_CRT.test(arg),
  );
}

/**
 * rustc 写给 gcc 类链接器的 response file（GNU 的写法）：空白分隔；反斜杠转义下一个字符；单引号里原样，
 * 双引号里反斜杠仍转义。
 */
export function parseResponseFile(text) {
  const args = [];
  let current = "";
  let started = false;
  let quote = null;
  for (let i = 0; i < text.length; i += 1) {
    const c = text[i];
    if (quote === "'") {
      if (c === "'") quote = null;
      else current += c;
    } else if (c === "\\" && i + 1 < text.length) {
      i += 1;
      current += text[i];
      started = true;
    } else if (quote === '"') {
      if (c === '"') quote = null;
      else current += c;
    } else if (c === "'" || c === '"') {
      quote = c;
      started = true;
    } else if (/\s/.test(c)) {
      if (started) args.push(current);
      current = "";
      started = false;
    } else {
      current += c;
      started = true;
    }
  }
  if (started) args.push(current);
  return args;
}

/**
 * 写给 zig 读的 response file：每个参数一行、放在双引号里。zig 按 Windows 命令行的规则读（实测）：反斜杠
 * 只在引号前才转义，所以引号写成 `\"`，紧挨在引号（含收尾的引号）前面的反斜杠加倍。
 */
export function formatResponseFile(args) {
  return args
    .map((arg) => {
      let out = "";
      let slashes = 0;
      for (const c of arg) {
        if (c === "\\") {
          slashes += 1;
          continue;
        }
        if (c === '"') out += "\\".repeat(slashes * 2 + 1) + '"';
        else out += "\\".repeat(slashes) + c;
        slashes = 0;
      }
      return `"${out}${"\\".repeat(slashes * 2)}"\n`;
    })
    .join("");
}
