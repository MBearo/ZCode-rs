// rust-cli 与 Node CLI 的互操作检查第八段：新会话的默认模型（docs/specs/rust-cli/workspace-trust.md §3）。
//
// 1. 同一份内置配置（rust-cli 随包副本）与个人配置夹具，五种 `defaultModelSelection`（可选、没有档位、档位无效、
//    模型不在目录、没有）下，Node `resolveInitialModelSelection` 与 rust-cli 的 `catalog.default`（`ui_prefs
//    model-initial`）相同；
// 2. rust-cli 写的缺省（`ui_prefs model-write`，`/model` 与 `/effort` 共用的写入）由 Node 解析成同一个初始选择；
//    Node `saveConfiguredDefault` 写的由 rust-cli 解析成同一个；
// 3. 从 Node 规范化过的夹具出发，两边写同一个选择得到逐字节相同的文本（带 provider 与规则的真实内容，
//    L29 的 `checkDefaultModel` 覆盖空配置）。
import { execFileSync } from "node:child_process";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

const FIXTURE = "packages/rust-cli/crates/platform/tests/fixtures/personal.json";
const BUILTIN = "packages/rust-cli/crates/platform/assets/zcode-builtin.json";

export async function checkInitialModel({ nodeRoot, repoRoot, example, home, expectEqual }) {
  const provider = await import(pathToFileURL(join(nodeRoot, "packages/provider/src/index.ts")).href);
  const providerNode = await import(
    pathToFileURL(join(nodeRoot, "packages/provider-node/src/index.ts")).href
  );
  const file = join(home, ".zcode", "v2", "provider_config.json");
  await mkdir(join(home, ".zcode", "v2"), { recursive: true });
  const rust = (...command) =>
    JSON.parse(execFileSync(example("ui_prefs"), command, { encoding: "utf8" }));

  /** Node 独立 CLI 的读法：个人配置仓库读缺省，registry 没有账号凭据（fail closed）。 */
  async function nodeInitial() {
    const runtime = providerNode.createNodeProviderRegistryRuntime({
      zcodeBuiltinFilePath: join(repoRoot, BUILTIN),
      personalFilePath: file,
      personalPollingIntervalMs: false,
      accountSource: new provider.MutableAccountProviderConfigSource(),
    });
    try {
      await runtime.start();
      const selections = new providerNode.NodeModelSelectionConfigRepository({
        personalRepository: runtime.personalRepository,
      });
      const configuredDefault = await selections.read();
      selections.dispose();
      const initial = provider.resolveInitialModelSelection({
        configuredDefault,
        registry: { revision: 0, providers: runtime.registryService.listProviders() },
      });
      return initial.source === "none"
        ? null
        : {
            provider: initial.selection.providerId,
            model: initial.selection.modelId,
            reasoning: initial.selection.options?.reasoningLevel ?? null,
          };
    } finally {
      runtime.dispose();
    }
  }

  const fixture = JSON.parse(await readFile(join(repoRoot, FIXTURE), "utf8"));
  const withDefault = async (selection) => {
    const value = structuredClone(fixture);
    if (selection === undefined) delete value.config.defaultModelSelection;
    else value.config.defaultModelSelection = selection;
    await writeFile(file, JSON.stringify(value, null, 2));
  };
  const cases = [
    ["usable", { providerId: "personal:gateway", modelId: "custom-chat", options: { reasoningLevel: "low" } }],
    ["without a level", { providerId: "personal:gateway", modelId: "custom-chat" }],
    ["with an unknown level", { providerId: "personal:gateway", modelId: "custom-chat", options: { reasoningLevel: "max" } }],
    ["for a missing model", { providerId: "personal:gateway", modelId: "gone", options: { reasoningLevel: "low" } }],
    ["absent", undefined],
  ];
  for (const [name, selection] of cases) {
    await withDefault(selection);
    expectEqual(`initial model when the default is ${name}`, rust("model-initial", home), await nodeInitial());
  }

  // rust-cli 写、Node 解析；Node 写、rust-cli 解析。
  await withDefault(undefined);
  rust("model-write", home, "personal:anthropic", "claude-opus-5", "xhigh");
  expectEqual("Node resolves the default rust-cli saved", await nodeInitial(), {
    provider: "personal:anthropic",
    model: "claude-opus-5",
    reasoning: "xhigh",
  });
  const personal = new providerNode.NodePersonalProviderConfigRepository({
    filePath: file,
    pollingIntervalMs: false,
  });
  const selections = new providerNode.NodeModelSelectionConfigRepository({ personalRepository: personal });
  try {
    await selections.saveConfiguredDefault({
      providerId: "personal:responses",
      modelId: "gpt-5.4",
      options: { reasoningLevel: "medium" },
    });
    expectEqual("rust-cli resolves the default Node saved", rust("model-initial", home), {
      provider: "personal:responses",
      model: "gpt-5.4",
      reasoning: "medium",
    });
    // 同一份（Node 规范化过的）输入，两边写同一个选择：文本逐字节相同。
    const start = await readFile(file, "utf8");
    const chosen = { providerId: "personal:gateway", modelId: "glm-5", options: { reasoningLevel: "enabled" } };
    await selections.saveConfiguredDefault(chosen);
    const nodeText = await readFile(file, "utf8");
    await writeFile(file, start);
    rust("model-write", home, "personal:gateway", "glm-5", "enabled");
    expectEqual("the same default over the fixture gives the same file text", await readFile(file, "utf8"), nodeText);
  } finally {
    selections.dispose();
    personal.dispose();
  }
}
