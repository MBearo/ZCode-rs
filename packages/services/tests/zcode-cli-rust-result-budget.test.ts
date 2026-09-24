import assert from "node:assert/strict";
import test from "node:test";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { event, end, fixture } from "./zcode-cli-rust-fixture.js";

type Message = Record<string, any>;
const BIG_TEXT_BYTES = 60_000;

/** 第一轮调用返回超大文本的 MCP 工具，第二轮调用 Grep，第三轮结束。 */
function respond(request: Message, response: any) {
  response.writeHead(200, { "Content-Type": "text/event-stream" });
  const toolMessages = request.messages.filter((m: Message) => m.role === "tool").length;
  const call = (id: string, name: string, args: Message) => {
    event(response, {
      tool_calls: [
        { index: 0, id, type: "function", function: { name, arguments: JSON.stringify(args) } },
      ],
    });
    end(response, "tool_calls");
  };
  if (toolMessages === 0) {
    const name = request.tools.find((t: Message) => t.function.name.startsWith("mcp__"));
    call("mcp-call", name.function.name, {});
  } else if (toolMessages === 1) {
    call("grep-call", "Grep", { pattern: "needle", output_mode: "content" });
  } else {
    event(response, { content: "done" });
    end(response, "stop");
  }
}

async function bigMcpServer(root: string) {
  const script = join(root, "big-mcp.mjs");
  await writeFile(
    script,
    `
import { createInterface } from "node:readline";
const reply=(m,result)=>process.stdout.write(JSON.stringify({jsonrpc:"2.0",id:m.id,result})+"\\n");
createInterface({input:process.stdin}).on("line",line=>{const m=JSON.parse(line);
 if(m.method==="initialize")reply(m,{protocolVersion:"2025-11-25",serverInfo:{name:"big",version:"1"},capabilities:{tools:{}}});
 else if(m.method==="tools/list")reply(m,{tools:[{name:"dump",description:"Dump",inputSchema:{type:"object"}}]});
 else if(m.method==="tools/call")reply(m,{content:[{type:"text",text:"x".repeat(${BIG_TEXT_BYTES})}]});});
`,
  );
  return { name: "big", command: process.execPath, args: [script], env: [], timeoutMs: 5000 };
}

test("Rust bounds oversized tool results with Node's result budgets", async () => {
  const f = await fixture({ respond });
  try {
    const server = await bigMcpServer(f.root);
    const lines = Array.from({ length: 400 }, (_, i) => `needle ${i} ${"y".repeat(200)}`);
    await mkdir(join(f.cwd, "src"), { recursive: true });
    await writeFile(join(f.cwd, "src/haystack.txt"), `${lines.join("\n")}\n`);
    const h = f.start();
    const ack = await h.command(
      h.envelope("createSession", null, { workspaceId: f.cwd, mcpServers: [server] }),
    );
    assert(ack.result, JSON.stringify(ack));
    const sid = (ack.result as { sessionId: string }).sessionId;
    await h.subscribe(`conversation/${sid}`);
    await h.command(h.envelope("sendText", sid, { text: "use the tools" }));
    await h.completed(sid);

    // MCP：有效上限 50 000 字节，截断说明计入上限。
    const mcp = f.requests[1]!.messages.at(-1).content as string;
    assert.equal(Buffer.byteLength(mcp), 50_000);
    assert.match(
      mcp,
      /\n\n\[Tool output truncated by resultBudget: originalBytes=\d+, maxModelBytes=50000, strategy=truncate\]$/,
    );

    // Grep：超过 20 000 字节时落盘，模型只看到信封与预览。
    const grep = f.requests[2]!.messages.at(-1).content as string;
    const header = grep.match(
      /^<persisted-output>\nOutput too large \((\d+) KB\)\. Full output saved to: (.+-tool-result-[0-9a-f-]+\.json)\n\nPreview \(first 2 KB\):\n/,
    );
    assert(header, grep.slice(0, 200));
    assert(grep.endsWith("\n...\n</persisted-output>"));
    const saved = await readFile(header[2]!, "utf8");
    assert(Buffer.byteLength(saved) > 20_000);
    assert(saved.includes("needle 0 ") && grep.includes(saved.slice(0, 100)));
    assert.equal(Number(header[1]), Math.round(Buffer.byteLength(saved) / 1000));
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});
