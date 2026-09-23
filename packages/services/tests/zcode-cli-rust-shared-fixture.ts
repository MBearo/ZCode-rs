import { createHash } from "node:crypto";
import { zcodeSessionStateSnapshotSchema } from "@zcode/shared";
import type { Harness } from "./zcode-cli-rust-fixture.js";
export const markdown =
  "# Shared conversation: fixture\n\n## User\n\nSHARED_CONTEXT_SECRET\n\n## Assistant\n\nUse the installed artifact.";
export function sharedHistory(contextId = "context-A", status = "pending") {
  return {
    source: "sharedContext",
    title: "Imported fixture",
    markdown,
    createdAt: 1234,
    provenance: {
      shareId: "share-A",
      contextId,
      shareUrl: "https://example.com/cn/share/fixture",
      status,
      projectionSha256: "a".repeat(64),
      artifactSetSha256: "b".repeat(64),
      markdownSha256: createHash("sha256").update(markdown).digest("hex"),
      formatterVersion: 1,
      installedArtifacts: [
        { artifactId: "artifact-A", workspaceRelativePath: ".zcode/imports/fixture/note.txt" },
      ],
    },
  };
}
export function importShared(
  h: Harness,
  cwd: string,
  sessionId = "shared-A",
  history = sharedHistory(),
) {
  return h.client.request(
    "session/create",
    {
      sessionId,
      workspace: { workspacePath: cwd, workspaceIdentity: h.workspace, workspaceKey: h.workspace },
      persistence: "immediate",
      importedHistory: history,
    },
    zcodeSessionStateSnapshotSchema,
  );
}
export const ref = (id = "context-A") => [{ kind: "shared_context_import", context_id: id }];
export async function sharedSnapshot(
  h: Harness,
  id = "shared-A",
  connectionId = "fixture-desktop",
  clientMode = "desktop-continuous",
) {
  const before = h.messages.length;
  const sub = await h.subscribe(`conversation/${id}`, connectionId, clientMode);
  return (
    await h.wait(
      (m) =>
        m.params?.subscriptionId === sub.ack.subscriptionId &&
        m.params?.frame?.payload?.kind === "snapshot",
      before,
    )
  ).params.frame.payload.snapshot;
}
