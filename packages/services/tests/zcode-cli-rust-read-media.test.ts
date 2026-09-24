import assert from "node:assert/strict";
import test from "node:test";
import { chmod, mkdir, mkdtemp, realpath, writeFile } from "node:fs/promises";
import { delimiter, join } from "node:path";
import { tmpdir } from "node:os";
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
const read = (file: string, extra: Message = {}) => JSON.stringify({ file_path: file, ...extra });

function chatCall(res: ServerResponse, args: string) {
  res.writeHead(200, { "content-type": "text/event-stream" });
  event(res, {
    tool_calls: [
      {
        index: 0,
        id: "call-read",
        type: "function",
        function: { name: "Read", arguments: args },
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
  args?: Message;
  env?: Record<string, string>;
}) {
  const args = read(options.file, options.args);
  const f = await fixture({
    env: options.env,
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
            [{ type: "input_json_delta", partial_json: args }],
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
      if (first) chatCall(res, args);
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
  return { f, h, first: f.requests[0]!, final: f.requests.at(-1)!, row };
}

/** The final request's Read result: Anthropic blocks, or the Chat tool message text. */
function toolResult(final: Message): any {
  const last = final.messages.at(-1);
  if (Array.isArray(last.content)) {
    return last.content.find((b: Message) => b.type === "tool_result").content;
  }
  return final.messages.find((m: Message) => m.role === "tool").content;
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

const pdf = Buffer.from("%PDF-1.4\n1 0 obj << >> endobj\n%%EOF\n");
const pdfFormat = { supportsText: true, supportsImage: true, supportsPdf: true };

/** Fake Poppler tools: pdfinfo reports 12 pages for big.pdf; pdftoppm copies a PNG per page. */
async function poppler(page: Buffer) {
  const bin = join(await mkdtemp(join(tmpdir(), "zcode-poppler-")), "bin");
  await mkdir(bin);
  await writeFile(join(bin, "page.jpg"), page);
  const scripts: Record<string, string> = {
    pdfinfo: `#!/bin/sh
case "$1" in *big.pdf) echo "Pages:          12" ;; *) echo "Pages:          1" ;; esac
`,
    pdftoppm: `#!/bin/sh
if [ "$1" = "-v" ]; then echo "pdftoppm version 24.0.0" >&2; exit 0; fi
case "$8" in *locked.pdf) echo "Command Line Error: Incorrect password" >&2; exit 1 ;; esac
i=$5
while [ "$i" -le "$7" ]; do cp "${bin}/page.jpg" "$9-$i.jpg"; i=$((i + 1)); done
`,
  };
  for (const [name, body] of Object.entries(scripts)) {
    await writeFile(join(bin, name), body);
    await chmod(join(bin, name), 0o755);
  }
  return { PATH: `${bin}${delimiter}${process.env.PATH ?? ""}` };
}
const posixOnly = { skip: process.platform === "win32" && "fake Poppler tools are shell scripts" };

test("Rust Read sends a whole PDF as a document block to PDF models", posixOnly, async () => {
  const env = await poppler(png);
  const { f, first, final, row } = await run({
    anthropic: true,
    inputFormat: pdfFormat,
    file: "doc.pdf",
    bytes: pdf,
    env,
  });
  try {
    const definition = first.tools.find((t: Message) => t.name === "Read");
    assert.equal(definition.input_schema.properties.pages.type, "string");
    assert.ok(
      definition.description.endsWith(
        '\n- Reads PDFs via the `pages` parameter (e.g. "1-5", max 20 pages/request; required for PDFs over 10 pages).',
      ),
    );
    const shown = join(await realpath(f.cwd), "doc.pdf");
    const heading = `PDF file read: ${shown} (${pdf.length} bytes)`;
    assert.deepEqual(toolResult(final), [
      { type: "text", text: heading },
      {
        type: "document",
        source: { type: "base64", media_type: "application/pdf", data: pdf.toString("base64") },
      },
    ]);
    assert.equal(row.output.text, `${heading}\n\n[Attached application/pdf: doc.pdf]`);
  } finally {
    await f.close();
  }
});

test("Rust Read reports Node's PDF handler failures", posixOnly, async () => {
  const env = await poppler(png);
  const cases: [string, Buffer, Message, (path: string) => string][] = [
    [
      "bad.pdf",
      Buffer.from("plain"),
      {},
      (p) => `File is not a valid PDF (missing %PDF- header): ${p}`,
    ],
    [
      "big.pdf",
      pdf,
      {},
      () =>
        'This PDF has 12 pages, which is too many to read at once. Use the pages parameter to read specific page ranges (e.g., pages: "1-5"). Maximum 20 pages per request.',
    ],
    [
      "doc.pdf",
      pdf,
      { pages: "0-2" },
      () =>
        'Invalid pages parameter: "0-2". Use formats like "1-5", "3", or "10-20". Pages are 1-indexed.',
    ],
    [
      "locked.pdf",
      pdf,
      { pages: "1" },
      () => "PDF is password-protected. Please provide an unprotected version.",
    ],
  ];
  for (const [file, bytes, args, message] of cases) {
    const { f, final } = await run({
      anthropic: false,
      inputFormat: pdfFormat,
      file,
      bytes,
      args,
      env,
    });
    try {
      assert.equal(
        toolResult(final),
        `<tool_use_error>${message(join(await realpath(f.cwd), file))}</tool_use_error>`,
        file,
      );
    } finally {
      await f.close();
    }
  }
});

test("Rust Read renders requested PDF pages as images", posixOnly, async () => {
  const env = await poppler(png);
  const { f, final, row } = await run({
    anthropic: true,
    inputFormat: pdfFormat,
    file: "doc.pdf",
    bytes: pdf,
    args: { pages: "2-3" },
    env,
  });
  try {
    const image = {
      type: "image",
      source: { type: "base64", media_type: "image/png", data: png.toString("base64") },
    };
    const heading = `PDF pages extracted: 2 page(s) from ${join(await realpath(f.cwd), "doc.pdf")} (${pdf.length} bytes)`;
    assert.deepEqual(toolResult(final), [{ type: "text", text: heading }, image, image]);
    assert.equal(
      row.output.text,
      [heading, "[Attached image/png: PDF page 2]", "[Attached image/png: PDF page 3]"].join(
        "\n\n",
      ),
    );
  } finally {
    await f.close();
  }
});

test("Rust Read treats PDFs as files for models without PDF input", async () => {
  const { f, first, final } = await run({
    anthropic: false,
    inputFormat: { supportsText: true, supportsImage: true },
    file: "notes.pdf",
    bytes: Buffer.from("hello\n"),
    args: { pages: "1" },
  });
  try {
    const definition = first.tools.find((t: Message) => t.function.name === "Read");
    assert.equal(definition.function.parameters.properties.pages, undefined);
    assert.match(toolResult(final), /1\thello/);
  } finally {
    await f.close();
  }
});
