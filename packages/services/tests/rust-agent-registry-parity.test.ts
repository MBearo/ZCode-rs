import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import test from "node:test";
import { z } from "zod";
import {
  ProviderConfigResolver,
  parseZCodeBuiltinProviderConfigRules,
  parseZCodeBuiltinModelConfigRules,
  parsePersonalProviderConfigMap,
  parsePersonalModelConfigRules,
  parseAccountProviderConfigMap,
} from "@zcode/provider";
import { fixture } from "./rust-agent-fixture.js";
import { configureRegistry } from "./rust-agent-registry-fixture.js";

test("Personal manual rules, null clearing, dictionary replacement and ordering match TS", async () => {
  const f = await fixture({ registry: true });
  try {
    const { builtin } = await configureRegistry(f);
    builtin.config.providerConfigRules.templateRules.push({
      templateId: "fixture-overlay",
      templateNameMap: { "en-US": "Fixture overlay", "zh-CN": "配置覆盖测试" },
      config: {
        api: {
          type: "openai-chat-completions",
          baseUrl: f.baseUrl,
          headers: { "x-template": "must-be-cleared" },
        },
        access: { type: "api-key" },
        builtinModelIds: ["model-a", "model-null", "model-disabled"],
      },
    });
    const manual = {
      properties: {
        contextWindow: 90000,
        supportsJsonSchemaOutput: false,
        supportsNativeWebSearch: false,
        supportsMidConversationSystem: true,
        inputFormat: { supportsImage: false, supportsVideo: false, supportsPdf: false },
      },
      optionSpecs: {
        reasoningLevel: { values: ["none", "extra"], map: "{'reasoning_effort': reasoningLevel}" },
        maxOutputTokens: { max: 2048 },
      },
    };
    const ids = ["personal:first", "personal:second"];
    const personal: any = {
      schemaVersion: 1,
      config: {
        providerOrder: [...ids].reverse(),
        providerConfigRules: {
          providerRules: ids.map((providerId, index) => ({
            providerId,
            templateId: "fixture-overlay",
            config: {
              group: "standard-personal",
              access: { type: "api-key", apiKey: "synthetic-overlay-key" },
              api: { headers: index === 0 ? null : { "x-personal": "replacement" } },
              personalModelIds: ["model-b", "model-a"],
              modelOrder: ["model-b", "model-a"],
            },
          })),
        },
        modelConfigRules: {
          providerModelRules: ids.flatMap((providerId) => [
            { providerId, modelId: "model-null", config: { properties: { contextWindow: null } } },
            { providerId, modelId: "model-disabled", config: { enabled: false } },
          ]),
          manualProviderModelRules: ids.map((providerId) => ({
            providerId,
            modelId: "model-a",
            config: manual,
          })),
        },
      },
    };
    const { providers, providerTemplates } = parseZCodeBuiltinProviderConfigRules(
      builtin.config.providerConfigRules,
    );
    const expected = new ProviderConfigResolver()
      .resolve({
        zcodeBuiltinProviders: providers,
        zcodeBuiltinProviderTemplates: providerTemplates,
        personalProviders: parsePersonalProviderConfigMap(personal.config.providerConfigRules),
        zcodeBuiltinModelRules: parseZCodeBuiltinModelConfigRules(builtin.config.modelConfigRules),
        personalModels: parsePersonalModelConfigRules(personal.config.modelConfigRules),
        accountProviders: parseAccountProviderConfigMap({}),
        personalProviderOrder: personal.config.providerOrder,
      })
      .registryProviders.flatMap((p) =>
        p.models.map((m) => [
          p.providerId,
          m.modelId,
          m.config.properties.contextWindow,
          [...m.config.optionSpecs.reasoningLevel.values],
        ]),
      );
    assert.equal(expected.length, 4);
    await writeFile(join(f.root, "builtin.json"), JSON.stringify(builtin));
    await writeFile(join(f.root, "personal.json"), JSON.stringify(personal));
    const h = f.start();
    await h.subscribe(`workspace-config/${f.cwd}`);
    const frame = await h.wait((m) => m.params?.frame?.payload?.snapshot?.config);
    const options = frame.params.frame.payload.snapshot.config.configOptions[0].options;
    assert.deepEqual(
      options.map((o: any) => [o.modelProviderId, o.value, o.contextWindow, o.modelThoughtLevels]),
      expected,
    );
    for (const providerId of ids) {
      const id = await h.create();
      await h.subscribe(`conversation/${id}`);
      await h.command(
        h.envelope("sendText", id, {
          text: "verify manual mapping",
          modelSelection: { providerId, modelId: "model-a", options: { reasoningLevel: "extra" } },
        }),
      );
      await h.completed(id);
      assert.equal(f.requests.at(-1)?.reasoning_effort, "extra");
      assert.equal(f.requests.at(-1)?.max_tokens, 2048);
      assert.equal(f.requestHeaders.at(-1)?.["x-template"], undefined);
    }
    assert.equal(f.requestHeaders[0]?.["x-personal"], undefined);
    assert.equal(f.requestHeaders[1]?.["x-personal"], "replacement");
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});

test("Native Registry catalog matches TS builtin/template/overlay model ordering and reasoning levels", async () => {
  const f = await fixture({ registry: true });
  try {
    const builtin = JSON.parse(await readFile("config/provider/zcode-builtin.json", "utf8"));
    const personal: any = {
      schemaVersion: 1,
      config: {
        providerConfigRules: {
          providerRules: builtin.config.providerConfigRules.templateRules.map((t: any) => ({
            providerId: `personal:${t.templateId}`,
            templateId: t.templateId,
            providerName: t.name ?? t.templateId,
            config: {
              group: "standard-personal",
              access: { type: "api-key", apiKey: "fixture-synthetic" },
              personalModelIds: [
                "gpt-5",
                "gpt-4.1",
                "claude-sonnet-4-5",
                "glm-5",
                "deepseek-chat",
                "kimi-k2.5",
                "minimax-m2.5",
                "qwen3-coder-plus",
                "mimo-v2-pro",
              ],
            },
          })),
        },
        modelConfigRules: { providerModelRules: [], manualProviderModelRules: [] },
      },
    };
    const accounts: any = {};
    const states: any = {};
    for (const p of builtin.config.providerConfigRules.providerRules) {
      accounts[p.providerId] = {
        access: { type: "zhipu-account", entitled: true },
        builtinModelIds: ["glm-5", "glm-4.7"],
      };
      states[p.providerId] = {
        entitled: true,
        availability: "available",
        ...(p.config.access.mode === "off-peak" ? {} : { current: true }),
      };
    }
    const { providers, providerTemplates } = parseZCodeBuiltinProviderConfigRules(
      builtin.config.providerConfigRules,
    );
    const expected = new ProviderConfigResolver()
      .resolve({
        zcodeBuiltinProviders: providers,
        zcodeBuiltinProviderTemplates: providerTemplates,
        personalProviders: parsePersonalProviderConfigMap(personal.config.providerConfigRules),
        zcodeBuiltinModelRules: parseZCodeBuiltinModelConfigRules(builtin.config.modelConfigRules),
        personalModels: parsePersonalModelConfigRules(personal.config.modelConfigRules),
        accountProviders: parseAccountProviderConfigMap(accounts),
        accountStates: states,
      })
      .registryProviders.filter((p) => p.config.visibility !== "hidden")
      .flatMap((p) =>
        p.models.map((m) => [
          p.providerId,
          m.modelId,
          [...m.config.optionSpecs.reasoningLevel.values],
        ]),
      );
    assert.ok(expected.length > 30);
    await writeFile(join(f.root, "builtin.json"), JSON.stringify(builtin));
    await writeFile(join(f.root, "personal.json"), JSON.stringify(personal));
    const h = f.start();
    const revision = `zcode-builtin:${builtin.revision}:${createHash("sha256").update(join(f.root, "builtin.json")).digest("hex")}`;
    await h.client.request(
      "provider/updateAccountConfig",
      {
        revision: "full-fixture",
        basedOnZCodeBuiltinRevision: revision,
        providers: accounts,
        states,
      },
      z.any(),
    );
    await h.subscribe(`workspace-config/${f.cwd}`);
    const frame = await h.wait((m) => m.params?.frame?.payload?.snapshot?.config);
    const options = frame.params.frame.payload.snapshot.config.configOptions[0].options;
    assert.deepEqual(
      options.map((o: any) => [o.modelProviderId, o.value, o.modelThoughtLevels]),
      expected,
    );
    assert.equal(f.requests.length, 0);
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});

test("Account/file revision pairing applies atomically and old catalog remains usable while waiting", async () => {
  const f = await fixture({ registry: true });
  try {
    const { builtin, revision } = await configureRegistry(f, true);
    const h = f.start();
    const account = {
      revision: "first",
      basedOnZCodeBuiltinRevision: revision,
      providers: { "account:fixture": { access: { type: "zhipu-account", entitled: true } } },
      states: { "account:fixture": { current: true } },
    };
    const sync = (v: any) => h.client.request("provider/updateAccountConfig", v, z.any());
    assert.equal((await sync(account)).status, "received");
    assert.equal((await sync(account)).status, "unchanged");
    await h.subscribe(`workspace-config/${f.cwd}`);
    const first = await h.wait((m) => m.params?.frame?.payload?.snapshot?.config);
    const next = {
      ...account,
      revision: "second",
      basedOnZCodeBuiltinRevision: revision.replace(
        `:${builtin.revision}:`,
        `:${builtin.revision + 1}:`,
      ),
      providers: {
        "account:fixture": {
          access: { type: "zhipu-account", entitled: true },
          builtinModelIds: ["model-b"],
        },
      },
    };
    assert.equal((await sync(next)).status, "received");
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    const old = await h.wait((m) => m.params?.frame?.payload?.snapshot?.sessionId === id);
    assert.equal(old.params.frame.payload.snapshot.config.model, "model-a");
    builtin.revision++;
    const before = h.messages.length;
    await writeFile(join(f.root, "builtin.json"), JSON.stringify(builtin));
    const changed = await h.wait(
      (m) =>
        m.params?.frame?.payload?.snapshot?.config?.configOptions?.[0]?.options?.[0]?.value ===
        "model-b",
      before,
    );
    assert.notDeepEqual(
      changed.params.frame.payload.snapshot.config,
      first.params.frame.payload.snapshot.config,
    );
    const fresh = await h.create();
    await h.subscribe(`conversation/${fresh}`);
    const snap = await h.wait((m) => m.params?.frame?.payload?.snapshot?.sessionId === fresh);
    assert.equal(snap.params.frame.payload.snapshot.config.model, "model-b");
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});
