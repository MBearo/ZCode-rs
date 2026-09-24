import { createSqliteSessionStore } from "../../../apps/zcode-cli/packages/adapters/src/storage/session-store.js";
import type { MessageId, PartId, ProjectId, SessionId } from "@zcode/contracts";
import type { fixture } from "./zcode-cli-rust-fixture.js";

/**
 * 用 Node 仓储在共享库里写入 `count` 个本地工作区的冷会话（`<prefix>-<i>`），每个会话一条
 * user 消息与正文，供启动、索引与驻留用例构造大量未打开的历史。
 */
export async function seedColdSessions(
  f: Awaited<ReturnType<typeof fixture>>,
  count: number,
  prefix = "cold",
) {
  const store = createSqliteSessionStore({ dbPath: f.db });
  const ids: string[] = [];
  for (let i = 0; i < count; i++) {
    const id = `${prefix}-${i}` as SessionId;
    const messageID = `msg_${prefix}_${i}` as MessageId;
    await store.createSession({
      id,
      projectID: "fixture" as ProjectId,
      directory: f.cwd,
      slug: id,
      title: id,
      version: "fixture",
    });
    await store.saveMessage({
      id: messageID,
      sessionID: id,
      role: "user",
      time: { created: 1 },
      agent: "main",
    });
    await store.savePart({
      id: `prt_${prefix}_${i}` as PartId,
      sessionID: id,
      messageID,
      type: "text",
      text: `history ${i}`,
    });
    ids.push(id);
  }
  store.close();
  return ids;
}

/**
 * 界面行不落库（spec rust-m11-node-storage §2.4）：冷加载与重启后按 Node 冷投影重建，
 * 行与实体 id 属于新 epoch。跨冷热比对用内容形状。
 */
export function rowShape(row: Record<string, any>) {
  return {
    kind: row.kind,
    text: row.text,
    state: row.state,
    status: row.status,
    toolName: row.toolName,
    marker: row.marker?.type,
  };
}
