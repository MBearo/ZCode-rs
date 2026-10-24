// 第十一段的场景（docs/specs/rust-cli/node-store.md §6 第 11 步）：两边轮流在同一个会话上跑真实回合与会话命令。
//
// 每一步之后检查这一步的模型请求：第一条请求里的对话 = 到目前为止应有的历史 + 这一步的输入；同一步里后面的请求
// 是前一条加上它的回复（工具轮）。回退、压缩、分支之后，应有的历史按 Node / rust-cli 的语义改写。每个场景结束时
// 再比两边的冷读取（Node history hydrator 与 rust-cli 的 `NodeStore::load`）与 App 协议 schema。
import { readFile } from "node:fs/promises";
import { existsSync } from "node:fs";
import { join } from "node:path";
import { isDeepStrictEqual } from "node:util";
import { appSchemaRejections, nodeShape, rustShape, rustText } from "./node-interop-history.mjs";
import { conversation, replyItems } from "./node-interop-runtime-harness.mjs";
import { nodeKilled } from "./node-interop-node-killed.mjs";
import { toolSearchSession } from "./node-interop-tool-search.mjs";

/** 一条会话线（含分支出的会话）上模型应当看到的历史。 */
class Thread {
  constructor(ctx, name) {
    this.ctx = ctx;
    this.name = name;
    this.expected = [];
    this.cursor = ctx.model.records.length;
  }

  /** 新请求里属于主线程的那些（替身按工具清单与压缩提示分类）。 */
  take(kind = "main") {
    const records = this.ctx.model.records.slice(this.cursor);
    this.cursor = this.ctx.model.records.length;
    return records.filter((record) => record.kind === kind);
  }

  /** 一步回合之后：核对这一步的每个主请求，更新应有的历史。 */
  after(label, prompt) {
    const { check } = this.ctx;
    const records = this.take();
    const name = `${this.name} ${label}`;
    if (!records.length) {
      check(`${name}: the turn sent a main request`, 0, 1);
      return;
    }
    const first = conversation(records[0].parsed);
    check(`${name}: the model sees the history so far plus the new input`, first, [
      ...this.expected,
      { role: "user", text: prompt },
    ]);
    let previous = { items: first, reply: records[0].reply };
    for (const record of records.slice(1)) {
      const items = conversation(record.parsed);
      const prefix = [...previous.items, ...replyItems(previous.reply)];
      check(
        `${name}: request #${record.seq} continues the step`,
        items.slice(0, prefix.length),
        prefix,
      );
      previous = { items, reply: record.reply };
    }
    this.expected = [...previous.items, ...replyItems(previous.reply)];
  }

  /** 回退到 `prompt` 这条输入之前（对话回退的语义：这条输入与之后的都不再发给模型）。 */
  revertBefore(prompt) {
    const index = this.expected.findIndex((item) => item.role === "user" && item.text === prompt);
    this.ctx.check(`${this.name}: the revert target is in the history`, index >= 0, true);
    if (index >= 0) this.expected = this.expected.slice(0, index);
  }

  /**
   * 压缩之后的第一步：摘要在前、压缩前的输入都不在；之后以这一请求里的历史为准（另一边续写时逐条比）。
   * `dropped` 是压缩前的输入，`summary` 是这次摘要请求的回复里的标记。
   */
  afterCompaction(label, prompt, { summary, dropped }) {
    const { check } = this.ctx;
    const records = this.take();
    const name = `${this.name} ${label}`;
    const items = records.length ? conversation(records[0].parsed) : [];
    const head = items.slice(0, -1);
    check(`${name}: the new input is last`, items.at(-1), { role: "user", text: prompt });
    check(
      `${name}: the history starts from the summary`,
      head.some((item) => item.role === "user" && item.text?.includes(summary)),
      true,
    );
    check(
      `${name}: inputs before the compaction are gone`,
      dropped.filter((text) => head.some((item) => item.text?.includes(text))),
      [],
    );
    let previous = { items, reply: records[0]?.reply };
    for (const record of records.slice(1)) {
      const next = conversation(record.parsed);
      const prefix = [...previous.items, ...replyItems(previous.reply)];
      check(
        `${name}: request #${record.seq} continues the step`,
        next.slice(0, prefix.length),
        prefix,
      );
      previous = { items: next, reply: record.reply };
    }
    this.expected = [...previous.items, ...replyItems(previous.reply)];
  }

  /** 这一步发出的压缩摘要请求的标记（`SUMMARY-<序号>`）。 */
  summary(label) {
    const records = this.take("compact");
    this.ctx.check(`${this.name} ${label}: one summary request`, records.length, 1);
    return `SUMMARY-${records.at(-1)?.seq}`;
  }
}

/** 最新建的顶层会话（刚跑完的新会话）。 */
async function newest(ctx) {
  const sessions = await ctx.store.listSessions({ directory: ctx.layout.work, roots: true });
  return sessions.sort((a, b) => b.time.created - a.time.created)[0]?.id;
}

/** 文本含 `marker` 的真实用户输入的消息 ID。 */
async function userMessage(ctx, sessionID, marker) {
  const messages = await ctx.store.messages({ sessionID });
  const found = messages.find(
    (message) =>
      message.info.role === "user" &&
      message.info.semantics?.origin === "real_user" &&
      message.parts.some((part) => part.type === "text" && part.text.includes(marker)),
  );
  if (!found) throw new Error(`no user message ${marker} in ${sessionID}`);
  return found.info.id;
}

/** Node 自己的运行时提醒：合成的 user 消息，part 都没有 rust-cli 的元数据。 */
const nodeReminder = ({ info, parts }) =>
  info.role === "user" && info.synthetic === true && !parts.some((part) => part.metadata?.rustCli);

/** rust-cli 冷读取里的分叉提示（`/fork` 的新会话，session-lifecycle.md §6.5.2）。 */
const isForkNotice = (message) => message.type === "attachment" && message.source === "fork";

/**
 * 场景结束：两边冷读取相同、App schema 接受每条消息；`shape` 为 true 时再比消息结构（没有回退与压缩的会话）。
 * 分叉提示在 Node 的 hydrator 里是 attachment 条目、不在归一的消息里，比历史时 rust-cli 一侧同样去掉它（第七段
 * 另比提示的正文）。
 */
async function settle(ctx, name, sessionIDs, { shape = false } = {}) {
  const { check, store, artifactStore, nodeHistory, rust, nodeRoot } = ctx;
  for (const sessionID of sessionIDs) {
    const session = await store.getSession(sessionID);
    const loaded = await rust.read(sessionID);
    check(
      `${name} ${sessionID}: cold history (Node hydrator, rust-cli load)`,
      await nodeHistory(store, artifactStore, session, sessionID),
      rustText(loaded.messages.filter((message) => !isForkNotice(message))),
    );
    // Node 自己写的运行时提醒（合成消息、part 没有 `metadata.rustCli`，例如 Node 的 `agent_listing_delta`）
    // rust-cli 冷读取跳过（node-store.md §5 第 4 步、§7），结构比较也不比它们。
    if (shape)
      check(
        `${name} ${sessionID}: message structure`,
        nodeShape(
          (await store.messages({ sessionID })).filter((message) => !nodeReminder(message)),
        ),
        rustShape(loaded.messages),
      );
  }
  check(
    `${name}: every message passes the App protocol schema`,
    await appSchemaRejections(nodeRoot, store, sessionIDs),
    [],
  );
}

/** 1. Node 真实回合续写 rust-cli 的会话（文本与工具），rust-cli 再续写，再回到 Node；另一条线由 Node 先建。 */
async function continuations(ctx) {
  const { node, rust, layout } = ctx;
  const notes = join(layout.work, "notes.txt");
  const thread = new Thread(ctx, "continue");
  await rust.turn(null, "T1 hello");
  const session = await newest(ctx);
  thread.after("rust T1", "T1 hello");
  for (const [side, prompt] of [
    ["node", `T2 [[read ${notes}]]`],
    ["rust", `T3 [[read ${notes}]]`],
    ["node", "T4 thanks"],
    ["rust", "T5 bye"],
  ]) {
    await ctx[side].turn(session, prompt);
    thread.after(`${side} ${prompt.slice(0, 2)}`, prompt);
  }
  const nodeFirst = new Thread(ctx, "node-first");
  const created = await node.turn(null, `U1 [[read ${notes}]]`);
  nodeFirst.after("node U1", `U1 [[read ${notes}]]`);
  await rust.turn(created.session, "U2 next");
  nodeFirst.after("rust U2", "U2 next");
  await node.turn(created.session, "U3 last");
  nodeFirst.after("node U3", "U3 last");
  await settle(ctx, "continue", [session, created.session], { shape: true });
}

/** 2. Node 回退后 rust-cli 续写；rust-cli `/rewind` 后 Node 续写。 */
async function reverts(ctx) {
  const { node, rust } = ctx;
  const thread = new Thread(ctx, "revert");
  await rust.turn(null, "R1 one");
  const session = await newest(ctx);
  thread.after("rust R1", "R1 one");
  for (const [side, prompt] of [
    ["node", "R2 two"],
    ["rust", "R3 three"],
    ["node", "R4 four"],
  ]) {
    await ctx[side].turn(session, prompt);
    thread.after(`${side} ${prompt.slice(0, 2)}`, prompt);
  }
  const reverted = await node.revert(session, await userMessage(ctx, session, "R3 three"));
  ctx.check("revert: Node rewinds the conversation", reverted.strategy, "active_chain");
  thread.revertBefore("R3 three");
  await rust.turn(session, "R5 five");
  thread.after("rust R5 after the Node revert", "R5 five");
  const rewound = await rust.rewind(
    session,
    await userMessage(ctx, session, "R2 two"),
    "conversation",
  );
  ctx.check("revert: rust-cli /rewind is accepted", Object.keys(rewound), ["accepted"]);
  thread.revertBefore("R2 two");
  await node.turn(session, "R6 six");
  thread.after("node R6 after the rust-cli rewind", "R6 six");
  await rust.turn(session, "R7 seven");
  thread.after("rust R7", "R7 seven");
  await settle(ctx, "revert", [session]);
}

/** 3. `workspace_file_rewind` 混写：checkpoint 与文件回退在两边交替，回退结果与改动摘要两边一致。 */
async function fileRewinds(ctx) {
  const { node, rust, layout, check } = ctx;
  const a = join(layout.work, "f12-a.txt");
  const b = join(layout.work, "f12-b.txt");
  const content = async (path) => (existsSync(path) ? readFile(path, "utf8") : null);
  const thread = new Thread(ctx, "files");
  const step = async (side, session, prompt) => {
    const created = await ctx[side].turn(session, prompt);
    thread.after(`${side} ${prompt.slice(0, 2)}`, prompt);
    return created;
  };
  await step("rust", null, `F1 [[write ${a}|one\\n]]`);
  const session = await newest(ctx);
  await step("node", session, `F2 [[read ${a}]] [[write ${a}|two\\n]]`);
  check("files: both writes landed", await content(a), "two\n");
  const f2 = await userMessage(ctx, session, "F2 ");
  const nodeRewind = await node.fileRewind(session, [f2]);
  check(
    "files: Node rewinds its own turn",
    [nodeRewind.applied, await content(a)],
    [true, "one\n"],
  );
  await step("rust", session, `F3 [[write ${b}|three\\n]]`);
  const f3 = await userMessage(ctx, session, "F3 ");
  const rustRewind = await rust.rewind(session, f3, "code");
  check(
    "files: rust-cli rewinds its own turn (code)",
    [Object.keys(rustRewind), await content(b)],
    [["accepted"], null],
  );
  await step("node", session, `F4 [[read ${a}]] [[write ${a}|four\\n]]`);
  // 改动摘要：Node 的冷读取（App 的改动摘要）给每个改过文件的回合；rust-cli 的 rewind 选择器给还能恢复代码的回合
  // （两边写的 checkpoint，修复线 F12 起含 Node 写的）。没有撤销过的回合（F1 rust-cli、F4 Node）两边的文件与增删
  // 行数相同；撤销过的回合（F2 被 Node、F3 被 rust-cli）rust-cli 不再给出它们的改动（选择器的 `code` 指“从这一回合
  // 起有可恢复的 checkpoint”，后面还有 F4，所以仍是 true）。
  const options = await rust.options(session);
  const summaries = await ctx.nodeChangeSummaries(session);
  const prompts = await Promise.all(
    ["F1 ", "F2 ", "F3 ", "F4 "].map((marker) => userMessage(ctx, session, marker)),
  );
  const option = (id) => options.find((entry) => entry.message === id);
  const rustFiles = (id) =>
    option(id)?.files?.map((file) => [file.path, file.additions, file.deletions]) ?? null;
  check(
    "files: change summary of the turns not rewound (Node cold summaries, rust-cli rewind options)",
    [prompts[0], prompts[3]].map((id) => summaries.get(id) ?? null),
    [prompts[0], prompts[3]].map(rustFiles),
  );
  check(
    "files: Node keeps the summary of the rewound turns",
    [prompts[1], prompts[2]].map((id) => summaries.has(id)),
    [true, true],
  );
  check(
    "files: rust-cli lists no changes for the rewound turns",
    [prompts[1], prompts[2]].map(rustFiles),
    [null, null],
  );
  // rust-cli 回退 F1 起的代码：连带 Node 写的 F4（Node 的 checkpoint），a.txt 回到 F1 之前（不存在）。以前 rust-cli
  // 不认 Node 的 checkpoint，a.txt 被 Node 改过，整体报冲突。
  const cascade = await rust.rewind(session, prompts[0], "code");
  check(
    "files: rust-cli rewinds the code from F1 through the Node turn",
    [Object.keys(cascade), await content(a)],
    [["accepted"], null],
  );
  const after = await ctx.nodeFileRewinds(session);
  check(
    "files: Node reads the rust-cli file rewinds of F1 and F4",
    [prompts[0], prompts[3]].every((id) => after.includes(id)),
    true,
  );
  await step("node", session, `F5 [[read ${a}]]`);
  await settle(ctx, "files", [session]);
}

/** 4. rust-cli `Fork{at: None}`（`--fork-session` 的命令；F75 之前是 `/branch`）分出的会话由 Node 续写；原会话不受影响。 */
async function branches(ctx) {
  const { node, rust } = ctx;
  const thread = new Thread(ctx, "branch");
  await rust.turn(null, "B1 one");
  const parent = await newest(ctx);
  thread.after("rust B1", "B1 one");
  await node.turn(parent, "B2 two");
  thread.after("node B2", "B2 two");
  const branched = await rust.branch(parent);
  const child = branched.session;
  ctx.check(
    "branch: rust-cli Fork makes a new session",
    typeof child === "string" && child !== parent,
    true,
  );
  const before = thread.expected;
  await node.turn(child, "B3 three");
  thread.after("node B3 in the branch", "B3 three");
  await rust.turn(child, "B4 four");
  thread.after("rust B4 in the branch", "B4 four");
  const original = new Thread(ctx, "branch-parent");
  original.expected = before;
  await node.turn(parent, "B5 five");
  original.after("node B5 in the original", "B5 five");
  await settle(ctx, "branch", [parent, child]);
}

/**
 * 5. rust-cli `/fork`（F75）：三轮之后从第二轮分叉，分叉里模型看到前两轮、看不到第三轮；Node 与 rust-cli 在分叉里
 * 续写；Node 续写原会话，第三轮还在、分叉里的不在。
 */
async function forks(ctx) {
  const { node, rust, check } = ctx;
  const thread = new Thread(ctx, "fork");
  await rust.turn(null, "K1 one");
  const parent = await newest(ctx);
  thread.after("rust K1", "K1 one");
  await node.turn(parent, "K2 two");
  thread.after("node K2", "K2 two");
  const throughTwo = thread.expected;
  await rust.turn(parent, "K3 three");
  thread.after("rust K3", "K3 three");
  const forked = await rust.fork(parent, await userMessage(ctx, parent, "K2 two"));
  const child = forked.session;
  check(
    "fork: rust-cli /fork makes a new session from the chosen input",
    [
      typeof child === "string" && child !== parent,
      forked.response?.startsWith(`Forked session ${child} from message `),
    ],
    [true, true],
  );
  const branch = new Thread(ctx, "fork-child");
  branch.expected = throughTwo;
  await node.turn(child, "K4 four");
  branch.after("node K4 in the fork", "K4 four");
  await rust.turn(child, "K5 five");
  branch.after("rust K5 in the fork", "K5 five");
  const original = new Thread(ctx, "fork-parent");
  original.expected = thread.expected;
  await node.turn(parent, "K6 six");
  original.after("node K6 in the original", "K6 six");
  const notices = async (sessionID) =>
    (await rust.read(sessionID)).messages.filter(isForkNotice).length;
  check(
    "fork: one fork notice in the fork, none in the original",
    [await notices(child), await notices(parent)],
    [1, 0],
  );
  await settle(ctx, "fork", [parent, child]);
}

/** 6. 压缩后 Node 续写，再由 Node 压缩、rust-cli 续写，rust-cli 再压缩、Node 续写。 */
async function compactions(ctx) {
  const { node, rust, layout } = ctx;
  const notes = join(layout.work, "notes.txt");
  const thread = new Thread(ctx, "compact");
  await rust.turn(null, "C1 one");
  const session = await newest(ctx);
  thread.after("rust C1", "C1 one");
  await node.turn(session, `C2 [[read ${notes}]]`);
  thread.after("node C2", `C2 [[read ${notes}]]`);
  await rust.turn(session, "C3 three");
  thread.after("rust C3", "C3 three");
  await rust.compact(session);
  let summary = thread.summary("rust /compact");
  await node.turn(session, "C4 four");
  thread.afterCompaction("node C4 after the rust-cli compaction", "C4 four", {
    summary,
    dropped: ["C1 one", "C2 ", "C3 three"],
  });
  await node.turn(session, "/compact");
  summary = thread.summary("node /compact");
  await rust.turn(session, "C5 five");
  thread.afterCompaction("rust C5 after the Node compaction", "C5 five", {
    summary,
    dropped: ["C4 four"],
  });
  await rust.compact(session);
  summary = thread.summary("rust /compact again");
  await node.turn(session, "C6 six");
  thread.afterCompaction("node C6 after the second rust-cli compaction", "C6 six", {
    summary,
    dropped: ["C5 five"],
  });
  await rust.turn(session, "C7 seven");
  thread.after("rust C7", "C7 seven");
  await settle(ctx, "compact", [session]);
}

// `nodeKilled` 是第二十一段（Node 写的被杀会话，工作线 F86、F88，node-interop-node-killed.mjs）：要这里的模型替身与临时
// HOME。

/** 8. 第二十二段（工作线 F84，node-interop-tool-search.mjs）：用过本地 ToolSearch 的会话由 Node 续写、rust-cli 再续写。 */
const toolSearch = (ctx) => toolSearchSession(ctx, { Thread, newest, settle });

export const SCENARIOS = {
  continuations,
  reverts,
  fileRewinds,
  branches,
  forks,
  compactions,
  nodeKilled,
  toolSearch,
};

/** 跑全部场景（或 `only` 里的），返回每个场景的耗时（毫秒）。 */
export async function runScenarios(ctx, only = []) {
  const timings = {};
  for (const [name, scenario] of Object.entries(SCENARIOS)) {
    if (only.length && !only.includes(name)) continue;
    const started = Date.now();
    try {
      await scenario(ctx);
    } catch (error) {
      ctx.check(`${name}: the scenario ran to the end`, String(error?.stack ?? error), null);
    }
    timings[name] = Date.now() - started;
  }
  return timings;
}

export const same = isDeepStrictEqual;
