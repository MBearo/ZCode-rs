// Run through generate-zcode-cli-rust-fixtures.mjs. Executes Node's session
// store repositories on a fixed operation list (fixed clock and UUIDs) and
// records every table afterwards; the Rust node repositories replay the same
// operations and must produce byte-identical rows. Spec rust-m11-node-storage.
import crypto from "node:crypto";
import { syncBuiltinESMExports } from "node:module";
import { DatabaseSync } from "node:sqlite";
import { readFile, writeFile } from "node:fs/promises";
import { runSqliteSessionMigrations } from "../apps/zcode-cli/packages/adapters/src/storage/session-store/migration-runner.ts";
import { SQLITE_MIGRATIONS } from "../apps/zcode-cli/packages/adapters/src/storage/session-store/migrations.ts";
import * as sessions from "../apps/zcode-cli/packages/adapters/src/storage/session-store/repositories/sessions.ts";
import * as messages from "../apps/zcode-cli/packages/adapters/src/storage/session-store/repositories/messages.ts";
import * as entries from "../apps/zcode-cli/packages/adapters/src/storage/session-store/repositories/session-entries.ts";
import * as inputs from "../apps/zcode-cli/packages/adapters/src/storage/session-store/repositories/session-inputs.ts";
import * as todos from "../apps/zcode-cli/packages/adapters/src/storage/session-store/repositories/todos.ts";
import * as settings from "../apps/zcode-cli/packages/adapters/src/storage/session-store/repositories/local-settings.ts";
import * as history from "../apps/zcode-cli/packages/adapters/src/storage/session-store/repositories/input-history.ts";
import * as targets from "../apps/zcode-cli/packages/adapters/src/storage/session-target.ts";
import { MESSAGE_OPERATIONS } from "./zcode-cli-rust-node-db-message-ops.mjs";
import { LEDGER_OPERATIONS } from "./zcode-cli-rust-node-db-ledger-ops.mjs";

const MIGRATION_DIR = "../apps/zcode-cli-rust/crates/state/src/node/migrations/";
const TABLES = [
  "session",
  "message",
  "part",
  "session_entry",
  "session_input",
  "todo",
  "session_target",
  "local_setting",
  "input_history",
];

export const NODE_DB_OPERATIONS = [...MESSAGE_OPERATIONS, ...LEDGER_OPERATIONS];

let clock = 0;
let uuids = [];
const realUuid = crypto.randomUUID;
function installClock() {
  Date.now = () => clock;
  const next = () => uuids.shift() ?? realUuid();
  crypto.randomUUID = next;
  Object.defineProperty(globalThis.crypto, "randomUUID", { value: next, configurable: true });
  syncBuiltinESMExports();
}

async function apply(db, o) {
  if (o.now !== undefined) clock = o.now;
  if (o.uuid) uuids = [o.uuid];
  switch (o.op) {
    case "exec":
      return db.exec(o.sql);
    case "createSession":
      return sessions.createSession(db, o.input);
    case "updateSession":
      return sessions.updateSession(db, o.input);
    case "setRevert":
      return sessions.setRevert(db, { sessionID: o.sessionID, revert: o.revert });
    case "saveMessage":
      return messages.saveMessage(db, o.info, o.copyFrom);
    case "savePart":
      return messages.savePart(db, o.part, o.copyFrom);
    case "removeMessage":
      return messages.removeMessage(db, { sessionID: o.sessionID, messageID: o.messageID });
    case "saveEntry":
      return entries.saveSessionEntry(db, o.entry);
    case "saveInput":
      return inputs.saveSessionInput(db, o.input);
    case "updateInputs":
      return inputs.updateSessionInputs(db, { sessionID: o.sessionID, updates: o.updates });
    case "promoteInput":
      return inputs.promoteSessionInput(db, o);
    case "settleInput":
      return inputs.settleSessionInput(db, o);
    case "markPromoted":
      return inputs.markSessionInputPromoted(db, { ...o, promotedMessageID: o.messageID });
    case "updateTodos":
      return todos.updateTodos(db, o);
    case "setTarget":
      return targets.setSessionTarget(db, o);
    case "startTargetRun": {
      const target = targets.readSessionTarget(db, o);
      return targets.startSessionTargetRun(db, { ...o, targetID: target.targetID });
    }
    case "heartbeatTargetRun": {
      const target = targets.readSessionTarget(db, o);
      return targets.heartbeatSessionTargetRun(db, { ...o, targetID: target.targetID });
    }
    case "finishTargetRun": {
      const target = targets.readSessionTarget(db, o);
      return targets.finishSessionTargetRun(db, { ...o, targetID: target.targetID });
    }
    case "recoverTargetRun":
      return targets.recoverInterruptedSessionTargetRun(db, o);
    case "updateTargetStatus":
      return targets.updateSessionTargetStatus(db, o);
    case "updateTargetSummaryTitle": {
      const target = targets.readSessionTarget(db, o);
      return targets.updateSessionTargetSummaryTitle(db, { ...o, targetID: target.targetID });
    }
    case "accountTargetUsage": {
      const target = targets.readSessionTarget(db, o);
      return targets.accountSessionTargetUsage(db, { ...o, targetID: target.targetID });
    }
    case "savePermissionMode":
      return settings.saveProjectPermissionMode(db, o);
    case "savePermission":
      return settings.saveProjectPermission(db, o);
    case "recordInputHistory":
      return history.recordInputHistory(db, o.input);
    default:
      throw new Error(`unknown op ${o.op}`);
  }
}

export async function nodeDbFixtures() {
  const realNow = Date.now;
  installClock();
  try {
    const db = new DatabaseSync(":memory:");
    runSqliteSessionMigrations(db, ":memory:");
    db.exec("pragma foreign_keys = on");
    for (const o of NODE_DB_OPERATIONS) await apply(db, o);
    const tables = Object.fromEntries(
      TABLES.map((table) => [
        table,
        db
          .prepare(`select * from ${table} order by rowid`)
          .all()
          .map((row) => Object.values(row)),
      ]),
    );
    const read = {
      messages: await messages.messages(db, { sessionID: "sess_a" }),
      entries: entries.sessionEntries(db, { sessionID: "sess_a" }),
      inputs: await inputs.listSessionInputs(db, { sessionID: "sess_a" }),
      list: (
        await sessions.listSessions(db, {
          directory: "/w",
          taskTypes: ["interactive", "subagent_child"],
        })
      ).map((s) => s.id),
    };
    return { operations: NODE_DB_OPERATIONS, tables, read };
  } finally {
    Date.now = realNow;
    crypto.randomUUID = realUuid;
    delete globalThis.crypto.randomUUID;
    syncBuiltinESMExports();
  }
}

/** Keeps the embedded Rust migration SQL byte-identical to Node's. */
export async function syncNodeMigrations(check) {
  for (const migration of SQLITE_MIGRATIONS) {
    const path = new URL(`${MIGRATION_DIR}${migration.id}.sql`, import.meta.url);
    if (check) {
      if ((await readFile(path, "utf8")) !== migration.sql)
        throw new Error(`${migration.id}.sql differs from TS; regenerate it`);
    } else await writeFile(path, migration.sql);
  }
}
