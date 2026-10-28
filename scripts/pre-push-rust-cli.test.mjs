// pre-push 的 rust-cli 检查（docs/testing/pre-push-hook.md「rust-cli 检查」）：临时 Git 仓库里
// 验证 push 范围的判断、开源树、没有 Rust 工具链与显式跳过；check:rust-cli 本身用注入的替身。
import { execFile } from "node:child_process";
import { chmod, copyFile, mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { promisify } from "node:util";
import { afterEach, describe, expect, it, vi } from "vitest";

import { cargoAvailable, main, rustCliChanges } from "./pre-push-rust-cli.mjs";

const exec = promisify(execFile);
const zero = "0".repeat(40);
const roots = [];

afterEach(async () => {
  await Promise.all(roots.splice(0).map((root) => rm(root, { recursive: true, force: true })));
});

// 外层 hook 的 Git local env 不属于夹具；与 affected 的夹具一样用环境副本隔离。
const gitEnv = Object.fromEntries(
  Object.entries(process.env).filter(([key]) => !key.startsWith("GIT_")),
);

async function fixture({ rustCli = true } = {}) {
  const root = await mkdtemp(join(tmpdir(), "pre-push-rust-cli-"));
  roots.push(root);
  const write = async (path, content) => {
    await mkdir(dirname(join(root, path)), { recursive: true });
    await writeFile(join(root, path), content);
  };
  const git = async (...args) =>
    (
      await exec(
        "git",
        [
          "-c",
          "user.name=Test",
          "-c",
          "user.email=test@example.invalid",
          "-c",
          "commit.gpgsign=false",
          ...args,
        ],
        { cwd: root, env: gitEnv },
      )
    ).stdout.trim();
  if (rustCli) {
    await write("packages/rust-cli/Cargo.toml", "[workspace]\n");
    await write("packages/rust-cli/crates/base/src/lib.rs", "// base\n");
    await write("scripts/rust-cli/check-boundaries.mjs", "// boundaries\n");
  }
  await write("docs/specs/rust-cli/README.md", "# spec\n");
  await write("packages/ui/src/a.ts", "export {};\n");
  await write("rust-toolchain.toml", "[toolchain]\n");
  await git("init", "-b", "main");
  await git("add", "-A");
  await git("commit", "-m", "base");
  const base = await git("rev-parse", "HEAD");
  const change = async (path, content = "// changed\n") => {
    await write(path, content);
    await git("add", "-A");
    await git("commit", "-m", `change ${path}`);
    return git("rev-parse", "HEAD");
  };
  // 模拟 hook 的 stdin：本地 ref 指向当前 HEAD，remote SHA 是上次推送的位置。
  const push = async (...remotes) => {
    const head = await git("rev-parse", "HEAD");
    const lines = (remotes.length ? remotes : [base]).map(
      (remote, i) => `refs/heads/b${i} ${head} refs/heads/b${i} ${remote}`,
    );
    return { ZCODE_PRE_PUSH_STDIN: lines.join("\n"), npm_execpath: "/pnpm.cjs" };
  };
  return { root, write, git, base, change, push };
}

function harness({ cargo = true, code = 0 } = {}) {
  const lines = { log: [], warn: [], error: [] };
  const log = {
    log: (line) => lines.log.push(line),
    warn: (line) => lines.warn.push(line),
    error: (line) => lines.error.push(line),
  };
  const hasCargo = vi.fn(async () => cargo);
  const check = vi.fn(async () => code);
  return { lines, log, hasCargo, check };
}

describe("pre-push 的 rust-cli 检查", () => {
  it("改了 packages/rust-cli：跑 check:rust-cli，它的结果决定退出码", async () => {
    const { root, change, push } = await fixture();
    await change("packages/rust-cli/crates/base/src/lib.rs");
    const env = await push();
    const ok = harness();
    expect(await main({ root, env, ...ok })).toBe(0);
    expect(ok.check).toHaveBeenCalledTimes(1);
    expect(ok.check).toHaveBeenCalledWith(root, env);
    expect(ok.lines.log.join("\n")).toMatch(/packages\/rust-cli\/crates\/base\/src\/lib\.rs/u);
    const failed = harness({ code: 101 });
    expect(await main({ root, env, ...failed })).toBe(1);
  });

  // 工具链文件在 packages/rust-cli 下（工作线 F46），按前缀算。
  it.each(["scripts/rust-cli/check-boundaries.mjs", "packages/rust-cli/rust-toolchain.toml"])(
    "改了 %s 也跑",
    async (path) => {
      const { root, change, push } = await fixture();
      await change(path);
      expect(await rustCliChanges(root, await push())).toEqual([path]);
    },
  );

  it("只改 rust-cli 的 spec 或其他目录：不跑，也不探测 cargo", async () => {
    const { root, change, push } = await fixture();
    await change("docs/specs/rust-cli/README.md");
    await change("packages/ui/src/a.ts");
    // 仓库根的 rust-toolchain.toml 不属于 rust-cli：工具链文件已移到 packages/rust-cli（工作线 F46）。
    await change("rust-toolchain.toml");
    const h = harness({ cargo: false });
    expect(await main({ root, env: await push(), ...h })).toBe(0);
    expect(h.hasCargo).not.toHaveBeenCalled();
    expect(h.check).not.toHaveBeenCalled();
    expect(h.lines.log.join("\n")).toMatch(/没有改 rust-cli/u);
  });

  it("检出里没有 packages/rust-cli（开源树）：什么都不做，不解析 push 范围", async () => {
    const { root } = await fixture({ rustCli: false });
    const h = harness({ cargo: false });
    // 无效的 hook 输入也不报错：这一步根本没有读 push 范围。
    expect(await main({ root, env: { ZCODE_PRE_PUSH_STDIN: "bad" }, ...h })).toBe(0);
    expect(h.hasCargo).not.toHaveBeenCalled();
    expect(h.check).not.toHaveBeenCalled();
    expect([...h.lines.log, ...h.lines.warn, ...h.lines.error]).toEqual([]);
  });

  it("入口在没有 packages/rust-cli 的检出里直接退出 0，不调用 git", async () => {
    // 复制入口与它导入的模块到一个不是 Git 仓库的目录：问 git 要根目录会失败（exit 2）。
    const copy = await mkdtemp(join(tmpdir(), "pre-push-rust-cli-copy-"));
    roots.push(copy);
    await mkdir(join(copy, "scripts/engineering-tests"), { recursive: true });
    for (const file of [
      "pre-push-rust-cli.mjs",
      "check-push-worktree.mjs",
      "engineering-tests/process.mjs",
      "engineering-tests/scope.mjs",
    ])
      await copyFile(join(import.meta.dirname, file), join(copy, "scripts", file));
    const result = await exec(process.execPath, [join(copy, "scripts/pre-push-rust-cli.mjs")], {
      cwd: copy,
      env: { ...gitEnv, GIT_CEILING_DIRECTORIES: dirname(copy) },
    });
    expect(result.stdout + result.stderr).toBe("");
  });

  it("没有 Rust 工具链：阻止 push，列出文件并说明安装与跳过的写法", async () => {
    const { root, change, push } = await fixture();
    await change("packages/rust-cli/crates/base/src/lib.rs");
    const h = harness({ cargo: false });
    expect(await main({ root, env: await push(), ...h })).toBe(2);
    expect(h.check).not.toHaveBeenCalled();
    const message = h.lines.error.join("\n");
    expect(message).toMatch(/packages\/rust-cli\/crates\/base\/src\/lib\.rs/u);
    expect(message).toMatch(/rustup/u);
    expect(message).toMatch(/rust-toolchain\.toml/u);
    expect(message).toMatch(/ZCODE_PRE_PUSH_SKIP_RUST_CLI=1 git push/u);
  });

  it("ZCODE_PRE_PUSH_SKIP_RUST_CLI=1：只跳过这一项，并警告没有检查", async () => {
    const { root, change, push } = await fixture();
    await change("packages/rust-cli/crates/base/src/lib.rs");
    const h = harness({ cargo: false });
    const env = { ...(await push()), ZCODE_PRE_PUSH_SKIP_RUST_CLI: "1" };
    expect(await main({ root, env, ...h })).toBe(0);
    expect(h.hasCargo).not.toHaveBeenCalled();
    expect(h.check).not.toHaveBeenCalled();
    expect(h.lines.warn.join("\n")).toMatch(/没有检查/u);
  });

  // 导出文件的全量扫描（P5 之后，docs/specs/rust-cli/export.md §4）：替身扫描脚本按环境变量给出结果。
  const FAKE_SCAN = [
    "export async function scanAll(root, { env }) {",
    "  if (env.FAKE_SCAN_THROW) throw new Error('scan should not run');",
    "  const hard = env.FAKE_SCAN_HARD ? [{ file: 'packages/rust-cli/x.rs', line: 3, term: 'sample', text: 'old hit' }] : [];",
    "  return { hard, warn: [{ file: 'packages/rust-cli/x.rs', line: 4, term: 'short-name', text: '' }] };",
    "}",
    "",
  ].join("\n");

  it("有扫描脚本：全量扫描会导出的文件，有硬失败时阻止 push、不跑 check:rust-cli", async () => {
    const { root, write, git, change, push } = await fixture();
    await write("scripts/rust-cli/export/scan.mjs", FAKE_SCAN);
    await git("add", "-A");
    await git("commit", "-m", "scan");
    await change("packages/rust-cli/crates/base/src/lib.rs");
    // 命中不在 push 范围新增的行里，全量扫描照样报。
    const env = { ...(await push()), FAKE_SCAN_HARD: "1" };
    const h = harness();
    expect(await main({ root, env, ...h })).toBe(1);
    expect(h.check).not.toHaveBeenCalled();
    const message = h.lines.error.join("\n");
    expect(message).toMatch(/old hit/u);
    expect(message).toMatch(/export\.md/u);
    // 没有硬失败：只报告告警数，照常跑 check:rust-cli。
    const clean = harness();
    expect(await main({ root, env: await push(), ...clean })).toBe(0);
    expect(clean.check).toHaveBeenCalledTimes(1);
    expect(clean.lines.log.join("\n")).toMatch(/告警 1 处/u);
  });

  it("有扫描脚本时 ZCODE_PRE_PUSH_SKIP_RUST_CLI=1 同样跳过扫描", async () => {
    const { root, write, git, change, push } = await fixture();
    await write("scripts/rust-cli/export/scan.mjs", FAKE_SCAN);
    await git("add", "-A");
    await git("commit", "-m", "scan");
    await change("packages/rust-cli/crates/base/src/lib.rs");
    const env = { ...(await push()), ZCODE_PRE_PUSH_SKIP_RUST_CLI: "1", FAKE_SCAN_THROW: "1" };
    const h = harness();
    expect(await main({ root, env, ...h })).toBe(0);
    expect(h.check).not.toHaveBeenCalled();
  });

  it("合并带入的 rust-cli 改动照常算（与 affected 相同，不按来源过滤）", async () => {
    const { root, git, change, push } = await fixture();
    await git("switch", "-c", "side");
    await change("packages/rust-cli/crates/base/src/lib.rs");
    await git("switch", "main");
    await change("packages/ui/src/a.ts");
    await git("merge", "--no-ff", "-m", "merge side", "side");
    expect(await rustCliChanges(root, await push())).toEqual([
      "packages/rust-cli/crates/base/src/lib.rs",
    ]);
  });

  it("把文件移出 rust-cli 也算改了 rust-cli", async () => {
    const { root, git, push } = await fixture();
    await mkdir(join(root, "packages/other"), { recursive: true });
    await git("mv", "packages/rust-cli/crates/base/src/lib.rs", "packages/other/lib.rs");
    await git("commit", "-m", "move out");
    expect(await rustCliChanges(root, await push())).toEqual([
      "packages/rust-cli/crates/base/src/lib.rs",
    ]);
  });

  it("首次推送按 origin/staging 的 merge-base；只删除 ref 时不跑", async () => {
    const { root, git, base, change, push } = await fixture();
    await git("update-ref", "refs/remotes/origin/staging", base);
    await change("packages/rust-cli/crates/base/src/lib.rs");
    expect(await rustCliChanges(root, await push(zero))).toEqual([
      "packages/rust-cli/crates/base/src/lib.rs",
    ]);
    const deleted = { ZCODE_PRE_PUSH_STDIN: `(delete) ${zero} refs/heads/gone ${base}` };
    const h = harness();
    expect(await main({ root, env: deleted, ...h })).toBe(0);
    expect(h.check).not.toHaveBeenCalled();
  });

  it("多个 ref 合并各自的范围", async () => {
    const { root, base, change, push } = await fixture();
    const afterRustCli = await change("packages/rust-cli/crates/base/src/lib.rs");
    await change("packages/ui/src/a.ts");
    expect(await rustCliChanges(root, await push(afterRustCli))).toEqual([]);
    expect(await rustCliChanges(root, await push(afterRustCli, base))).toEqual([
      "packages/rust-cli/crates/base/src/lib.rs",
    ]);
  });

  it("脏工作区或推送目标不是 HEAD：与 affected 一样 exit 2", async () => {
    const { root, base, write, change, push } = await fixture();
    await change("packages/rust-cli/crates/base/src/lib.rs");
    const notHead = { ZCODE_PRE_PUSH_STDIN: `refs/heads/main ${base} refs/heads/main ${base}` };
    const h = harness();
    expect(await main({ root, env: notHead, ...h })).toBe(2);
    expect(h.lines.error.join("\n")).toMatch(/不是当前 HEAD/u);
    await write("untracked.txt", "x");
    expect(await main({ root, env: await push(), ...h })).toBe(2);
    expect(h.lines.error.join("\n")).toMatch(/工作区不干净/u);
    expect(h.check).not.toHaveBeenCalled();
  });

  it("不经 pnpm 运行（没有 npm_execpath）时报错，不静默通过", async () => {
    const { root, change, push } = await fixture();
    await change("packages/rust-cli/crates/base/src/lib.rs");
    const { npm_execpath: _, ...env } = await push();
    const h = harness();
    expect(await main({ root, env, log: h.log, hasCargo: h.hasCargo })).toBe(2);
    expect(h.lines.error.join("\n")).toMatch(/pnpm/u);
  });

  it("PATH 里没有 cargo 时探测结果为 false", async () => {
    const { root } = await fixture();
    const empty = await mkdtemp(join(tmpdir(), "pre-push-rust-cli-path-"));
    roots.push(empty);
    expect(await cargoAvailable(root, { PATH: empty })).toBe(false);
  });

  // rustup 只从当前目录往上找 rust-toolchain.toml，不看 --manifest-path；工具链文件在 packages/rust-cli
  // 下（工作线 F46），探测要在那里做，才与 check:rust-cli 用同一个工具链。替身 cargo 是 sh 脚本。
  it.skipIf(process.platform === "win32")("在 packages/rust-cli 下探测 cargo", async () => {
    const { root } = await fixture();
    const bin = await mkdtemp(join(tmpdir(), "pre-push-rust-cli-bin-"));
    roots.push(bin);
    await writeFile(
      join(bin, "cargo"),
      `#!/bin/sh\n[ "$(pwd -P)" = "$(cd "${join(root, "packages/rust-cli")}" && pwd -P)" ]\n`,
    );
    await chmod(join(bin, "cargo"), 0o755);
    expect(await cargoAvailable(root, { PATH: bin })).toBe(true);
  });
});
