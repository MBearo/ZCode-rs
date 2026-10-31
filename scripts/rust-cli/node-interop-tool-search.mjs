// rust-cli 与 Node CLI 的互操作检查第二十二段：用过本地 ToolSearch 的会话（工作线 F84，docs/specs/rust-cli/tools-more.md
// §3.2.2、§3.6）。
//
// 在第十一段（node-interop-runtime.mjs）的子进程里跑，用它的驱动：本地模型替身、Node 的真实运行时与真实的 `zcode -p`。
// 替身的 provider 是 127.0.0.1 上的 anthropic-messages 端点，不是 z.ai / bigmodel，所以 rust-cli 用本地 ToolSearch：
//
// 1. rust-cli 新会话：ToolSearch 选中 CronList，再调用它。第一个请求的 tools 里有 ToolSearch、没有 CronList、没有
//    `defer_loading` 与占位工具，`anthropic-beta` 没有 `advanced-tool-use`；选中之后的请求里 CronList 是普通定义，
//    ToolSearch 的结果是文字 `Tool loaded: CronList`，后面有 `Tool loaded.`；
// 2. Node 真实回合续写：模型看到的历史里 ToolSearch 的结果是同一句文字（落库的 `output`）；
// 3. rust-cli 再续写：CronList 仍是已加载的普通定义（从 `modelContentLayout` 还原引用），模型直接调用它；
// 4. 两边冷读取相同、消息结构相同，每条消息过 App 的 strict schema。

const ADVANCED_TOOL_USE = "advanced-tool-use-2025-11-20";
const PLACEHOLDER = "DeferredToolPlaceholder";

/** 一个请求的 `anthropic-beta`（原始 header 对里找，名字不分大小写）。 */
function beta(record) {
  const raw = record.rawHeaders ?? [];
  for (let index = 0; index < raw.length; index += 2)
    if (raw[index].toLowerCase() === "anthropic-beta") return raw[index + 1];
  return "";
}

/** 请求里 ToolSearch 的样子：工具名、带 `defer_loading` 的工具、占位工具、beta、延迟清单里有没有 `name`。 */
function wire(record, name) {
  const tools = record.parsed?.tools ?? [];
  return {
    search: tools.some((tool) => tool.name === "ToolSearch"),
    sent: tools.some((tool) => tool.name === name),
    deferred: tools.filter((tool) => tool.defer_loading !== undefined).map((tool) => tool.name),
    placeholder: tools.some((tool) => tool.name === PLACEHOLDER),
    advanced: beta(record).includes(ADVANCED_TOOL_USE),
    listed: JSON.stringify(record.parsed?.messages ?? []).includes(`\\n${name}`),
  };
}

/**
 * 最后一条带工具结果的 user 消息里，`id` 这个调用的结果与同一条消息里的文本块。提醒不算：没有对话中间 system 的模型
 * 把这一步的提醒（`<total_tokens>` 等）接在这条消息后面（context.md §3），与 ToolSearch 的结果无关（工作线 F121）。
 */
function resultOf(record, id) {
  for (const message of [...(record.parsed?.messages ?? [])].reverse()) {
    if (message.role !== "user" || !Array.isArray(message.content)) continue;
    const result = message.content.find(
      (block) => block.type === "tool_result" && block.tool_use_id === id,
    );
    if (!result) continue;
    const texts = message.content
      .filter((block) => block.type === "text" && !block.text.startsWith("<system-reminder>"))
      .map((b) => b.text);
    return { content: result.content, texts };
  }
  return null;
}

/** 这一步里所有的主请求（第十一段的替身按工具清单分类）。 */
const mains = (ctx, from) =>
  ctx.model.records.slice(from).filter((record) => record.kind === "main");

/**
 * 场景：`helpers` 是第十一段的 `Thread`、`newest`、`settle`（同一套历史比较与冷读取比较）。
 */
export async function toolSearchSession(ctx, { Thread, newest, settle }) {
  const { node, rust, check } = ctx;
  const thread = new Thread(ctx, "tool-search");

  let from = ctx.model.records.length;
  const first = "Q1 [[search select:CronList]] [[call CronList|{}]]";
  await rust.turn(null, first);
  const session = await newest(ctx);
  const loading = mains(ctx, from);
  thread.after("rust Q1", first);
  check("tool-search: rust Q1 sends three main requests", loading.length, 3);
  check(
    "tool-search: before loading, ToolSearch is local (no defer_loading, placeholder or beta) and CronList is only listed",
    loading[0] && wire(loading[0], "CronList"),
    {
      search: true,
      sent: false,
      deferred: [],
      placeholder: false,
      advanced: false,
      listed: true,
    },
  );
  const searchCall = loading[0]?.reply?.content?.find((block) => block.type === "tool_use");
  check(
    "tool-search: after loading, CronList is a plain definition",
    loading[1] && { ...wire(loading[1], "CronList"), listed: undefined },
    {
      search: true,
      sent: true,
      deferred: [],
      placeholder: false,
      advanced: false,
      listed: undefined,
    },
  );
  check(
    "tool-search: the ToolSearch result is text, followed by Tool loaded.",
    loading[1] && searchCall && resultOf(loading[1], searchCall.id),
    {
      content: [{ type: "text", text: "Tool loaded: CronList" }],
      texts: ["Tool loaded."],
    },
  );

  // Node 续写：历史里 ToolSearch 的结果是落库的同一句文字（第十一段的历史比较逐条核对）。
  await node.turn(session, "Q2 next");
  thread.after("node Q2", "Q2 next");

  // rust-cli 再续写：CronList 仍是已加载的普通定义，模型直接调用它（不再 ToolSearch）。
  from = ctx.model.records.length;
  const third = "Q3 [[call CronList|{}]]";
  await rust.turn(session, third);
  const resumed = mains(ctx, from);
  thread.after("rust Q3", third);
  check(
    "tool-search: after Node's turn, rust-cli still sends CronList as a plain definition",
    resumed[0] && { ...wire(resumed[0], "CronList"), listed: undefined },
    {
      search: true,
      sent: true,
      deferred: [],
      placeholder: false,
      advanced: false,
      listed: undefined,
    },
  );
  check(
    "tool-search: the model calls CronList directly",
    resumed[0]?.reply?.content?.find((block) => block.type === "tool_use")?.name,
    "CronList",
  );
  await settle(ctx, "tool-search", [session], { shape: true });
}
