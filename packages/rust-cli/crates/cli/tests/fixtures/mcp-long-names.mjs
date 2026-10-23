#!/usr/bin/env node
// mcp_tool_names.rs 的 stdio MCP 服务器：一行一条 JSON-RPC 消息。列出一个普通工具与一个名字太长的工具
// （`mcp__names__` 加 120 个字符，超过 128）。不读写文件。
import { createInterface } from "node:readline";

const TOOLS = [
  {
    name: "lookup",
    description: "Look up the value stored under a key.",
    inputSchema: { type: "object", properties: { key: { type: "string" } } },
  },
  {
    name: "x".repeat(120),
    description: "A tool whose full name is too long for the API.",
    inputSchema: { type: "object", properties: {} },
  },
];

function result(method, params) {
  switch (method) {
    case "initialize":
      return {
        protocolVersion: params?.protocolVersion ?? "2025-06-18",
        capabilities: { tools: {} },
        serverInfo: { name: "long-names", version: "1.0.0" },
      };
    case "tools/list":
      return { tools: TOOLS };
    case "tools/call":
      return { content: [{ type: "text", text: `value for ${params?.arguments?.key}` }] };
    default:
      return undefined;
  }
}

const lines = createInterface({ input: process.stdin });
lines.on("line", (line) => {
  let message;
  try {
    message = JSON.parse(line);
  } catch {
    return;
  }
  if (message.id === undefined) return;
  const value = result(message.method, message.params);
  const reply =
    value === undefined
      ? { jsonrpc: "2.0", id: message.id, error: { code: -32601, message: "Method not found" } }
      : { jsonrpc: "2.0", id: message.id, result: value };
  process.stdout.write(`${JSON.stringify(reply)}\n`);
});
