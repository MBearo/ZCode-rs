// rust-cli 与 Node CLI 互操作脚本的公共部分（docs/specs/rust-cli/node-store.md §6）。
//
// 脚本直接导入 apps/zcode-cli 的 TypeScript 源码，需要一份装好依赖的检出（node_modules 与 tsx）。
// 默认用本仓库；worktree 里没有 node_modules 时用 `--node-root <目录>` 或 ZCODE_NODE_ROOT 指向
// 同一提交、装过依赖的检出（只读取，不写入）。没有 tsx 时给出提示并以 2 退出。
import { spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import { readFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";

export const repoRoot = resolve(import.meta.dirname, "../..");

/** 解析 `--node-root`，返回 `{ nodeRoot, args }`（args 去掉了该参数）。 */
export function nodeRootFromArgs(argv) {
  const args = [...argv];
  let nodeRoot = process.env.ZCODE_NODE_ROOT || repoRoot;
  const index = args.indexOf("--node-root");
  if (index >= 0) {
    nodeRoot = args[index + 1];
    args.splice(index, 2);
  }
  return { nodeRoot: resolve(nodeRoot), args };
}

/**
 * 保证当前进程能导入 TypeScript：不在 tsx 下时用 `<nodeRoot>/node_modules/tsx` 重新执行本脚本。
 * 返回 true 表示调用方应继续执行（已在 tsx 下）；否则本函数以子进程的退出码结束进程。
 */
export function ensureTypeScript(script, nodeRoot, args) {
  if (process.env.RUST_CLI_NODE_TSX === "1") return true;
  const loader = join(nodeRoot, "node_modules/tsx/dist/loader.mjs");
  const adapters = join(nodeRoot, "apps/zcode-cli/packages/adapters/node_modules");
  if (!existsSync(loader) || !existsSync(adapters)) {
    console.error(
      `Node CLI sources with installed dependencies are needed at ${nodeRoot}.\n` +
        "Run `pnpm install` there, or pass --node-root <checkout with node_modules> " +
        "(or set ZCODE_NODE_ROOT).",
    );
    process.exit(2);
  }
  const result = spawnSync(
    process.execPath,
    [
      "--no-warnings",
      "--import",
      pathToFileURL(loader).href,
      script,
      "--node-root",
      nodeRoot,
      ...args,
    ],
    { stdio: "inherit", env: { ...process.env, RUST_CLI_NODE_TSX: "1" } },
  );
  process.exit(result.status ?? 1);
}

/** 导入 Node CLI 的一个源文件（相对 `<nodeRoot>/apps/zcode-cli/packages`）。 */
export function importNode(nodeRoot, path) {
  return import(pathToFileURL(join(nodeRoot, "apps/zcode-cli/packages", path)).href);
}

/**
 * 在 `run` 期间把本进程的 HOME / USERPROFILE 指向临时 `home`（修复线 F50）。Node 的部分代码按本进程的
 * `os.homedir()` 找 `~/.zcode`，不看传入的 env：登录写个人配置时导入旧 CLI 用户文件 `~/.zcode/cli/config.json`
 * （`readLegacyCliPersonalProviderConfig`），`createConfig` 读用户配置。不这样做会把本机真实的 provider（带 key）读进
 * 临时目录，之后 `zcode -p` 可能用它真的联网。只在这一段调用期间改，结束后恢复（调用方不并发）。
 */
export async function withHome(home, run) {
  const saved = { HOME: process.env.HOME, USERPROFILE: process.env.USERPROFILE };
  process.env.HOME = home;
  process.env.USERPROFILE = home;
  try {
    return await run();
  } finally {
    for (const [name, value] of Object.entries(saved)) {
      if (value === undefined) delete process.env[name];
      else process.env[name] = value;
    }
  }
}

/**
 * 个人配置（`provider_config.json`）里 provider 的个数；文件不存在时是 0。临时 home 里不该有 provider，有就是从临时
 * home 以外导入的，调用方不再运行 rust-cli。只给个数：内容可能有 key，解析失败时也不带出原文。
 */
export async function personalProviderCount(file) {
  let text;
  try {
    text = await readFile(file, "utf8");
  } catch (error) {
    if (error?.code === "ENOENT") return 0;
    throw new Error(`cannot read ${file}`);
  }
  let personal;
  try {
    personal = JSON.parse(text);
  } catch {
    throw new Error(`${file} is not JSON`);
  }
  return personal?.config?.providerConfigRules?.providerRules?.length ?? 0;
}
