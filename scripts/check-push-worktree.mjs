import { execFile } from "node:child_process";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { promisify } from "node:util";

const exec = promisify(execFile);

export async function checkPushWorktree(cwd = process.cwd(), env = process.env) {
  const git = async (...args) =>
    (await exec("git", args, { cwd, env, maxBuffer: 16 * 1024 * 1024 })).stdout.trim();
  // App 单测选择器退役后，仍须保证检查内容就是推送版本；不自动 stash 或修改 index。
  if (await git("status", "--porcelain=v1", "-z", "--untracked-files=all"))
    throw new Error("Push 工作区不干净：请先提交或自行保存已暂存、未暂存及未跟踪改动");
  const head = await git("rev-parse", "HEAD");
  for (const line of (env.ZCODE_PRE_PUSH_STDIN ?? "").split(/\r?\n/u).filter((s) => s.trim())) {
    const entry = line.trim().split(/\s+/u);
    if (
      entry.length !== 4 ||
      !/^[\da-f]{40,64}$/iu.test(entry[1]) ||
      !/^[\da-f]{40,64}$/iu.test(entry[3])
    )
      throw new Error("推送 ref 输入无效");
    if (/^0+$/u.test(entry[1])) continue;
    // 附注标签 SHA 指向 tag 对象；先剥离到 commit，避免发布时误拒绝指向 HEAD 的标签。
    const target = await git("rev-parse", "--verify", "--end-of-options", `${entry[1]}^{commit}`);
    if (target !== head) throw new Error("推送目标不是当前 HEAD");
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  try {
    await checkPushWorktree();
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
