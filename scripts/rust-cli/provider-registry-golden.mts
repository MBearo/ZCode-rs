#!/usr/bin/env node
// rust-cli 平台层的 Provider 对照夹具（docs/specs/rust-cli/platform.md §7）：用 Node 的 Provider registry
// 解析内置 + 个人配置，输出 CLI 能执行的模型。在装好依赖的检出里运行：
//
//   node_modules/.bin/tsx scripts/rust-cli/provider-registry-golden.mts \
//     packages/rust-cli/crates/platform/assets/zcode-builtin.json \
//     packages/rust-cli/crates/platform/tests/fixtures/personal.json \
//     packages/rust-cli/crates/platform/tests/fixtures/node-registry.json \
//     account:zai-individual-coding-plan
//
// 最后一个参数是账号 overlay 里唯一授权的账号 provider（其余账号 provider 一律不授权）。
import { writeFileSync } from "node:fs";
import {
  MutableAccountProviderConfigSource,
  ProviderConfig,
  ProviderConfigMap,
  ZhipuAccountAccessConfig,
  createAccountProviderConfigSnapshot,
} from "../../packages/provider/src/index.ts";
import { createNodeProviderRegistryRuntime } from "../../packages/provider-node/src/index.ts";

const [builtin, personal, out, entitled] = process.argv.slice(2);
const accountSource = new MutableAccountProviderConfigSource();
const runtime = createNodeProviderRegistryRuntime({
  zcodeBuiltinFilePath: builtin,
  personalFilePath: personal,
  personalPollingIntervalMs: false,
  accountSource,
});
await runtime.configService.start?.();
const config = await runtime.configService.read();
accountSource.replace(
  createAccountProviderConfigSnapshot(
    config.zcodeBuiltinRevision,
    new ProviderConfigMap(
      config.zcodeBuiltinProviders
        .entries()
        .filter(([, provider]) => provider.access?.type === "zhipu-account")
        .map(([id]) => [
          id,
          new ProviderConfig({
            access: new ZhipuAccountAccessConfig({ entitled: id === entitled }),
          }),
        ]),
    ),
  ),
  "fixture",
);
await runtime.start();
const models = runtime.registryService.listProviders().flatMap((provider) =>
  provider.models.map((model) => ({
    provider: provider.providerId,
    model: model.modelId,
    apiType: provider.config.api?.type,
    baseUrl: provider.config.api?.baseUrl,
    hidden: provider.config.visibility === "hidden",
    reasoningLevels: model.config.optionSpecs?.reasoningLevel?.values,
    contextWindow: model.config.properties?.contextWindow,
    maxOutputTokens: model.config.optionSpecs?.maxOutputTokens?.max,
  })),
);
writeFileSync(out, `${JSON.stringify(models, null, 2)}\n`);
runtime.dispose();
process.exit(0);
