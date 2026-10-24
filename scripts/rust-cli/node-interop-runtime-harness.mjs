// 第十一段（node-interop-runtime.mjs）的驱动：本地模型替身、Node 的真实运行时、rust-cli 的真实进程，以及模型请求里
// 历史的归一（docs/specs/rust-cli/node-store.md §6 第 11 步）。只在临时 HOME 的子进程里运行。
//
// 入口的选择：Node 一侧直接调 bootstrap 的 `createZCodeApp`（Node 自己的 compact E2E 也这样驱动真实回合）：它是 CLI 与
// App 共用的组装，回合走 provider 适配器、runtime、工具执行与会话存储，比 `zcode -p` 少的只有参数解析；模型目录用
// Node 的 `createNodeProviderRegistryRuntime` 读同一份 `~/.zcode/v2/provider_config.json`（两边读同一个文件，与真实
// 共用一致），没有凭据与网络。rust-cli 一侧回合用真实的 `zcode -p`（`-r` 续写），`-p` 没有的 `/compact`、`/rewind`、
// `/fork` 与 `--fork-session` 的 `Fork` 用 `node_kernel` 例子程序发同样的内核命令。
import { spawn } from "node:child_process";
import { mkdir, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { importNode } from "./node-root.mjs";
import { startModelFixture, text, toolUse } from "./fixture/model.mjs";

/** 压缩摘要提示的开头：rust-cli 与 Node 的提示同一句（compact.md §3）。 */
const COMPACT_PROMPT = "CRITICAL: Respond with TEXT ONLY. Do NOT call any tools.";
export const PROVIDER = "personal:fake";
export const MODEL = "claude-opus-5";

const blocksOf = (content) =>
  typeof content === "string" ? [{ type: "text", text: content }] : (content ?? []);

/** 去掉 `<system-reminder>` 段（两边各自宣布环境、日期、清单；rust-cli 还会把提醒并进工具结果，context.md §3）。 */
export const stripReminders = (value) =>
  value.replace(/<system-reminder>[\s\S]*?<\/system-reminder>/g, "").trim();

/**
 * rust-cli 发给模型时在 ToolSearch 结果之后加的说明（tools-more.md §3.6）：只在发出的请求里、不落库，Node 不加，比较
 * 历史时不算（第二十二段，工作线 F84）。
 */
const TOOL_LOADED = "Tool loaded.";

const resultText = (content) =>
  stripReminders(
    blocksOf(content)
      .map((block) => (block.type === "text" ? block.text : `[${block.type}]`))
      .join("\n\n"),
  );

/**
 * 一个请求里模型看到的对话：去掉 system 角色的消息与提醒，按块展开成 `{role, text}`、`{role, tool_use}`、
 * `{role, tool_result}`。两边对同一段历史给出的条目应当相同。
 */
export function conversation(body) {
  const items = [];
  for (const message of body?.messages ?? []) {
    if (message.role === "system") continue;
    for (const block of blocksOf(message.content)) {
      if (block.type === "text") {
        const value = stripReminders(block.text);
        if (value && value !== TOOL_LOADED) items.push({ role: message.role, text: value });
      } else if (block.type === "tool_use") {
        items.push({
          role: "assistant",
          tool_use: { id: block.id, name: block.name, input: block.input },
        });
      } else if (block.type === "tool_result") {
        items.push({
          role: "user",
          tool_result: {
            id: block.tool_use_id,
            error: block.is_error === true,
            text: resultText(block.content),
          },
        });
      } else if (block.type === "thinking") {
        items.push({ role: "assistant", thinking: block.thinking });
      }
    }
  }
  return items;
}

/** 替身回复的块 → 条目（与 `conversation` 同一形状）。 */
export function replyItems(reply) {
  return (reply?.content ?? []).flatMap((block) => {
    if (block.type === "text") return block.text ? [{ role: "assistant", text: block.text }] : [];
    if (block.type === "tool_use")
      return [
        { role: "assistant", tool_use: { id: block.id, name: block.name, input: block.input } },
      ];
    return [];
  });
}

const lastUser = (body) => (body?.messages ?? []).findLast((message) => message.role === "user");

/**
 * 输入里的动作，按出现的顺序：`[[read 路径]]`、`[[write 路径|内容]]`（内容里的 `\\n` 是换行）；第二十二段（工作线 F84）
 * 另有 `[[search 查询]]`（ToolSearch）与 `[[call 工具|JSON 输入]]`。
 */
function actions(prompt) {
  return [...prompt.matchAll(/\[\[(read|write|search|call) ([^|\]]+)(?:\|([^\]]*))?\]\]/g)].map(
    ([, kind, arg, body]) => {
      if (kind === "read") return { name: "Read", input: { file_path: arg } };
      if (kind === "search") return { name: "ToolSearch", input: { query: arg } };
      if (kind === "call") return { name: arg, input: JSON.parse(body ?? "{}") };
      return {
        name: "Write",
        input: { file_path: arg, content: (body ?? "").replaceAll("\\n", "\n") },
      };
    },
  );
}

/** 这一回合的输入（最后一条带文字、不是工具结果的 user 消息）与之后已经发出的工具调用数。 */
function turnOf(body) {
  const messages = (body?.messages ?? []).filter((message) => message.role !== "system");
  let calls = 0;
  for (let index = messages.length - 1; index >= 0; index -= 1) {
    const blocks = blocksOf(messages[index].content);
    if (messages[index].role === "assistant") {
      calls += blocks.filter((block) => block.type === "tool_use").length;
      continue;
    }
    const texts = blocks
      .filter((block) => block.type === "text")
      .map((block) => stripReminders(block.text))
      .filter(Boolean);
    if (texts.length && !blocks.some((block) => block.type === "tool_result"))
      return { prompt: texts.join("\n"), calls };
  }
  return { prompt: "", calls };
}

/**
 * 本地模型替身：压缩提示 → 带标记的摘要；没有工具清单的旁路请求（标题等）→ 文本；主请求按这一回合输入里的动作
 * 依次发工具调用（每次一个，前一个的结果回来后发下一个），动作做完 → 文本。每个请求记下类别与回复。
 */
export async function startModel() {
  const fixture = await startModelFixture({
    classify: () => "side",
    side: (record, body) => {
      const last = lastUser(body);
      const tools = (body.tools ?? []).map((tool) => tool.name);
      let reply;
      if (JSON.stringify(last ?? "").includes(COMPACT_PROMPT)) {
        record.kind = "compact";
        reply = text(
          `<analysis>a</analysis><summary>SUMMARY-${record.seq}: the earlier turns.</summary>`,
        );
      } else if (!tools.includes("Read")) {
        record.kind = "side";
        reply = text("Side reply");
      } else {
        record.kind = "main";
        const { prompt, calls } = turnOf(body);
        const next = actions(prompt)[calls];
        reply = next
          ? toolUse(`toolu_f12_${record.seq}`, next.name, next.input)
          : text(calls ? `Done ${record.seq}.` : `Reply ${record.seq}.`);
      }
      record.parsed = body;
      record.reply = reply;
      return reply;
    },
  });
  return fixture;
}

/** 个人 provider 配置：一个 anthropic-messages 的个人 provider 指向替身，缺省选择带档位（Node 只绑定完整选择）。 */
export async function writeProviderConfig(home, baseUrl) {
  const v2 = join(home, ".zcode", "v2");
  await mkdir(v2, { recursive: true });
  const file = join(v2, "provider_config.json");
  const config = {
    schemaVersion: 1,
    config: {
      providerConfigRules: {
        providerRules: [
          {
            providerId: PROVIDER,
            providerName: "Interop fake",
            enabled: true,
            config: {
              group: "standard-personal",
              access: { type: "api-key", apiKey: "sk-interop" },
              api: { type: "anthropic-messages", baseUrl },
              personalModelIds: [MODEL],
            },
          },
        ],
      },
      // Node 的个人配置 schema 要求 `modelConfigRules`（没有时整个文件按无效处理）。
      modelConfigRules: { providerModelRules: [], manualProviderModelRules: [] },
      defaultModelSelection: {
        providerId: PROVIDER,
        modelId: MODEL,
        options: { reasoningLevel: "high" },
      },
    },
  };
  await writeFile(file, `${JSON.stringify(config, null, 2)}\n`);
  return file;
}

/** Node 一侧：同一份 provider 配置的 registry、真实的 `createZCodeApp`、读库用的 store。 */
export async function nodeSide({ nodeRoot, repoRoot, layout, personalFile }) {
  const provider = await import(
    pathToFileURL(join(nodeRoot, "packages/provider/src/index.ts")).href
  );
  const providerNode = await import(
    pathToFileURL(join(nodeRoot, "packages/provider-node/src/index.ts")).href
  );
  const bootstrap = await importNode(nodeRoot, "bootstrap/src/index.ts");
  const registryRuntime = providerNode.createNodeProviderRegistryRuntime({
    zcodeBuiltinFilePath: join(
      repoRoot,
      "packages/rust-cli/crates/platform/assets/zcode-builtin.json",
    ),
    personalFilePath: personalFile,
    personalPollingIntervalMs: false,
    accountSource: new provider.MutableAccountProviderConfigSource(),
  });
  await registryRuntime.start();
  const selections = new providerNode.NodeModelSelectionConfigRepository({
    personalRepository: registryRuntime.personalRepository,
  });
  const initial = provider.resolveInitialModelSelection({
    configuredDefault: await selections.read(),
    registry: { revision: 0, providers: registryRuntime.registryService.listProviders() },
  });
  if (initial.source === "none") throw new Error("Node resolves no model from the provider config");
  const userConfigPath = join(layout.base, "node-config.json");
  await writeFile(
    userConfigPath,
    `${JSON.stringify({ features: { mcp: false, memory: false, skill: false }, logging: { level: "warn" } })}\n`,
  );
  /** 打开（或新建）一个会话的 App；`fn(app)` 之后关闭。 */
  async function withApp(sessionId, fn) {
    const app = await bootstrap.createZCodeApp({
      env: layout.env,
      runtimeConfig: {
        mcp: { enabled: false, servers: {} },
        memory: { enabled: false, use: false },
        mode: "yolo",
        workingDirectory: layout.work,
        maxTurns: 8,
      },
      configuredDefaultModelSelection: initial.selection,
      userConfigPath,
      providerRegistry: registryRuntime.registryService,
      ...(sessionId ? { resume: true, sessionId } : {}),
    });
    try {
      return await fn(app);
    } finally {
      await app.close?.();
    }
  }
  return {
    /** Node 真实回合：`submitPrompt`（新会话或 `resume`）。 */
    turn: (sessionId, prompt) =>
      withApp(sessionId, async (app) => {
        const result = await app.submitPrompt(prompt);
        return { session: app.sessionId, response: result.response };
      }),
    /** Node 的对话回退（App 的 edit / retry 与 `/rewind` 共用的 `rewindConversationToMessage`）。 */
    revert: (sessionId, targetMessageId) =>
      withApp(sessionId, async (app) => {
        await app.resume();
        const result = await app.runtime.rewindConversationToMessage({
          events: [],
          targetMessageId,
          traceContext: { traceId: sessionId },
        });
        return { strategy: result.strategy };
      }),
    /** Node 的文件回退（App 的 `applyFileRewind`）。 */
    fileRewind: (sessionId, targetMessageIds) =>
      withApp(sessionId, async (app) => {
        await app.resume();
        const result = await app.runtime.applyWorkspaceFileRewind({ targetMessageIds });
        return {
          applied: result.applied,
          files: (result.preview?.files ?? []).map((file) => [file.path, file.action ?? null]),
          unsafe: (result.preview?.unsafeFiles ?? []).map((file) => [file.path, file.reason]),
        };
      }),
    /** Node 的文件回退预览（App 的改动摘要与“能否回退”）。 */
    filePreview: (sessionId, targetMessageIds) =>
      withApp(sessionId, async (app) => {
        await app.resume();
        return app.runtime.previewWorkspaceFileRewind({ targetMessageIds });
      }),
    dispose() {
      selections.dispose();
      registryRuntime.dispose();
    },
  };
}

/** 跑一个子进程，收集 stdout / stderr。 */
export function run(file, args, options) {
  return new Promise((resolve, reject) => {
    const child = spawn(file, args, { ...options, stdio: ["ignore", "pipe", "pipe"] });
    let stdout = "";
    let stderr = "";
    child.stdout.on("data", (chunk) => (stdout += chunk));
    child.stderr.on("data", (chunk) => (stderr += chunk));
    child.on("error", reject);
    child.on("exit", (code) => resolve({ code, stdout, stderr }));
  });
}

/** rust-cli 一侧：真实的 `zcode -p`（必须异步启动：替身在本进程里，同步等子进程会卡住它）与内核例子程序。 */
export function rustSide({ zcode, kernel, interop, layout, baseUrl }) {
  const json = async (file, args) => {
    const result = await run(file, args, { cwd: layout.work, env: layout.env });
    if (result.code !== 0)
      throw new Error(`${file} ${args.join(" ")} exited ${result.code}: ${result.stderr.trim()}`);
    return JSON.parse(result.stdout);
  };
  return {
    /** `zcode [-r <id>] -p <prompt>`（`--permission-mode bypassPermissions`：Write 不问）。 */
    async turn(sessionId, prompt) {
      const args = [
        ...(sessionId ? ["-r", sessionId] : []),
        "-p",
        prompt,
        "--permission-mode",
        "bypassPermissions",
      ];
      const result = await run(zcode, args, { cwd: layout.work, env: layout.env });
      if (result.code !== 0)
        throw new Error(`zcode ${args.join(" ")} exited ${result.code}: ${result.stderr.trim()}`);
      return result.stdout.trim();
    },
    compact: (sessionId) => json(kernel, ["compact", layout.storage, sessionId, baseUrl]),
    rewind: (sessionId, target, scope) =>
      json(kernel, ["rewind", layout.storage, sessionId, target, scope]),
    branch: (sessionId) => json(kernel, ["branch", layout.storage, sessionId]),
    /** `/fork` 选中 `message` 这条输入（F75）。 */
    fork: (sessionId, message) => json(kernel, ["fork", layout.storage, sessionId, message]),
    options: (sessionId) => json(kernel, ["options", layout.storage, sessionId]),
    read: (sessionId) => json(interop, ["read", layout.storage, sessionId]),
  };
}
