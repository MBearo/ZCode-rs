#!/usr/bin/env node
// rust-cli 登录写入的 Node 对照夹具（docs/specs/rust-cli/platform.md §7）：用 Node 的个人配置仓库
// （`NodePersonalProviderConfigRepository` + `NodeModelSelectionConfigRepository.saveConfiguredDefault`，与
// Node 独立 CLI 登录后的写入相同）生成 `provider_config.json` 的写入结果。在装好依赖的检出里运行：
//
//   node_modules/.bin/tsx scripts/rust-cli/login-golden.mts \
//     packages/rust-cli/crates/platform/tests/fixtures/personal.json \
//     packages/rust-cli/crates/platform/tests/fixtures [--node-root <装好依赖的检出>]
//
// worktree 里没有 node_modules 时用 `--node-root` 指向同一提交、装过依赖的检出（只读取它的源码）。
//
// 输出三个文件：
// - login-config-empty.json：文件不存在时登录 zai 写出的内容；
// - login-config-node.json：个人配置夹具经 Node 登录 bigmodel 后的内容（Node 顺带规范化整份文件）；
// - login-config-node-zai.json：在上一份的基础上再登录 zai 的内容（本工程对同一输入必须写出相同字节）。
import { copyFileSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";

const args = process.argv.slice(2);
const rootFlag = args.indexOf("--node-root");
const nodeRoot =
  rootFlag >= 0 ? resolve(args.splice(rootFlag, 2)[1]!) : resolve(import.meta.dirname, "../..");
const [personal, outDir] = args;
const { NodeModelSelectionConfigRepository, NodePersonalProviderConfigRepository } = await import(
  pathToFileURL(join(nodeRoot, "packages/provider-node/src/index.ts")).href
);
const ZAI = { providerId: "account:zai-individual-coding-plan", modelId: "GLM-5.3" };
const BIGMODEL = { providerId: "account:bigmodel-individual-coding-plan", modelId: "GLM-5.3" };

async function saveDefault(filePath: string, selection: typeof ZAI): Promise<string> {
  const personalRepository = new NodePersonalProviderConfigRepository({
    filePath,
    pollingIntervalMs: false,
  });
  const repository = new NodeModelSelectionConfigRepository({ personalRepository });
  try {
    await repository.saveConfiguredDefault(selection);
  } finally {
    repository.dispose();
    personalRepository.dispose();
  }
  return readFileSync(filePath, "utf8");
}

const dir = mkdtempSync(join(tmpdir(), "rust-cli-login-golden-"));
writeFileSync(
  join(outDir, "login-config-empty.json"),
  await saveDefault(join(dir, "empty.json"), ZAI),
);
const existing = join(dir, "existing.json");
copyFileSync(personal, existing);
writeFileSync(join(outDir, "login-config-node.json"), await saveDefault(existing, BIGMODEL));
writeFileSync(join(outDir, "login-config-node-zai.json"), await saveDefault(existing, ZAI));
process.exit(0);
