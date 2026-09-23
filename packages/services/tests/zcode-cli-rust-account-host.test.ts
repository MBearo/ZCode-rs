import assert from "node:assert/strict";
import test from "node:test";
import { randomUUID } from "node:crypto";
import { join } from "node:path";
import {
  parseAccountProviderConfigMap,
  parseZCodeBuiltinProviderConfigRules,
  parseZCodeBuiltinModelConfigRules,
  parsePersonalProviderConfigMap,
  parsePersonalModelConfigRules,
  ProviderConfigResolver,
  projectModelSelectionProviderView,
} from "@zcode/provider";
import { fixture, binary } from "./zcode-cli-rust-fixture.js";
import { configureRegistry } from "./zcode-cli-rust-registry-fixture.js";
import { createZCodeAgentService } from "../src/zcode-agent/zcodeAgentService.js";
import { conversationTopicFrameSchema } from "@zcode/shared/zcode-protocol-v4";

test("Existing Host account source and auth service drive native sessions, connectivity and workspace generation", async () => {
  const f = await fixture({ registry: true });
  const { revision, builtin, personal } = await configureRegistry(f, true);
  const accountProviders = parseAccountProviderConfigMap({
    "account:fixture": { access: { type: "zhipu-account", entitled: true } },
  });
  const { providers, providerTemplates } = parseZCodeBuiltinProviderConfigRules(
    builtin.config.providerConfigRules,
  );
  const resolution = new ProviderConfigResolver().resolve({
    zcodeBuiltinProviders: providers,
    zcodeBuiltinProviderTemplates: providerTemplates,
    personalProviders: parsePersonalProviderConfigMap(personal.config.providerConfigRules),
    zcodeBuiltinModelRules: parseZCodeBuiltinModelConfigRules(builtin.config.modelConfigRules),
    personalModels: parsePersonalModelConfigRules(personal.config.modelConfigRules),
    accountProviders,
    accountStates: {
      "account:fixture": { current: true, entitled: true, availability: "available" },
    },
  });
  let authCount = 0;
  const service = createZCodeAgentService({
    presentationSurface: "desktop",
    requestTimeoutMs: 5000,
    modelSelectionReadinessSource: {
      async getView() {
        return {
          revision: 1,
          providers: resolution.registryProviders.map(projectModelSelectionProviderView),
        };
      },
    },
    commandResolver: () => ({
      command: binary,
      args: ["app-server", "--stdio", "--cwd", f.cwd, "--data-dir", f.dataDir],
      cwd: f.cwd,
      supportsStorageStartup: true,
      storagePreparationMode: "process",
      env: {
        HOME: f.root,
        ZCODE_SESSION_DB_PATH: join(f.root, "ts.sqlite"),
        ZCODE_BUILTIN_PROVIDER_CONFIG_FILE: join(f.root, "builtin.json"),
        ZCODE_PERSONAL_PROVIDER_CONFIG_FILE: join(f.root, "personal.json"),
      },
    }),
    accountProviderConfigSource: {
      async read() {
        return {
          revision: "host-a1",
          basedOnZCodeBuiltinRevision: revision,
          providers: parseAccountProviderConfigMap({
            "account:fixture": { access: { type: "zhipu-account", entitled: true } },
          }),
          states: {
            "account:fixture": { current: true, entitled: true, availability: "available" },
          },
        };
      },
      onDidChange: () => () => {},
    },
    accountRequestAuthService: {
      async resolveAccessCurrent() {
        return null;
      },
      async assertCurrent() {},
      async resolveCurrent(input) {
        assert.equal(input.providerId, "account:fixture");
        authCount++;
        return { apiKey: `fixture-host-${authCount}` };
      },
    },
  });
  try {
    const workspace = { workspacePath: f.cwd };
    assert.equal((await service.initialize(workspace)).available, true);
    const selection = {
      providerId: "account:fixture",
      modelId: "model-a",
      options: { reasoningLevel: "low" },
    };
    const connected = await service.testModelConnectivity({ ...workspace, selection });
    assert.equal(connected.success, true);
    const generated = await service.generateWorkspaceText({
      ...workspace,
      selection,
      prompt: "commit description",
      querySource: "fixture",
      maxOutputTokens: 123,
    });
    assert.equal(generated.text, "你好 Rust");
    assert.equal(f.requests.at(-1)?.max_tokens, 123);
    const created = await service.sendConversationCommandV4({
      ...workspace,
      envelope: {
        commandId: randomUUID(),
        clientId: "host-test",
        sessionId: null,
        type: "createSession",
        issuedAt: Date.now(),
        payload: { workspaceId: f.cwd, config: { mode: "yolo", modelSelection: selection } },
      },
    });
    assert.equal(created.result?.type, "createSession");
    if (created.result?.type !== "createSession") throw new Error("Missing session");
    const sessionId = created.result.sessionId;
    let timer: NodeJS.Timeout | undefined;
    let dispose = () => {};
    const completed = new Promise<void>((done, fail) => {
      timer = setTimeout(() => fail(new Error("Host stream timeout")), 5000);
      const sub = service.onDynamicConversationFrame(workspace)((wire) => {
        if (wire.kind !== "complete") return;
        const frame = conversationTopicFrameSchema.parse(wire.frame);
        if (
          frame.payload.kind === "deltas" &&
          frame.payload.deltas.some(
            (d) => d.op === "state.updated" && d.patch.control?.phase === "completedSuccess",
          )
        )
          done();
      });
      dispose = () => sub.dispose();
    });
    try {
      await service.subscribeConversationV4({ ...workspace, sessionId });
      await service.sendConversationCommandV4({
        ...workspace,
        envelope: {
          commandId: randomUUID(),
          clientId: "host-test",
          sessionId,
          type: "sendText",
          issuedAt: Date.now(),
          payload: { text: "hello" },
        },
      });
      await completed;
    } finally {
      clearTimeout(timer);
      dispose();
    }
    assert.equal(authCount, 3);
    // Renderer 的 task-index 通过既有只读快照回源，V4 流成功不能代替该契约。
    const snapshot = await service.readSession({
      ...workspace,
      sessionId,
      runtimePolicy: "existing-only",
    });
    assert.equal(snapshot.session.title, "hello");
    assert.equal(snapshot.session.workspace.workspaceKey, f.cwd);
    assert.equal(snapshot.session.sessionKind, "interactive");
    assert.deepEqual(snapshot.settings.model.current, selection);
    assert.equal(snapshot.settings.model.available[0]?.ref.modelId, "model-a");
    assert.ok(
      snapshot.messages.some(
        (m) =>
          m.info.role === "user" && m.parts.some((p) => p.type === "text" && p.text === "hello"),
      ),
    );
    assert.ok(
      snapshot.messages.some(
        (m) =>
          m.info.role === "assistant" &&
          m.parts.some((p) => p.type === "text" && p.text === "你好 Rust"),
      ),
    );
    assert.equal(snapshot.runtime.pendingRequestIds.length, 0);
    assert.equal(authCount, 3);
    assert.deepEqual(
      f.requestHeaders.map((h) => h.authorization),
      ["Bearer fixture-host-1", "Bearer fixture-host-2", "Bearer fixture-host-3"],
    );
  } finally {
    await service.disposeAllAndWait();
    await f.close();
  }
});
