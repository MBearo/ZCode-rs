import { execFile } from "node:child_process";
import { access } from "node:fs/promises";
import { basename, dirname, resolve } from "node:path";
import { promisify } from "node:util";

const exec = promisify(execFile);

export async function npm(args, options = {}) {
  const configured = process.env.npm_execpath;
  const cli =
    configured && basename(configured) === "npm-cli.js"
      ? configured
      : resolve(
          dirname(process.execPath),
          process.platform === "win32"
            ? "node_modules/npm/bin/npm-cli.js"
            : "../lib/node_modules/npm/bin/npm-cli.js",
        );
  await access(cli);
  // Windows 也通过 node 运行 npm 的 JS 入口，不经过 cmd 或 shell 拼接参数。
  return exec(process.execPath, [cli, ...args], { maxBuffer: 8_000_000, ...options });
}
