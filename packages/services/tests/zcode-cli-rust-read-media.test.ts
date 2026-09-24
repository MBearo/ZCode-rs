import assert from "node:assert/strict";
import test from "node:test";
import { writeFile } from "node:fs/promises";
import { join } from "node:path";
import { crc32, deflateSync } from "node:zlib";
import type { ServerResponse } from "node:http";
import { end, event, fixture } from "./zcode-cli-rust-fixture.js";
import { sse } from "./zcode-cli-rust-protocol-fixture.js";

type Message = Record<string, any>;
/** A valid 2×2 RGB PNG (the tool decodes it, so the bytes must be well-formed). */
function pngImage(): Buffer {
  const chunk = (type: string, data: Buffer) => {
    const length = Buffer.alloc(4);
    length.writeUInt32BE(data.length);
    const body = Buffer.concat([Buffer.from(type, "ascii"), data]);
    const crc = Buffer.alloc(4);
    crc.writeUInt32BE(crc32(body));
    return Buffer.concat([length, body, crc]);
  };
  const header = Buffer.alloc(13);
  header.writeUInt32BE(2, 0);
  header.writeUInt32BE(2, 4);
  header[8] = 8;
  header[9] = 2;
  const rows = Buffer.from([0, 255, 0, 0, 0, 255, 0, 0, 0, 0, 255, 255, 255, 255]);
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk("IHDR", header),
    chunk("IDAT", deflateSync(rows)),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}
const png = pngImage();
const read = (file: string) => JSON.stringify({ file_path: file });

function chatCall(res: ServerResponse, file: string) {
  res.writeHead(200, { "content-type": "text/event-stream" });
  event(res, {
    tool_calls: [
      {
        index: 0,
        id: "call-read",
        type: "function",
        function: { name: "Read", arguments: read(file) },
      },
    ],
  });
  end(res, "tool_calls");
}
function chatText(res: ServerResponse) {
  res.writeHead(200, { "content-type": "text/event-stream" });
  event(res, { content: "seen" });
  end(res, "stop");
}
function anthropicMessage(res: ServerResponse, block: Message, deltas: Message[], stop: string) {
  res.writeHead(200, { "content-type": "text/event-stream" });
  sse(res, {
    type: "message_start",
    message: {
      id: "m",
      role: "assistant",
      content: [],
      usage: { input_tokens: 3, output_tokens: 1 },
    },
  });
  sse(res, { type: "content_block_start", index: 0, content_block: block });
  for (const delta of deltas) sse(res, { type: "content_block_delta", index: 0, delta });
  sse(res, { type: "content_block_stop", index: 0 });
  sse(res, { type: "message_delta", delta: { stop_reason: stop }, usage: { output_tokens: 2 } });
  sse(res, { type: "message_stop" });
  res.end();
}

async function run(options: {
  anthropic: boolean;
  inputFormat: Message;
  file: string;
  bytes: Buffer;
}) {
  const f = await fixture({
    config: {
      ...(options.anthropic ? { apiType: "anthropic-messages", reasoningParameters: {} } : {}),
      formatProperties: { inputFormat: options.inputFormat, outputFormat: { supportsText: true } },
    },
    respond(req, res) {
      const first = f.requests.length === 1;
      if (options.anthropic) {
        if (first) {
          anthropicMessage(
            res,
            { type: "tool_use", id: "call-read", name: "Read", input: {} },
            [{ type: "input_json_delta", partial_json: read(options.file) }],
            "tool_use",
          );
        } else {
          anthropicMessage(
            res,
            { type: "text", text: "" },
            [{ type: "text_delta", text: "seen" }],
            "end_turn",
          );
        }
        return;
      }
      if (first) chatCall(res, options.file);
      else chatText(res);
      void req;
    },
  });
  await writeFile(join(f.cwd, options.file), options.bytes);
  const h = f.start();
  const id = await h.create();
  await h.subscribe(`conversation/${id}`);
  const after = h.messages.length;
  await h.command(h.envelope("sendText", id, { text: "look" }));
  await h.completed(id, after);
  const rows: Message[] = (await h.rows(id)).rows;
  const row = rows.find((r) => r.kind === "toolCall")!;
  return { f, h, final: f.requests.at(-1)!, row };
}

test("Rust Read returns images as tool-result blocks on Anthropic", async () => {
  const { f, final, row } = await run({
    anthropic: true,
    inputFormat: { supportsText: true, supportsImage: true },
    file: "dot.png",
    bytes: png,
  });
  try {
    const result = final.messages.at(-1).content.find((b: Message) => b.type === "tool_result");
    assert.equal(result.is_error, undefined);
    assert.deepEqual(result.content, [
      {
        type: "image",
        source: { type: "base64", media_type: "image/png", data: png.toString("base64") },
      },
    ]);
    assert.equal(row.output.text, "[Attached image/png: Read image]");
  } finally {
    await f.close();
  }
});

test("Rust Read sends image media after the tool message on Chat and omits unsupported media", async () => {
  const supported = await run({
    anthropic: false,
    inputFormat: { supportsText: true, supportsImage: true },
    file: "dot.png",
    bytes: png,
  });
  try {
    const messages = supported.final.messages;
    const tool = messages.findIndex((m: Message) => m.role === "tool");
    assert.equal(messages[tool].content, "[Attached image/png: Read image]");
    assert.deepEqual(messages[tool + 1], {
      role: "user",
      content: [
        { type: "text", text: "Tool result media from Read:" },
        {
          type: "image_url",
          image_url: { url: `data:image/png;base64,${png.toString("base64")}` },
        },
      ],
    });
  } finally {
    await supported.f.close();
  }
  const unsupported = await run({
    anthropic: false,
    inputFormat: { supportsText: true, supportsImage: false },
    file: "dot.png",
    bytes: png,
  });
  try {
    const messages = unsupported.final.messages;
    const tool = messages.findIndex((m: Message) => m.role === "tool");
    assert.equal(
      messages[tool].content,
      "[Attached image/png: Read image]\n[Media omitted from provider request because the selected model does not support image input.]",
    );
    assert.equal(messages.length, tool + 1);
  } finally {
    await unsupported.f.close();
  }
});

test("Rust Read defers video after the tool result on every protocol", async () => {
  const video = Buffer.from("fake mp4 bytes");
  const { f, final } = await run({
    anthropic: true,
    inputFormat: { supportsText: true, supportsVideo: true },
    file: "clip.mp4",
    bytes: video,
  });
  try {
    const blocks = final.messages.at(-1).content;
    const result = blocks.find((b: Message) => b.type === "tool_result");
    assert.deepEqual(result.content, [{ type: "text", text: "[Attached video/mp4: Read video]" }]);
    const index = blocks.indexOf(result);
    assert.deepEqual(blocks.slice(index + 1), [
      { type: "text", text: "Tool result media from Read:" },
      {
        type: "video",
        source: { type: "base64", media_type: "video/mp4", data: video.toString("base64") },
      },
    ]);
  } finally {
    await f.close();
  }
});
