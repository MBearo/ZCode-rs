// Rust's cold reading of every session of a Node session database compared with Node's
// own readers (history hydrator, cold projection). Spec rust-m11-node-storage §6, §11.
// Only counts and JSON paths are returned, never session content.
import { execFileSync } from "node:child_process";
import { mkdtemp, readFile, readdir, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { isDeepStrictEqual } from "node:util";
import { SqliteSessionStore } from "../apps/zcode-cli/packages/adapters/src/storage/session-store/sqlite-session-store.ts";
import { createNodeToolArtifactStore } from "../apps/zcode-cli/packages/adapters/src/storage/index.ts";
import { hydrateMessageHistoryFromSession } from "../apps/zcode-cli/packages/core/src/agent/session-history-hydrator.ts";
import { createMessageHistory } from "../apps/zcode-cli/packages/core/src/agent/message-history.ts";
import { selectActiveConversationBranch } from "../apps/zcode-cli/packages/contracts/src/rewind/index.ts";
import { ProductProjection } from "../apps/zcode-cli/packages/bootstrap/src/zcode-protocol-v4/product-projection.ts";
import { mergeColdConversationEvents } from "../apps/zcode-cli/packages/bootstrap/src/zcode-protocol-v4/cold-event-merge.ts";
import { goalVerificationEntriesFromSessionEntries } from "../apps/zcode-cli/packages/bootstrap/src/zcode-protocol-v4/transcript-hydration.ts";

const EPHEMERAL = new Set(["protocolVersion", "sessionId", "logEpoch", "seq", "revision", "rows"]);

/** The first JSON path where `a` and `b` differ (values are never printed). */
export function diffPath(a, b, path = "$") {
  if (isDeepStrictEqual(a, b)) return null;
  if (typeof a !== "object" || typeof b !== "object" || a === null || b === null) return path;
  if (Array.isArray(a) !== Array.isArray(b)) return path;
  if (Array.isArray(a)) {
    if (a.length !== b.length) return `${path}.length`;
    for (let i = 0; i < a.length; i++) {
      const found = diffPath(a[i], b[i], `${path}[${i}]`);
      if (found) return found;
    }
    return path;
  }
  for (const key of new Set([...Object.keys(a), ...Object.keys(b)])) {
    const found = diffPath(a[key], b[key], `${path}.${key}`);
    if (found) return found;
  }
  return path;
}

/**
 * Runs the Rust reader (`examples/node_read`) over `db`, then compares each session with
 * Node's reading of the same database. `db` is read and may be settled like a resume
 * (admitted inputs discarded), so pass a copy.
 */
export async function compareDatabase(db, artifacts, reader) {
  const dumps = await mkdtemp(join(tmpdir(), "zcode-node-compare-"));
  const cache = await mkdtemp(join(tmpdir(), "zcode-node-cache-"));
  try {
    execFileSync(reader, [db, artifacts, dumps], { stdio: ["ignore", "ignore", "inherit"] });
    const rustSummary = JSON.parse(await readFile(join(dumps, "summary.json"), "utf8"));
    const store = await SqliteSessionStore.openStartup({ dbPath: db });
    const artifactStore = createNodeToolArtifactStore({
      imageCacheRootDir: join(cache, "image"),
      pdfCacheRootDir: join(cache, "pdf"),
      rootDir: artifacts,
      videoCacheRootDir: join(cache, "video"),
    });
    const compare = async (rust) => {
      const sessionID = rust.sessionId;
      const session = await store.getSession(sessionID);
      const stored = await store.messages({ sessionID });
      const revert = session.revert;
      const branch = {
        branchCutAfterMessageId: revert?.branchCutAfterMessageID,
        rewindCreatedMessageId: revert?.createdMessageID,
        rewindKeptMessageIds: revert?.keptMessageIDs,
        rewindTargetMessageId: revert?.targetMessageID,
      };
      const history = createMessageHistory();
      await hydrateMessageHistoryFromSession({
        artifactStore,
        history,
        messages: stored,
        ...branch,
      });
      const merged = mergeColdConversationEvents({
        memoryEvents: [],
        messages: selectActiveConversationBranch(stored, branch),
        sessionId: sessionID,
        goalVerificationEntries: goalVerificationEntriesFromSessionEntries(
          await store.sessionEntries({ sessionID }),
        ),
        target: await store.readTarget({ sessionID }),
      });
      const projection = new ProductProjection(sessionID, "epoch");
      projection.beginHydrationReplay();
      for (const event of merged.events) projection.applyHydrationEvent(event);
      projection.completeHydrationReplay();
      const snapshot = projection.getSnapshot();
      const strip = (state) =>
        Object.fromEntries(Object.entries(state).filter(([key]) => !EPHEMERAL.has(key)));
      const plain = (value) => JSON.parse(JSON.stringify(value));
      return {
        history: diffPath(plain(history.toRuntimeEntries()), rust.history),
        rows: diffPath(plain(snapshot.rows.window), rust.rows),
        state: diffPath(plain(strip(snapshot)), strip(rust.state)),
      };
    };
    const mismatches = { history: {}, rows: {}, state: {} };
    let equal = 0;
    let nodeFailed = 0;
    for (const file of await readdir(dumps)) {
      if (file === "summary.json") continue;
      const rust = JSON.parse(await readFile(join(dumps, file), "utf8"));
      let result;
      try {
        result = await compare(rust);
      } catch {
        nodeFailed++;
        continue;
      }
      if (!result.history && !result.rows && !result.state) equal++;
      for (const kind of ["history", "rows", "state"]) {
        if (!result[kind]) continue;
        // 按去掉下标的路径归类，便于定位规则差异。
        const category = result[kind].replace(/\[\d+\]/g, "[]");
        mismatches[kind][category] = (mismatches[kind][category] ?? 0) + 1;
      }
    }
    store.close();
    return {
      sessions: rustSummary.sessions,
      read: rustSummary.read,
      rustFailed: rustSummary.failed.length,
      coldReadMs: rustSummary.coldReadMs,
      equal,
      nodeFailed,
      mismatches,
    };
  } finally {
    await rm(dumps, { recursive: true, force: true });
    await rm(cache, { recursive: true, force: true });
  }
}
