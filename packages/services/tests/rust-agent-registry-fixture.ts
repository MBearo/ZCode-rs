import { createHash } from "node:crypto";
import { readFile, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { fixture } from "./rust-agent-fixture.js";
export async function configureRegistry(f: Awaited<ReturnType<typeof fixture>>, account = false) {
  const builtin = JSON.parse(await readFile(resolve("config/provider/zcode-builtin.json"), "utf8"));
  builtin.config.providerConfigRules.providerRules = account
    ? [
        {
          providerId: "account:fixture",
          config: {
            group: "zai-family",
            api: { type: "openai-chat-completions", baseUrl: f.baseUrl },
            access: { type: "zhipu-account", accountType: "zai", mode: "individual-coding-plan" },
            builtinModelIds: ["model-a"],
          },
        },
      ]
    : [];
  await writeFile(join(f.root, "builtin.json"), JSON.stringify(builtin));
  const personal = {
    schemaVersion: 1,
    config: {
      providerConfigRules: {
        providerRules: account
          ? []
          : [
              {
                providerId: "personal:fixture",
                providerName: "Fixture",
                config: {
                  group: "standard-personal",
                  access: { type: "api-key", apiKey: "fixture-personal-key" },
                  api: { type: "openai-chat-completions", baseUrl: f.baseUrl },
                  personalModelIds: ["model-a", "model-b"],
                },
              },
            ],
      },
      modelConfigRules: {
        providerModelRules: ["model-a", "model-b"].map((modelId) => ({
          providerId: account ? "account:fixture" : "personal:fixture",
          modelId,
          config: {
            optionSpecs: {
              reasoningLevel: {
                values: ["low", "high"],
                map: "{'reasoning_effort': reasoningLevel}",
              },
            },
          },
        })),
        manualProviderModelRules: [],
      },
      defaultModelSelection: {
        providerId: account ? "account:fixture" : "personal:fixture",
        modelId: "model-a",
        options: { reasoningLevel: "low" },
      },
    },
  };
  await writeFile(join(f.root, "personal.json"), JSON.stringify(personal));
  return {
    personal,
    builtin,
    revision: `zcode-builtin:${builtin.revision}:${createHash("sha256").update(join(f.root, "builtin.json")).digest("hex")}`,
  };
}
