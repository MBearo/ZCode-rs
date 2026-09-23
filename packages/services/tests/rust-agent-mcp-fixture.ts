import { createServer, type ServerResponse } from "node:http";
import { once } from "node:events";
import { writeFile } from "node:fs/promises";
import { join } from "node:path";
import { zcodeMcpListResultSchema } from "@zcode/shared";
import { type Harness } from "./rust-agent-fixture.js";

export const tools = [
  {
    name: "echo",
    description: "Echo a value",
    inputSchema: { type: "object", properties: { text: { type: "string" } }, required: ["text"] },
    annotations: { readOnlyHint: true, destructiveHint: false },
  },
];
export async function stdioServer(root: string, mode = "normal") {
  const script = join(root, `mcp-${mode}.mjs`);
  const started = join(root, `mcp-${mode}-started`);
  const calls = join(root, `mcp-${mode}-calls`);
  await writeFile(
    script,
    `
import { createInterface } from "node:readline";
import { appendFileSync } from "node:fs";
appendFileSync(${JSON.stringify(started)}, process.pid + "\\n");
const reply = (m,result) => process.stdout.write(JSON.stringify({jsonrpc:"2.0",id:m.id,result})+"\\n");
createInterface({input:process.stdin}).on("line",line=>{
 const m=JSON.parse(line);
 if(m.method==="initialize") { if(${JSON.stringify(mode)}==="slow-connect")return;reply(m,{protocolVersion:"2025-11-25",serverInfo:{name:"fixture",version:"1"},capabilities:{tools:{}}}); }
 else if(m.method==="server/discover")process.stdout.write(JSON.stringify({jsonrpc:"2.0",id:m.id,error:{code:-32601,message:"legacy"}})+"\\n");
 else if(m.method==="tools/list") reply(m,{tools:${JSON.stringify(tools)}});
 else if(m.method==="tools/call") {
  appendFileSync(${JSON.stringify(calls)}, JSON.stringify(m.params)+"\\n");
  if(${JSON.stringify(mode)}==="slow-call")return;
  reply(m,{content:[{type:"text",text:JSON.stringify({echo:m.params.arguments.text,pid:process.pid,probe:process.env.MCP_PROBE})}],isError:m.params.arguments.text==="fail"});
 }
});
`,
  );
  return {
    script,
    started,
    calls,
    config: {
      name: "local.echo",
      command: process.execPath,
      args: [script],
      env: [{ name: "MCP_PROBE", value: "scoped" }],
      protocolVersion: "auto" as const,
      timeoutMs: 5000,
    },
  };
}
export function listMcp(
  h: Harness,
  workspacePath: string,
  mcpServers?: unknown[],
  mode = "connect",
) {
  return h.client.request(
    "mcp/list",
    { workspace: { workspacePath }, ...(mcpServers ? { mcpServers } : {}), mode },
    zcodeMcpListResultSchema,
  );
}
export async function httpServer(transport: "http" | "sse") {
  let stream: ServerResponse | undefined;
  const calls: any[] = [];
  const server = createServer(async (req, res) => {
    if (req.method === "DELETE") {
      res.writeHead(204).end();
      return;
    }
    if (req.method === "GET") {
      if (transport !== "sse") {
        res.writeHead(405).end();
        return;
      }
      stream = res;
      res.writeHead(200, { "Content-Type": "text/event-stream" });
      res.write("event: endpoint\ndata: /messages\n\n");
      return;
    }
    req.setEncoding("utf8");
    let body = "";
    for await (const part of req) body += part;
    const m = JSON.parse(body);
    calls.push({ ...m, headers: req.headers });
    if (m.id === undefined) {
      res.writeHead(202).end();
      return;
    }
    const result =
      m.method === "initialize"
        ? {
            protocolVersion: "2025-11-25",
            serverInfo: { name: "fixture", version: "1" },
            capabilities: { tools: {} },
          }
        : m.method === "tools/list"
          ? { tools }
          : {
              content: [{ type: "text", text: `HTTP ${m.params.arguments.text}` }],
              structuredContent: { echoed: m.params.arguments.text },
            };
    const response = { jsonrpc: "2.0", id: m.id, result };
    if (transport === "sse") {
      res.writeHead(202).end();
      stream!.write(`event: message\ndata: ${JSON.stringify(response)}\n\n`);
    } else {
      res
        .writeHead(200, { "Content-Type": "application/json", "Mcp-Session-Id": "fixture" })
        .end(JSON.stringify(response));
    }
  });
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  const address = server.address();
  if (!address || typeof address === "string") throw new Error("No fixture port");
  return {
    config: {
      name: "remote",
      type: transport,
      url: `http://127.0.0.1:${address.port}/${transport}`,
      headers: [{ name: "X-Fixture", value: "configured" }],
    },
    calls,
    async close() {
      stream?.end();
      server.closeAllConnections();
      await new Promise<void>((resolve) => server.close(() => resolve()));
    },
  };
}
