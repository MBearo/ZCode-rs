#!/usr/bin/env node
// 项目 hooks 信任 digest 的 Node 对照（docs/specs/rust-cli/config.md §4.1、§5）。
//
// 用 Node 的 `buildWorkspaceHookBundleSnapshot`（packages/shared/src/workspace-hook-digest.ts）对夹具
// `packages/rust-cli/crates/cli/tests/fixtures/workspace-hooks.json` 里的每组配置算出 digest，写进同目录的
// `workspace-hooks-golden.json`；`--check` 只比较。Rust 测试（crates/cli/src/config/trust_tests.rs）读同一份
// 夹具与结果逐项比较。
//
// 用法：node scripts/rust-cli/hook-digest-golden.mjs [--check] [--node-root <装好依赖的检出>]
import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { isDeepStrictEqual } from "node:util";
import { ensureTypeScript, importNode, nodeRootFromArgs, repoRoot } from "./node-root.mjs";

const { nodeRoot, args } = nodeRootFromArgs(process.argv.slice(2));
ensureTypeScript(import.meta.filename, nodeRoot, args);
const check = args.includes("--check");

const fixtures = join(repoRoot, "packages/rust-cli/crates/cli/tests/fixtures");
const cases = JSON.parse(readFileSync(join(fixtures, "workspace-hooks.json"), "utf8"));
const discovery = await import(
  pathToFileURL(join(nodeRoot, "packages/shared/src/workspace-hook-discovery.ts")).href
);
const { DefaultRuntimeConfig } = await importNode(nodeRoot, "contracts/src/config/index.ts");

const results = cases.map((testCase) => {
  const sources = testCase.sources.map((source) =>
    discovery.createWorkspaceHookSourceInput({
      path: source.path,
      workingDirectory: testCase.workspacePath,
      hooks: discovery.workspaceHooksConfigSchema.parse(source.hooks),
      discoveryOrder: source.discoveryOrder,
    }),
  );
  const userHooks = testCase.userHooks
    ? discovery.workspaceHooksConfigSchema.parse(testCase.userHooks)
    : undefined;
  const runtimeRoot = discovery.resolveWorkspaceHookRuntimeRoot([
    DefaultRuntimeConfig.hooks,
    userHooks,
    ...sources.map((source) => source.hooks),
  ]);
  const snapshot = discovery.buildWorkspaceHookBundleSnapshot({
    workspaceIdentity: testCase.workspacePath,
    workspacePath: testCase.workspacePath,
    sources,
    runtimeRoot,
  });
  return {
    name: testCase.name,
    bundleDigest: snapshot?.bundleDigest ?? null,
    hooks: (snapshot?.hooks ?? []).map((entry) => ({
      reviewItemId: entry.reviewItemId,
      sourceRelativePath: entry.sourceRelativePath,
      resolvedTimeoutMs: entry.resolvedTimeoutMs,
      configuredEnabled: entry.configuredEnabled,
      editable: entry.editable,
      hookDeclarationDigest: entry.hookDeclarationDigest,
    })),
  };
});

const golden = join(fixtures, "workspace-hooks-golden.json");
const text = `${JSON.stringify(results, null, 2)}\n`;
if (check) {
  const current = JSON.parse(readFileSync(golden, "utf8"));
  if (!isDeepStrictEqual(current, results)) {
    console.error("workspace-hooks-golden.json is out of date; rerun without --check");
    process.exit(1);
  }
  console.log("hook-digest-golden: OK");
} else {
  writeFileSync(golden, text);
  console.log(`wrote ${golden}`);
}
