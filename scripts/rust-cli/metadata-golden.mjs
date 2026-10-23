#!/usr/bin/env node
// `metadata.user_id` 的 Node 对照夹具（docs/specs/rust-cli/model.md §3.3，工作线 F52）：用 Node 的
// `createAnthropicRequestMetadataUserId`（`apps/zcode-cli/packages/adapters/src/model/anthropic-request-metadata.ts`）
// 生成 `packages/rust-cli/crates/model/tests/fixtures/metadata-golden.json`。本工程的 `metadata::user_id` 对同一输入
// 必须写出相同的文本（`metadata_tests.rs`）。
//
//   node scripts/rust-cli/metadata-golden.mjs [--node-root <装好依赖的检出>]
//
// 只调用纯函数；导入期间 HOME 仍指向临时目录（`withHome`），不读本机的 ~/.zcode。
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { ensureTypeScript, importNode, nodeRootFromArgs, repoRoot, withHome } from "./node-root.mjs";

const { nodeRoot, args } = nodeRootFromArgs(process.argv.slice(2));
ensureTypeScript(import.meta.filename, nodeRoot, args);

const DEVICE = "7f1431f0-53e8-41c1-9cf9-22d11aa51d6a";
const SESSIONS = [
  "sess_2d480878-38af-4bae-a54f-096e2fb0f4d7",
  "sess_subagent_agent_2d480878-38af-4bae-a54f-096e2fb0f4d7",
  "subagent_agent_abc",
  "sess_",
  "subagent_agent_",
  "sess_subagent_agent_",
  "sess_sess_x",
  "subagent_agent_sess_x",
  "query_1",
  "plain",
  "",
  null,
  'quote"back\\slash\u0001 中文',
];

const home = await mkdtemp(join(tmpdir(), "rust-cli-metadata-golden-"));
try {
  const cases = await withHome(home, async () => {
    const { createAnthropicRequestMetadataUserId } = await importNode(
      nodeRoot,
      "adapters/src/model/anthropic-request-metadata.ts",
    );
    return SESSIONS.flatMap((session) =>
      [DEVICE, 'dev"ice'].map((device) => ({
        device,
        session,
        userId: createAnthropicRequestMetadataUserId({
          deviceMid: device,
          sessionId: session ?? undefined,
        }),
      })),
    );
  });
  const out = join(repoRoot, "packages/rust-cli/crates/model/tests/fixtures/metadata-golden.json");
  await writeFile(out, `${JSON.stringify(cases, null, 2)}\n`);
  console.log(`metadata-golden: ${cases.length} cases → ${out}`);
} finally {
  await rm(home, { recursive: true, force: true });
}
