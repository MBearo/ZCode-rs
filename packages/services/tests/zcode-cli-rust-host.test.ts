import assert from "node:assert/strict";
import test from "node:test";
import { randomUUID } from "node:crypto";
import { access, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { fixture, binary } from "./zcode-cli-rust-fixture.js";
import type { ModelSelectionView } from "@zcode/provider";
import { completeNewModelSelection } from "@zcode/provider";
import { createComposerSubmissionConfig } from "../../ui/src/v4/composer/composerSubmissionConfig.js";
import {
  conversationTopicFrameSchema,
  type CommandEnvelope,
} from "@zcode/shared/zcode-protocol-v4";

test("Desktop storage preparation and Host service use the real native runtime", async () => {
  const f = await fixture();
  const keys = [
    "ZCODE_AGENT_SERVER_COMMAND",
    "ZCODE_AGENT_SERVER_ARGS_JSON",
    "ZCODE_AGENT_SERVER_RUNTIME",
    "ZCODE_DATA_BASE_DIR",
    "HOME",
  ] as const;
  const previous = Object.fromEntries(keys.map((key) => [key, process.env[key]]));
  Object.assign(process.env, {
    ZCODE_AGENT_SERVER_COMMAND: binary,
    ZCODE_AGENT_SERVER_ARGS_JSON: JSON.stringify([
      "app-server",
      "--stdio",
      "--data-dir",
      f.dataDir,
      "--config",
      f.config,
    ]),
    ZCODE_AGENT_SERVER_RUNTIME: "zcode-cli-rust",
    ZCODE_DATA_BASE_DIR: f.root,
    HOME: f.root,
  });
  const { setDataBaseDir } = await import("../src/paths.js");
  setDataBaseDir(f.root);
  const { prepareSessionStorage } =
    await import("../../desktop/src/host/storagePreparationProcesses.js");
  const { createZCodeAgentService } = await import("../src/zcode-agent/zcodeAgentService.js");
  let accountReads = 0;
  const view: ModelSelectionView = {
    revision: 1,
    providers: [
      {
        providerId: "fixture",
        config: {},
        models: [
          {
            modelId: "core-model",
            config: {
              enabled: true,
              properties: {
                requiresMfjsToolSchema: false,
                contextWindow: 128000,
                inputFormat: {
                  supportsText: true,
                  supportsImage: false,
                  supportsVideo: false,
                  supportsAudio: false,
                  supportsPdf: false,
                },
                outputFormat: { supportsText: true },
                supportsToolCall: true,
                supportsJsonSchemaOutput: false,
                supportsNativeWebSearch: false,
                supportsMidConversationSystem: false,
              },
              optionSpecs: {
                reasoningLevel: { values: ["none"], map: "{}" },
                maxOutputTokens: { max: 8192, map: "{}" },
              },
            },
          },
        ],
      },
    ],
  };
  const service = createZCodeAgentService({
    presentationSurface: "desktop",
    requestTimeoutMs: 5000,
    modelSelectionReadinessSource: { getView: async () => view },
    accountProviderConfigSource: {
      async read() {
        accountReads++;
        throw new Error("Native core must not request an account overlay");
      },
      onDidChange: () => () => {},
    },
  });
  try {
    const phases: string[] = [];
    const observed: string[] = [];
    const preparedPaths = new Set<string>();
    const prepare = (signal = new AbortController().signal) =>
      prepareSessionStorage({
        cwd: f.cwd,
        signal,
        preparedPaths,
        observePath: async (path) => {
          observed.push(path);
        },
        report: (phase) => phases.push(phase),
      });
    await prepare();
    await prepare();
    assert.deepEqual(phases, ["checking", "ready", "checking", "ready"]);
    assert.equal(observed.length, 1);
    assert.match(observed[0]!, /rust-sessions\.sqlite$/);
    await access(join(f.dataDir, "rust-sessions.sqlite"));
    const aborted = new AbortController();
    aborted.abort();
    await assert.rejects(prepare(aborted.signal), /transport_closed/);
    // 保留的 JS Worker 入口也走同一握手，不需要启动完整模型 bundle。
    const worker = join(f.root, "storage-worker.cjs");
    await writeFile(
      worker,
      `
      const lines = require("node:readline").createInterface({ input: process.stdin });
      const emit = (value) => process.stdout.write(JSON.stringify(value) + "\\n");
      emit({ method: "startup/storagePath", params: { path: process.env.FIXTURE_DB_PATH } });
      lines.once("line", (line) => {
        if (JSON.parse(line).method !== "startup/storagePathReady") throw new Error("Bad handshake");
        emit({ method: "startup/storageState", params: { schemaVersion: 1, attemptId: "worker-fixture", sequence: 1, databaseId: "fixture", databaseKind: "session", phase: "ready", elapsedMs: 0 } });
        process.stdout.write(JSON.stringify({ method: "startup/storagePrepared", params: {} }) + "\\n", () => process.exit(0));
      });
    `,
    );
    const workerPhases: string[] = [];
    await prepareSessionStorage({
      cwd: f.cwd,
      signal: new AbortController().signal,
      env: { FIXTURE_DB_PATH: join(f.root, "worker.sqlite") },
      resolveCommand: () => ({
        command: process.execPath,
        storagePreparationEntry: worker,
        supportsStorageStartup: true,
      }),
      observePath: async () => {},
      report: (phase) => workerPhases.push(phase),
    });
    assert.deepEqual(workerPhases, ["ready"]);

    const workspace = { workspacePath: f.cwd };
    assert.equal((await service.initialize(workspace)).available, true);
    assert.deepEqual((await service.readWorkspacePresentation(workspace)).executionCapabilities, {
      permissionModes: ["yolo"],
      independentPlanState: false,
    });
    const envelope = (
      type: CommandEnvelope["type"],
      sessionId: string | null,
      payload = {},
    ): CommandEnvelope => ({
      commandId: randomUUID(),
      clientId: "host-fixture",
      sessionId,
      type,
      payload,
      issuedAt: Date.now(),
    });
    const selection = completeNewModelSelection(view, {
      providerId: "fixture",
      modelId: "core-model",
    });
    const submission = createComposerSubmissionConfig(
      { mode: "yolo", planEnabled: false, modelSelection: selection },
      view,
    );
    assert(submission, "Real App composer must permit the selected model");
    const capabilities = { permissionModes: ["yolo" as const], independentPlanState: false };
    assert.equal(
      createComposerSubmissionConfig({ ...submission, mode: "build" }, view, capabilities),
      null,
    );
    assert.equal(
      createComposerSubmissionConfig({ ...submission, planEnabled: true }, view, capabilities),
      null,
    );
    assert.equal(createComposerSubmissionConfig(submission, view, capabilities)?.mode, "yolo");
    const created = await service.sendConversationCommandV4({
      ...workspace,
      envelope: envelope("createSession", null, { workspaceId: f.cwd, config: submission }),
    });
    assert.equal(created.result?.type, "createSession");
    if (created.result?.type !== "createSession") throw new Error("Session not created");
    const sessionId = created.result.sessionId;
    let timer: NodeJS.Timeout;
    let subscription: { dispose(): void };
    const completed = new Promise<void>((resolve, reject) => {
      timer = setTimeout(
        () => reject(new Error("Host did not receive the completed projection")),
        8000,
      );
      subscription = service.onDynamicConversationFrame(workspace)((wire) => {
        if (wire.kind !== "complete") return;
        const frame = conversationTopicFrameSchema.parse(wire.frame);
        if (
          frame.payload.kind === "deltas" &&
          frame.payload.deltas.some(
            (delta) =>
              delta.op === "state.updated" && delta.patch.control?.phase === "completedSuccess",
          )
        )
          resolve();
      });
    });
    try {
      await service.subscribeConversationV4({ ...workspace, sessionId });
      assert.equal(
        (
          await service.sendConversationCommandV4({
            ...workspace,
            envelope: envelope("sendText", sessionId, { text: "hello", ...submission }),
          })
        ).status,
        "accepted",
      );
      await completed;
    } finally {
      clearTimeout(timer!);
      subscription!.dispose();
    }
    assert.equal(accountReads, 0);
    assert.equal(f.requests.length, 1);
    assert.equal(f.requests[0]!.reasoning_effort, "none");
    await assert.rejects(
      service.sendConversationCommandV4({
        ...workspace,
        envelope: envelope("sendText", sessionId, {
          text: "unsupported",
          ...submission,
          modelSelection: { ...submission.modelSelection, options: { reasoningLevel: "high" } },
        }),
      }),
      /unsupported/i,
    );
  } finally {
    await service.disposeAllAndWait();
    for (const key of keys) {
      const value = previous[key];
      if (value === undefined) delete process.env[key];
      else process.env[key] = value;
    }
    setDataBaseDir(null);
    await f.close();
  }
});
