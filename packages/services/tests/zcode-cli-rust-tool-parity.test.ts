import { GlobOutputSchema } from "../../../apps/zcode-cli/packages/contracts/src/tools/glob.js";
import { GrepOutputSchema } from "../../../apps/zcode-cli/packages/contracts/src/tools/grep.js";
import { WriteOutputSchema } from "../../../apps/zcode-cli/packages/contracts/src/tools/write.js";
import { EditOutputSchema } from "../../../apps/zcode-cli/packages/contracts/src/tools/edit.js";
import assert from "node:assert/strict";
import test from "node:test";
import { spawn } from "node:child_process";
import { createInterface } from "node:readline";
import { mkdtemp, rm, writeFile, readFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { once } from "node:events";
import { NodeFileSystemAdapter } from "../../../apps/zcode-cli/packages/adapters/src/fs/index.js";
import { readTextFileForModel } from "../../../apps/zcode-cli/packages/core/src/tool/handlers/read-text.js";
import { globToolEntry } from "../../../apps/zcode-cli/packages/core/src/tool/handlers/glob.js";
import { grepToolEntry } from "../../../apps/zcode-cli/packages/core/src/tool/handlers/grep.js";
import { writeToolEntry } from "../../../apps/zcode-cli/packages/core/src/tool/handlers/write.js";
import { editToolEntry } from "../../../apps/zcode-cli/packages/core/src/tool/handlers/edit.js";
import type { ToolExecutionContext } from "../../../apps/zcode-cli/packages/core/src/tool/types.js";
import {
  ReadOutputSchema,
  ReadInputJsonSchema,
} from "../../../apps/zcode-cli/packages/contracts/src/tools/read.js";
import {
  BashInputJsonSchema,
  BashOutputSchema,
} from "../../../apps/zcode-cli/packages/contracts/src/tools/bash.js";
import { TaskOutputInputJsonSchema } from "../../../apps/zcode-cli/packages/contracts/src/tools/task-output.js";
import { TaskStopInputJsonSchema } from "../../../apps/zcode-cli/packages/contracts/src/tools/task-stop.js";
import { AskUserQuestionInputJsonSchema } from "../../../apps/zcode-cli/packages/contracts/src/tools/ask-user-question.js";
import { SkillInputJsonSchema } from "../../../apps/zcode-cli/packages/contracts/src/tools/skill.js";
import { AgentInputJsonSchema } from "../../../apps/zcode-cli/packages/contracts/src/tools/agent.js";
import { SendMessageInputJsonSchema } from "../../../apps/zcode-cli/packages/contracts/src/tools/send-message.js";
import { skillToolEntry } from "../../../apps/zcode-cli/packages/core/src/tool/handlers/skill.js";
import {
  TodoReadInputJsonSchema,
  TodoWriteInputJsonSchema,
} from "../../../apps/zcode-cli/packages/contracts/src/tools/todo.js";
import {
  todoReadToolEntry,
  todoWriteToolEntry,
} from "../../../apps/zcode-cli/packages/core/src/tool/handlers/todo.js";
import { askUserQuestionToolEntry } from "../../../apps/zcode-cli/packages/core/src/tool/handlers/ask-user-question.js";

async function driver() {
  const root = await mkdtemp(join(tmpdir(), "native-tools-parity-"));
  const child = spawn(
    resolve(
      `apps/zcode-cli-rust/target/debug/examples/tool_fixture${process.platform === "win32" ? ".exe" : ""}`,
    ),
    [root],
  );
  let errors = "";
  child.stderr.on("data", (s) => {
    errors += s;
  });
  const lines = createInterface({ input: child.stdout });
  const call = async (name: string, args: unknown, session = "fixture"): Promise<any> => {
    const next = once(lines, "line");
    child.stdin.write(
      JSON.stringify(name === "definitions" ? { definitions: true } : { name, args, session }) +
        "\n",
    );
    return JSON.parse((await next)[0]);
  };
  return {
    root,
    call,
    async close() {
      const closed = once(child, "close");
      child.stdin.end();
      assert.deepEqual(await closed, [0, null], errors);
      lines.close();
      await rm(root, { recursive: true, force: true });
    },
  };
}

test("Rust tool definitions use current TS schemas and real TS file/search handlers agree on representative calls", async () => {
  const d = await driver();
  try {
    const adapter = new NodeFileSystemAdapter();
    const context = {
      workingDirectory: d.root,
      workspaceRoot: d.root,
      sessionId: "fixture",
      toolCallId: "fixture",
      traceId: "fixture",
      abortSignal: new AbortController().signal,
      fileSystemPort: adapter,
      readFileState: new Map(),
    } as unknown as ToolExecutionContext;
    const entries = {
      Write: writeToolEntry,
      Edit: editToolEntry,
      Glob: globToolEntry,
      Grep: grepToolEntry,
    };
    const schemas = {
      Agent: AgentInputJsonSchema,
      SendMessage: SendMessageInputJsonSchema,
      Skill: SkillInputJsonSchema,
      TodoRead: TodoReadInputJsonSchema,
      TodoWrite: TodoWriteInputJsonSchema,
      Read: ReadInputJsonSchema,
      Bash: BashInputJsonSchema,
      TaskOutput: TaskOutputInputJsonSchema,
      TaskStop: TaskStopInputJsonSchema,
      AskUserQuestion: AskUserQuestionInputJsonSchema,
      ...Object.fromEntries(
        Object.entries(entries).map(([key, entry]) => [key, entry.inputSchema]),
      ),
    };
    for (const { function: tool } of (await d.call("definitions", {})).definitions) {
      assert.deepEqual(tool.parameters, schemas[tool.name as keyof typeof schemas]);
      if (tool.name === "TodoRead" || tool.name === "TodoWrite")
        assert.equal(
          tool.description,
          (tool.name === "TodoRead" ? todoReadToolEntry : todoWriteToolEntry).metadata.description,
        );
      if (tool.name === "AskUserQuestion")
        assert.equal(tool.description, askUserQuestionToolEntry.metadata.description);
      if (tool.name === "Skill")
        assert.equal(tool.description, skillToolEntry.metadata.description);
    }
    await writeFile(join(d.root, "a.txt"), "first\nhello 世界\nlast\n");
    for (const args of [{}, { offset: 2, limit: 1 }, { offset: 99 }]) {
      const rust = await d.call("Read", { file_path: join(d.root, "a.txt"), ...args });
      ReadOutputSchema.parse(rust.data);
      const ts = await readTextFileForModel({
        filePath: join(d.root, "a.txt"),
        fileSystemPort: adapter,
        ...args,
      });
      for (const key of ["content", "startLine", "numLines", "totalLines"] as const)
        assert.equal(rust.data[key], ts[key], `Read ${key} ${JSON.stringify(args)}`);
    }
    for (const [name, args] of [
      ["Glob", { pattern: "*.txt" }],
      ["Grep", { pattern: "hello", output_mode: "content" }],
      ["Grep", { pattern: "first\\nhello", multiline: true }],
      ["Grep", { pattern: "first|last", output_mode: "content", offset: 1, head_limit: 1 }],
    ] as const) {
      const entry = entries[name];
      const rust = await d.call(name, args);
      (name === "Glob" ? GlobOutputSchema : GrepOutputSchema).parse(rust.data);
      const ts = (await entry.handler(args, context)) as any;
      for (const key of ["filenames", "content", "numFiles", "truncated"])
        assert.deepEqual(rust.data[key], ts[key], `${name} ${key}`);
    }
    const tsPath = join(d.root, "ts.txt");
    const rustPath = join(d.root, "rust.txt");
    const normalize = (value: any) => {
      const result = { ...value };
      delete result.filePath;
      delete result.perf;
      delete result.gitDiff;
      return result;
    };
    const tsWrite = await writeToolEntry.handler(
      { file_path: tsPath, content: "alpha\nbeta\n" },
      context,
    );
    const nativeWrite = await d.call("Write", { file_path: rustPath, content: "alpha\nbeta\n" });
    WriteOutputSchema.parse(nativeWrite.data);
    assert.equal(nativeWrite.data.type, (tsWrite as any).type);
    for (const replaceAll of [false, true]) {
      const args = {
        old_string: replaceAll ? "alpha" : "beta",
        new_string: replaceAll ? "new" : "alpha",
        replace_all: replaceAll,
      };
      const ts = (await editToolEntry.handler({ file_path: tsPath, ...args }, context)) as any;
      const native = await d.call("Edit", { file_path: rustPath, ...args });
      EditOutputSchema.parse(native.data);
      assert.equal(await readFile(rustPath, "utf8"), await readFile(tsPath, "utf8"));
      const a = normalize(native.data),
        b = normalize(ts);
      delete a.structuredPatch;
      delete b.structuredPatch;
      assert.deepEqual(a, b);
    }
    if (process.platform !== "win32") {
      for (const [command, status] of [
        ["printf hello", "completed"],
        ["exit 7", "failed"],
        ["sleep 1", "timed_out"],
      ]) {
        const native = await d.call("Bash", {
          command,
          timeout: status === "timed_out" ? 10 : 1000,
        });
        BashOutputSchema.parse(native.data);
        assert.equal(native.data.status, status);
      }
    }
  } finally {
    await d.close();
  }
});
