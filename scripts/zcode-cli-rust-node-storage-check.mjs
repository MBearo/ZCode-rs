// Run with node --import tsx. Rust writes scripted sessions (queued input and a
// restarted runtime continuing it; guided and removed busy inputs) into fresh
// Node session databases (apps/zcode-cli-rust/tests/node_storage.rs); Node's
// SqliteSessionStore, history hydrator and cold projection read it back and
// must agree with what Rust reads. Spec rust-m11-node-storage §5.2, §11.
import { execFileSync } from "node:child_process";
import { mkdtemp, readdir, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { isDeepStrictEqual } from "node:util";
import { SqliteSessionStore } from "../apps/zcode-cli/packages/adapters/src/storage/session-store/sqlite-session-store.ts";
import { createNodeToolArtifactStore } from "../apps/zcode-cli/packages/adapters/src/storage/index.ts";
import { readSessionModelSelection } from "../apps/zcode-cli/packages/bootstrap/src/app/session-store.ts";
import { hydrateMessageHistoryFromSession } from "../apps/zcode-cli/packages/core/src/agent/session-history-hydrator.ts";
import { createMessageHistory } from "../apps/zcode-cli/packages/core/src/agent/message-history.ts";
import { selectActiveConversationBranch } from "../apps/zcode-cli/packages/contracts/src/rewind/index.ts";
import { ProductProjection } from "../apps/zcode-cli/packages/bootstrap/src/zcode-protocol-v4/product-projection.ts";
import { mergeColdConversationEvents } from "../apps/zcode-cli/packages/bootstrap/src/zcode-protocol-v4/cold-event-merge.ts";
import {
  PERMISSION_FULL_ACCESS_ENTRY,
  permissionFullAccessReceiptSchema,
} from "../apps/zcode-cli/packages/contracts/src/interfaces/permission-full-access.ts";
import { readPersistedBashShellSelectionSnapshot } from "../apps/zcode-cli/packages/core/src/runtime/methods/bash-shell-snapshot.ts";
import { goalVerificationEntriesFromSessionEntries } from "../apps/zcode-cli/packages/bootstrap/src/zcode-protocol-v4/transcript-hydration.ts";

// 与夹具一致：发布层计数不属于冷投影契约。
const EPHEMERAL_SNAPSHOT_KEYS = new Set([
  "protocolVersion",
  "sessionId",
  "logEpoch",
  "seq",
  "revision",
  "rows",
]);

function expectEqual(name, node, rust) {
  if (isDeepStrictEqual(JSON.parse(JSON.stringify(node)), rust)) return;
  console.error(`${name} differs\nnode: ${JSON.stringify(node)}\nrust: ${JSON.stringify(rust)}`);
  process.exitCode = 1;
}

async function check(name, root, file) {
  const rust = JSON.parse(await readFile(join(root, file), "utf8"));
  if (!rust.history.length || !rust.rows.length) throw new Error(`${name}: Rust read nothing back`);
  const sessionID = rust.sessionId;
  const store = await SqliteSessionStore.openStartup({ dbPath: join(root, "cli/db/db.sqlite") });
  try {
    const session = await store.getSession(sessionID);
    if (!session) throw new Error(`${name}: Node cannot read session ${sessionID}`);
    const listed = await store.listSessions({ directory: session.directory });
    if (!listed.some((s) => s.id === sessionID)) throw new Error(`${name}: session not listed`);
    const selection = await readSessionModelSelection(store, sessionID);
    if (selection?.modelId !== "m") throw new Error(`${name}: model selection unreadable`);
    await store.listSessionInputs({ sessionID });
    const stored = await store.messages({ sessionID });
    const entries = await store.sessionEntries({ sessionID });
    // 会话 shell 快照：Node 恢复时按快照继续使用同一个 shell。
    const shell = await readPersistedBashShellSelectionSnapshot({
      sessionId: sessionID,
      sessionStore: store,
      traceContext: { traceId: "check" },
    });
    // Node 的稳定分叉不复制 shell 快照（恢复时按当前设置），其余会话在创建时写入。
    const expected = session.taskType === "fork" ? "missing" : "restored";
    if (shell.status !== expected) throw new Error(`${name}: shell snapshot ${shell.status}`);
    // 授权回执是 Node 恢复与重试的事实源，必须通过 Node 的严格 schema。
    for (const entry of entries.filter((e) => e.type === PERMISSION_FULL_ACCESS_ENTRY)) {
      permissionFullAccessReceiptSchema.parse(entry.data);
    }
    const revert = session.revert;
    const branchOptions = {
      branchCutAfterMessageId: revert?.branchCutAfterMessageID,
      rewindCreatedMessageId: revert?.createdMessageID,
      rewindKeptMessageIds: revert?.keptMessageIDs,
      rewindTargetMessageId: revert?.targetMessageID,
    };
    const history = createMessageHistory();
    // 附件与媒体按 Node 产物目录读回（spec §5.3）。
    const artifactStore = createNodeToolArtifactStore({
      imageCacheRootDir: join(root, "cli/image-cache"),
      pdfCacheRootDir: join(root, "cli/pdf-cache"),
      rootDir: join(root, "cli/artifacts"),
      videoCacheRootDir: join(root, "cli/video-cache"),
    });
    await hydrateMessageHistoryFromSession({
      artifactStore,
      history,
      messages: stored,
      ...branchOptions,
    });
    expectEqual(`${name} history`, history.toRuntimeEntries(), rust.history);
    const merged = mergeColdConversationEvents({
      memoryEvents: [],
      messages: selectActiveConversationBranch(stored, branchOptions),
      sessionId: sessionID,
      goalVerificationEntries: goalVerificationEntriesFromSessionEntries(entries),
      target: null,
    });
    const projection = new ProductProjection(sessionID, "epoch");
    projection.beginHydrationReplay();
    for (const event of merged.events) projection.applyHydrationEvent(event);
    projection.completeHydrationReplay();
    const snapshot = projection.getSnapshot();
    expectEqual(`${name} rows`, snapshot.rows.window, rust.rows);
    const withoutEphemeral = (state) =>
      Object.fromEntries(
        Object.entries(state).filter(([key]) => !EPHEMERAL_SNAPSHOT_KEYS.has(key)),
      );
    expectEqual(`${name} state`, withoutEphemeral(snapshot), withoutEphemeral(rust.state));
  } finally {
    store.close();
  }
}

const dir = await mkdtemp(join(tmpdir(), "zcode-node-storage-"));
try {
  execFileSync(
    "cargo",
    ["test", "-q", "--manifest-path", "apps/zcode-cli-rust/Cargo.toml", "--test", "node_storage"],
    {
      stdio: "inherit",
      env: {
        ...process.env,
        ZCODE_CLI_RUST_NODE_DUMP: dir,
        CARGO_INCREMENTAL: "0",
        CARGO_PROFILE_DEV_DEBUG: "0",
        CARGO_PROFILE_TEST_DEBUG: "0",
      },
    },
  );
  const scenarios = await readdir(dir);
  if (scenarios.length === 0) throw new Error("Rust wrote no sessions");
  const checked = [];
  for (const scenario of scenarios) {
    const root = join(dir, scenario);
    for (const file of await readdir(root)) {
      if (!/^rust.*\.json$/.test(file)) continue;
      await check(`${scenario}/${file}`, root, file);
      checked.push(file === "rust.json" ? scenario : `${scenario} (${file})`);
    }
  }
  if (!process.exitCode) {
    console.log(`Node reads the Rust-written sessions identically: ${checked.join(", ")}.`);
  }
} finally {
  await rm(dir, { recursive: true, force: true });
}
