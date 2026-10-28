#!/usr/bin/env node
// pre-push 的 rust-cli 检查（docs/testing/pre-push-hook.md「rust-cli 检查」，rust-cli 审查 R2-3）：
// push 范围改了 rust-cli 时跑 pnpm check:rust-cli（边界、fmt、clippy -D warnings）。
// push 范围沿用 affected 的 resolveScope，不另算一套；开源树没有 packages/rust-cli，这一步不跑。
// 有 scripts/rust-cli/export/scan.mjs 时（只在闭源仓库），先全量扫描会导出的文件（docs/specs/rust-cli/export.md §4；
// P5 注释中性化之后不再只扫新增的行）。
import { access, realpath } from "node:fs/promises";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { git, run } from "./engineering-tests/process.mjs";
import { resolveScope } from "./engineering-tests/scope.mjs";

// clippy 与 rustfmt 的版本由 packages/rust-cli/rust-toolchain.toml 固定，按前缀算在里面（工作线 F46 把它
// 从仓库根移到这里）。
const PREFIXES = ["packages/rust-cli/", "scripts/rust-cli/"];
const SKIP = "ZCODE_PRE_PUSH_SKIP_RUST_CLI";
const SCAN = "scripts/rust-cli/export/scan.mjs";
const SHOWN = 20;

const isRustCli = (file) => PREFIXES.some((p) => file.startsWith(p));

/** push 范围里属于 rust-cli 的文件。基线与校验（脏工作区、目标不是 HEAD）都由 resolveScope 给出。 */
export async function rustCliChanges(root, env) {
  return (await rustCliScope(root, env)).files;
}

/** push 范围的基线与其中属于 rust-cli 的文件。 */
async function rustCliScope(root, env) {
  const { bases } = await resolveScope(root, { scope: "push" }, env);
  const files = new Set();
  for (const base of bases) {
    // 与 Vitest changed 相同的三点比较；--no-renames 让移出 rust-cli 的文件也算改动。
    const out = await git(root, ["diff", "--name-only", "--no-renames", "-z", `${base}...HEAD`]);
    for (const file of out.toString().split("\0")) if (file && isRustCli(file)) files.add(file);
  }
  return { bases, files: [...files].sort() };
}

/**
 * 导出文件的全量扫描：scan.mjs 的 `scanAll`；没有扫描脚本（导出树、开源树）时返回 null。resolveScope 已经拒绝
 * 脏工作区，所以扫的工作区就是要 push 的 HEAD。
 */
export async function scanExported(root, env) {
  const path = join(root, SCAN);
  if (!(await exists(path))) return null;
  const { scanAll } = await import(pathToFileURL(path).href);
  return scanAll(root, { env });
}

export async function cargoAvailable(root, env) {
  try {
    // rustup 只从当前目录往上找 rust-toolchain.toml：在 packages/rust-cli 下探测，与 check:rust-cli 用同一个
    // 工具链（工作线 F46）。
    const cwd = join(root, "packages/rust-cli");
    return (await run("cargo", ["--version"], { cwd, env, allowFailure: true })).code === 0;
  } catch {
    return false;
  }
}

async function runCheck(root, env) {
  // pnpm run 会给脚本设置 npm_execpath；沿用 run-cli-lint 的写法，Windows 上也不经 shell。
  if (!env.npm_execpath) throw new Error("请经 pnpm verify:pre-push 运行（缺少 npm_execpath）");
  return (
    await run(process.execPath, [env.npm_execpath, "run", "check:rust-cli"], {
      cwd: root,
      env,
      inherit: true,
      allowFailure: true,
    })
  ).code;
}

async function exists(path) {
  try {
    await access(path);
    return true;
  } catch {
    return false;
  }
}

export async function main({
  root,
  env = process.env,
  log = console,
  hasCargo = cargoAvailable,
  check = runCheck,
  scan = scanExported,
} = {}) {
  try {
    // 根目录按脚本位置取，不问 git：开源导出排除 packages/rust-cli（opensource/export.config.json），
    // 那时这一步不调用 git 与 cargo，也不读 push 范围。
    root ??= resolve(import.meta.dirname, "..");
    if (!(await exists(join(root, "packages/rust-cli/Cargo.toml")))) return 0;
    const { files } = await rustCliScope(root, env);
    if (!files.length) {
      log.log("[rust-cli] push 范围没有改 rust-cli，不跑 check:rust-cli");
      return 0;
    }
    const listed = `${files.length} 个文件：${files.slice(0, 5).join(", ")}${files.length > 5 ? " …" : ""}`;
    if (env[SKIP] === "1") {
      log.warn(
        `[rust-cli] ${SKIP}=1：跳过 check:rust-cli，push 范围里的 rust-cli 改动没有检查（${listed}）`,
      );
      return 0;
    }
    const scanned = await scan(root, env);
    if (scanned) {
      if (scanned.hard.length) {
        log.error(
          [
            `[rust-cli] 会导出的文件里有 ${scanned.hard.length} 处只能留在闭源仓库的出处（docs/specs/rust-cli/export.md §4）：`,
            ...scanned.hard
              .slice(0, SHOWN)
              .map((f) => `  ${f.file}:${f.line} [${f.term}] ${f.text}`),
            ...(scanned.hard.length > SHOWN ? [`  …另有 ${scanned.hard.length - SHOWN} 处`] : []),
            "  改成中性的说法（描述行为本身，出处写进 spec）；对齐用的测试放进 parity 文件。",
            `  合并带入、不是你写的：${SKIP}=1 git push（跳过整个 rust-cli 检查）。`,
          ].join("\n"),
        );
        return 1;
      }
      log.log(`[rust-cli] 导出文件的扫描：没有硬失败，告警 ${scanned.warn.length} 处`);
    }
    if (!(await hasCargo(root, env))) {
      log.error(
        [
          `[rust-cli] push 范围改了 rust-cli（${listed}），要跑 pnpm check:rust-cli，但没有可用的 cargo（cargo --version 失败）。`,
          "  安装 Rust 工具链（rustup；版本与组件由 packages/rust-cli/rust-toolchain.toml 固定）后再 push。",
          `  改动只是合并带入、你没有改 rust-cli 时：${SKIP}=1 git push（只跳过这一项）。`,
        ].join("\n"),
      );
      return 2;
    }
    log.log(`[rust-cli] push 范围改了 rust-cli（${listed}），运行 pnpm check:rust-cli`);
    return (await check(root, env)) === 0 ? 0 : 1;
  } catch (error) {
    log.error(`[rust-cli] ${error instanceof Error ? error.message : String(error)}`);
    return 2;
  }
}

// 按真实路径比较：经符号链接的路径启动时（如 macOS 的 /var → /private/var），import.meta.url 是
// 真实路径，直接比较会不跑 main、静默 exit 0。
if (process.argv[1] && import.meta.url === pathToFileURL(await realpath(process.argv[1])).href)
  process.exitCode = await main();
