import assert from "node:assert/strict";
import test from "node:test";
import {
  zcodeWorkspacePresentationSchema,
  supportsRuntimeExecution,
  runtimeExecutionCapabilitiesSchema,
} from "@zcode/shared";
import { fixture } from "./rust-agent-fixture.js";

const native = runtimeExecutionCapabilitiesSchema.parse({
  permissionModes: ["yolo"],
  independentPlanState: false,
});

test("Execution capabilities gate permissions and Plan without changing the user's intent", () => {
  const build = { mode: "build", planEnabled: false };
  assert.equal(supportsRuntimeExecution(build, native), false);
  assert.deepEqual(build, { mode: "build", planEnabled: false });
  assert.equal(supportsRuntimeExecution({ mode: "yolo", planEnabled: false }, native), true);
  assert.equal(supportsRuntimeExecution({ mode: "yolo", planEnabled: true }, native), false);
  assert.equal(supportsRuntimeExecution({ mode: "plan" }, native), false);
  assert.equal(supportsRuntimeExecution(build), true);
  assert.equal(supportsRuntimeExecution({ mode: "plan" }), true);
  assert.equal(
    runtimeExecutionCapabilitiesSchema.safeParse({ ...native, permissionModes: [] }).success,
    false,
  );
  assert.equal(
    runtimeExecutionCapabilitiesSchema.safeParse({ ...native, permissionModes: ["plan"] }).success,
    false,
  );
});

test("Native workspace presentation and both delivery subscriptions expose the same yolo-only capability", async () => {
  const f = await fixture();
  try {
    const h = f.start();
    const presentation = await h.client.request(
      "workspace/readPresentation",
      {
        workspace: { workspacePath: f.cwd, workspaceKey: f.cwd },
        includeExecutionCapabilities: true,
      },
      zcodeWorkspacePresentationSchema,
    );
    assert.deepEqual(presentation.executionCapabilities, native);
    const legacy = await h.client.request(
      "workspace/readPresentation",
      {
        workspace: { workspacePath: f.cwd, workspaceKey: f.cwd },
      },
      zcodeWorkspacePresentationSchema.omit({ executionCapabilities: true }).strict(),
    );
    assert.equal("executionCapabilities" in legacy, false);
    for (const delivery of ["desktop-continuous", "web-remote-replayable"]) {
      const ack = await h.subscribe(`workspace-config/${f.cwd}`, `modes-${delivery}`, delivery);
      const frame = await h.wait(
        (m) =>
          m.params?.subscriptionId === ack.ack.subscriptionId &&
          m.params?.topic === `workspace-config/${f.cwd}`,
      );
      assert.deepEqual(frame.params.frame.payload.snapshot.config.executionCapabilities, native);
    }
    assert.equal(f.requests.length, 0);
    await assert.rejects(
      h.command(
        h.envelope("createSession", null, {
          workspaceId: f.cwd,
          config: { mode: "build" },
          firstInput: { text: "never execute" },
        }),
      ),
      /Unsupported core execution mode/,
    );
    assert.equal(f.requests.length, 0);
  } finally {
    await f.close();
  }
});
