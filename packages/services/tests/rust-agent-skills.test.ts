import assert from "node:assert/strict";
import test from "node:test";
import { mkdir, writeFile, symlink } from "node:fs/promises";
import { dirname, join } from "node:path";
import { zcodeSkillsReferenceCatalogResultSchema } from "@zcode/shared";
import { end, event, fixture, type Harness } from "./rust-agent-fixture.js";

async function put(path: string, body: string) {
  await mkdir(dirname(path), { recursive: true });
  await writeFile(path, body);
}
function catalog(h: Harness, workspacePath: string, sessionId?: string) {
  return h.client.request(
    "skills/referenceCatalog",
    { workspace: { workspacePath }, ...(sessionId ? { sessionId } : {}) },
    zcodeSkillsReferenceCatalogResultSchema,
  );
}

test("Skill catalog freezes with session context, loads qualified plugin content and survives restart", async () => {
  const f = await fixture({
    respond(request, response) {
      response.writeHead(200, { "Content-Type": "text/event-stream" });
      if (request.messages.at(-1).role !== "tool") {
        event(response, {
          tool_calls: [
            {
              index: 0,
              id: "skill-call",
              type: "function",
              function: { name: "Skill", arguments: JSON.stringify({ skill: "demo:review" }) },
            },
          ],
        });
        end(response, "tool_calls");
      } else {
        event(response, { content: "skill loaded" });
        end(response, "stop");
      }
    },
  });
  try {
    const plugin = join(f.root, "plugin");
    const source = join(plugin, "skills/review/SKILL.md");
    await put(
      join(plugin, ".zcode-plugin/plugin.json"),
      JSON.stringify({ name: "demo", version: "1" }),
    );
    await put(
      source,
      "---\nname: review\ndescription: >-\n  Review the\n  current changes.\n---\nUse ${ZCODE_SKILL_DIR}/references/check.md and ${CLAUDE_SKILL_DIR}/scripts/test.sh.",
    );
    await put(join(f.cwd, ".zcode/config.json"), JSON.stringify({ plugins: { dirs: [plugin] } }));
    await put(
      join(f.root, ".agents/skills/review/SKILL.md"),
      "---\nname: review\ndescription: user review\n---\nUser content",
    );
    const h = f.start();
    const initial = await catalog(h, f.cwd);
    assert.equal(initial.authority, "workspace");
    assert.equal(initial.skills.filter((s) => s.name === "review").length, 2);
    assert.equal(
      initial.skills.find((s) => s.pluginName === "demo")?.description,
      "Review the current changes.",
    );
    const sid = await h.create();
    await h.subscribe(`conversation/${sid}`);
    assert.equal((await catalog(h, f.cwd, sid)).authority, "session");
    await put(join(f.cwd, ".agents/skills/new/SKILL.md"), "New plain Markdown skill");
    assert.ok((await catalog(h, f.cwd)).skills.some((s) => s.name === "new"));
    assert.ok(!(await catalog(h, f.cwd, sid)).skills.some((s) => s.name === "new"));
    await h.command(h.envelope("sendText", sid, { text: "use review" }));
    await h.completed(sid);
    assert.ok(f.requests[0]!.tools.some((t: any) => t.function.name === "Skill"));
    assert.match(JSON.stringify(f.requests[0]!.messages), /demo:review/);
    const output = f.requests[1]!.messages.at(-1).content;
    assert.match(output, /<skill_content name="demo:review">/);
    assert.ok(output.includes(`${dirname(source)}/references/check.md`));
    assert.ok(!output.includes("${ZCODE_SKILL_DIR}"));
    await h.close();
    const cold = f.start();
    await cold.subscribe(`conversation/${sid}`);
    assert.deepEqual((await catalog(cold, f.cwd, sid)).skills, initial.skills);
    await assert.rejects(catalog(cold, f.cwd, "unknown-session"));
    assert.deepEqual(cold.schemaErrors, []);
    await cold.close();
  } finally {
    await f.close();
  }
});

test("Skill discovery honors switches, disabled canonical paths, malformed metadata and plugin symlinks", async () => {
  const f = await fixture();
  try {
    const plugin = join(f.root, "plugin");
    const outside = join(f.root, "outside");
    await put(join(plugin, ".codex-plugin/plugin.json"), JSON.stringify({ name: "untrusted" }));
    await put(join(outside, "SKILL.md"), "---\nname: escape\ndescription: excluded\n---\nsecret");
    await mkdir(join(plugin, "skills"));
    await symlink(outside, join(plugin, "skills/escape"), "dir");
    await put(
      join(f.cwd, ".agents/skills/bad/SKILL.md"),
      "---\nname: no-description\n---\ninvalid",
    );
    await mkdir(join(f.cwd, ".agents/skills/linked"));
    await symlink(join(outside, "SKILL.md"), join(f.cwd, ".agents/skills/linked/SKILL.md"));
    const config = join(f.cwd, ".zcode/config.json");
    await put(
      config,
      JSON.stringify({
        plugins: { dirs: [plugin] },
        skill: { [join(outside, "SKILL.md")]: { enable: false } },
      }),
    );
    const h = f.start();
    assert.deepEqual((await catalog(h, f.cwd)).skills, []);
    await put(join(f.cwd, ".zcode/skills/plain/SKILL.md"), "plain content");
    assert.equal((await catalog(h, f.cwd)).skills.length, 1);
    await put(config, JSON.stringify({ features: { skill: false } }));
    assert.deepEqual((await catalog(h, f.cwd)).skills, []);
    await h.close();
  } finally {
    await f.close();
  }
});
