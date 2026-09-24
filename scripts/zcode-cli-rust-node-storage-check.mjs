// Run with node --import tsx. Rust writes scripted tool turns (a queued input,
// then a restarted runtime continuing the session) into a fresh Node session
// database (apps/zcode-cli-rust/tests/node_storage.rs); Node's
// SqliteSessionStore, history hydrator and cold projection read it back and
// must agree with what Rust reads. Spec rust-m11-node-storage §5.2, §11.
import { execFileSync } from "node:child_process";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { isDeepStrictEqual } from "node:util";
import { SqliteSessionStore } from "../apps/zcode-cli/packages/adapters/src/storage/session-store/sqlite-session-store.ts";
import { readSessionModelSelection } from "../apps/zcode-cli/packages/bootstrap/src/app/session-store.ts";
import { hydrateMessageHistoryFromSession } from "../apps/zcode-cli/packages/core/src/agent/session-history-hydrator.ts";
import { createMessageHistory } from "../apps/zcode-cli/packages/core/src/agent/message-history.ts";
import { selectActiveConversationBranch } from "../apps/zcode-cli/packages/contracts/src/rewind/index.ts";
import { ProductProjection } from "../apps/zcode-cli/packages/bootstrap/src/zcode-protocol-v4/product-projection.ts";
import { mergeColdConversationEvents } from "../apps/zcode-cli/packages/bootstrap/src/zcode-protocol-v4/cold-event-merge.ts";
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
  const rust = JSON.parse(await readFile(join(dir, "rust.json"), "utf8"));
  if (!rust.history.length || !rust.rows.length) throw new Error("Rust read nothing back");
  const sessionID = rust.sessionId;
  const store = await SqliteSessionStore.openStartup({ dbPath: join(dir, "cli/db/db.sqlite") });
  try {
    const session = await store.getSession(sessionID);
    if (!session) throw new Error(`Node cannot read session ${sessionID}`);
    const listed = await store.listSessions({ directory: session.directory });
    if (!listed.some((s) => s.id === sessionID)) throw new Error("Session missing from list");
    const selection = await readSessionModelSelection(store, sessionID);
    if (selection?.modelId !== "m") throw new Error("Model selection unreadable");
    const inputs = await store.listSessionInputs({ sessionID });
    if (inputs.length !== 3 || inputs.some((input) => input.status !== "promoted")) {
      throw new Error(`Unexpected input ledger: ${JSON.stringify(inputs)}`);
    }
    const stored = await store.messages({ sessionID });
    const entries = await store.sessionEntries({ sessionID });
    const revert = session.revert;
    const branchOptions = {
      branchCutAfterMessageId: revert?.branchCutAfterMessageID,
      rewindCreatedMessageId: revert?.createdMessageID,
      rewindKeptMessageIds: revert?.keptMessageIDs,
      rewindTargetMessageId: revert?.targetMessageID,
    };
    const history = createMessageHistory();
    await hydrateMessageHistoryFromSession({ history, messages: stored, ...branchOptions });
    expectEqual("history", history.toRuntimeEntries(), rust.history);
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
    expectEqual("rows", snapshot.rows.window, rust.rows);
    const state = Object.fromEntries(
      Object.entries(snapshot).filter(([key]) => !EPHEMERAL_SNAPSHOT_KEYS.has(key)),
    );
    const rustState = Object.fromEntries(
      Object.entries(rust.state).filter(([key]) => !EPHEMERAL_SNAPSHOT_KEYS.has(key)),
    );
    expectEqual("state", state, rustState);
  } finally {
    store.close();
  }
  if (!process.exitCode) console.log(`Node reads the Rust session ${sessionID} identically.`);
} finally {
  await rm(dir, { recursive: true, force: true });
}
