// 在 macOS / Linux 上交叉检查 Windows 目标（docs/specs/rust-cli/performance.md §8）：target、桩工具的环境变量与
// 包装脚本、桩工具要写的输出文件。纯函数，clippy.mjs 与 stub-tool.mjs 用。

/** Windows 的发布目标（npm 的 `win32-x64` 平台包，发布仓库的 platforms.json）。 */
export const WINDOWS_TARGET = "x86_64-pc-windows-msvc";

/** `clippy.mjs [--target windows]`：没有参数时检查本机目标。 */
export function parseCli(argv) {
  if (argv.length === 0) return { windows: false };
  if (argv.length === 2 && argv[0] === "--target" && argv[1] === "windows")
    return { windows: true };
  throw new Error(`不认识的参数 ${argv.join(" ")}；用法：clippy.mjs [--target windows]`);
}

/**
 * cc-rs 按 target 读的环境变量：C 与 C++ 编译器、归档器都换成桩（本机没有 MSVC 与 Windows SDK）。只设这个 target 的
 * 变量，主机上的 build.rs 与过程宏照常用本机的编译器。
 */
export function stubEnv({ cc, ar }) {
  const lower = WINDOWS_TARGET.replaceAll("-", "_");
  return { [`CC_${lower}`]: cc, [`CXX_${lower}`]: cc, [`AR_${lower}`]: ar };
}

const shQuote = (text) => `'${text.replaceAll("'", "'\\''")}'`;

/** cc-rs 调用的包装脚本（sh）：只转给 `node stub-tool.mjs <cc|ar>`。主机是 Windows 时不用桩（有 MSVC）。 */
export function stubLauncher({ node, script, tool }) {
  return (
    "#!/bin/sh\n" +
    "# 由 scripts/rust-cli/clippy.mjs 生成（docs/specs/rust-cli/performance.md §8），不要手改。\n" +
    `exec ${shQuote(node)} ${shQuote(script)} ${tool} "$@"\n`
  );
}

/**
 * 桩工具要写的空文件。编译器（cl 的写法，cc-rs 也可能给 GNU 的 `-o`）：`-Fo<文件>` / `/Fo<文件>`、`-o <文件>`
 * （不认 `-o<文件>`：会与 cl 的 `-openmp` 一类选项混淆）；`-E`（探测编译器族）与 `-?`（探测 cl 写法）不写文件。归档器（lib 的写法）：`-out:<文件>` /
 * `/OUT:<文件>`。不认识的工具报错。
 */
export function stubOutputs(tool, args) {
  if (tool === "ar") {
    return args.filter((arg) => /^[-/]out:/iu.test(arg)).map((arg) => arg.slice("-out:".length));
  }
  if (tool !== "cc") throw new Error(`不认识的工具 ${tool}`);
  if (args.includes("-E") || args.includes("-?")) return [];
  const outputs = [];
  for (let i = 0; i < args.length; i += 1) {
    const arg = args[i];
    if (/^[-/]Fo./u.test(arg)) outputs.push(arg.slice(3));
    else if (arg === "-o" && args[i + 1]) outputs.push(args[(i += 1)]);
  }
  return outputs;
}

/** `cargo clippy` 的参数：Windows 目标时加 `--target`。 */
export function clippyArgs({ windows, features }) {
  return [
    "clippy",
    "--locked",
    "--workspace",
    "--all-targets",
    ...(windows ? ["--target", WINDOWS_TARGET] : []),
    ...features,
    "--",
    "-D",
    "warnings",
  ];
}
