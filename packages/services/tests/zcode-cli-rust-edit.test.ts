import assert from "node:assert/strict";
import test from "node:test";
import { readFile, realpath, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { end, event, fixture } from "./zcode-cli-rust-fixture.js";

type Message = Record<string, any>;

/** Plays `calls` one per model step, then answers with text. */
function scripted(calls: [string, Message][]) {
  return fixture({
    respond(request, response) {
      response.writeHead(200, { "Content-Type": "text/event-stream" });
      const step = (request.messages as Message[]).filter((m) => m.role === "tool").length;
      const call = calls[step];
      if (!call) {
        event(response, { content: "done" });
        end(response, "stop");
        return;
      }
      const [name, args] = call;
      const function_ = { name, arguments: JSON.stringify(args) };
      event(response, {
        tool_calls: [{ index: 0, id: `call-${step}`, type: "function", function: function_ }],
      });
      end(response, "tool_calls");
    },
  });
}
const toolResults = (requests: Message[]) =>
  (requests.at(-1)!.messages as Message[]).filter((m) => m.role === "tool").map((m) => m.content);

test("Rust Edit falls back like Node and keeps the file's quote style", async () => {
  const f = await scripted([
    ["Read", { file_path: "notes.txt" }],
    ["Edit", { file_path: "notes.txt", old_string: 'say "hi"', new_string: 'say "bye"' }],
    ["Edit", { file_path: "notes.txt", old_string: "keep", new_string: "" }],
  ]);
  try {
    await writeFile(join(f.cwd, "notes.txt"), "say “hi”\r\nkeep\r\nend\r\n");
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    await h.command(h.envelope("sendText", id, { text: "edit" }));
    await h.completed(id);
    assert.equal(await readFile(join(f.cwd, "notes.txt"), "utf8"), "say “bye”\r\nend\r\n");
    const suffix = " (file state is current in your context — no need to Read it back)";
    assert.deepEqual(toolResults(f.requests).slice(1), [
      `The file notes.txt has been updated successfully.${suffix}`,
      `The file notes.txt has been updated successfully.${suffix}`,
    ]);
  } finally {
    await f.close();
  }
});

test("Rust Edit reports Node's failures", async () => {
  const f = await scripted([
    ["Edit", { file_path: "notes.txt", old_string: "a", new_string: "a" }],
    ["Edit", { file_path: "notes.txt", old_string: "a", new_string: "b" }],
    ["Read", { file_path: "notes.txt" }],
    ["Edit", { file_path: "notes.txt", old_string: "zz", new_string: "b" }],
    ["Edit", { file_path: "notes.txt", old_string: "x", new_string: "y" }],
    ["Edit", { file_path: "note.txt", old_string: "x", new_string: "y" }],
  ]);
  try {
    await writeFile(join(f.cwd, "notes.txt"), "x x\n");
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    await h.command(h.envelope("sendText", id, { text: "edit" }));
    await h.completed(id);
    const [same, unread, , missing, ambiguous, absent] = toolResults(f.requests);
    assert.ok(same.includes("No changes to make: old_string and new_string are exactly the same."));
    assert.ok(unread.includes("File has not been read yet. Read it first before writing to it."));
    assert.ok(missing.includes("String to replace not found in file.\nString: zz"));
    assert.ok(ambiguous.includes("Found 2 matches of the string to replace"));
    assert.ok(
      absent.includes(
        // 工作目录为物理路径（与 Node 的 process.cwd() 一致）。
        `File does not exist. Note: your current working directory is ${await realpath(f.cwd)}. Did you mean notes.txt?`,
      ),
      absent,
    );
  } finally {
    await f.close();
  }
});
