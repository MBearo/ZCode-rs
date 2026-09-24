import assert from "node:assert/strict";
import test from "node:test";
import { createHash, randomBytes } from "node:crypto";
import { writeFile, rm } from "node:fs/promises";
import { crc32, deflateSync } from "node:zlib";
import { join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { fixture, event, end } from "./zcode-cli-rust-fixture.js";
import { responses, anthropic } from "./zcode-cli-rust-protocol-fixture.js";
import {
  v4AttachmentBeginResultSchema,
  v4AttachmentChunkResultSchema,
  v4AttachmentCommitResultSchema,
} from "@zcode/shared/zcode-protocol-v4";

const properties = {
  inputFormat: {
    supportsText: true,
    supportsImage: true,
    supportsPdf: true,
    supportsVideo: true,
    supportsAudio: false,
  },
  outputFormat: { supportsText: true },
};
// 原先的 1x1 固定 PNG 的 IDAT CRC 不对，Node 的 Jimp 与 Rust 都无法解码（按 Node 降级为占位）。
const png = noisePng(2, 2);
for (const apiType of ["openai-chat-completions", "anthropic-messages"]) {
  test(`Rust ${apiType} preserves the TS gateway video content shape`, async () => {
    const f = await fixture({
      config: { apiType, reasoningParameters: {}, formatProperties: properties },
      respond(_req, res) {
        if (apiType === "anthropic-messages") anthropic(res);
        else {
          res.writeHead(200, { "content-type": "text/event-stream" });
          event(res, { content: "video accepted" });
          end(res, "stop");
        }
      },
    });
    try {
      const h = f.start();
      const id = await h.create();
      await h.subscribe(`conversation/${id}`);
      const path = join(f.cwd, "clip.mp4");
      const bytes = Buffer.from([0, 0, 0, 20, 102, 116, 121, 112, 105, 115, 111, 109]);
      await writeFile(path, bytes);
      await h.command(
        h.envelope("sendText", id, {
          text: "video",
          attachments: [
            { ref: path, fileName: "clip.mp4", mime: "video/mp4", bytes: bytes.length },
          ],
        }),
      );
      await h.completed(id);
      const content = f.requests[0]!.messages.findLast((m: any) => m.role === "user").content;
      // Anthropic 合并相邻 user 消息，context reminder 位于实际附件文本之前。
      if (apiType === "anthropic-messages") assert.match(content[0].text, /^<system-reminder>/);
      const video = content[apiType === "anthropic-messages" ? 2 : 1];
      if (apiType === "anthropic-messages")
        assert.deepEqual(video, {
          type: "video",
          source: { type: "base64", media_type: "video/mp4", data: bytes.toString("base64") },
        });
      else
        assert.deepEqual(video, {
          type: "video_url",
          video_url: { url: `data:video/mp4;base64,${bytes.toString("base64")}` },
        });
      assert.deepEqual(h.schemaErrors, []);
    } finally {
      await f.close();
    }
  });
}

test("Rust transports media above the text request budget and degrades a missing artifact to its placeholder after restart", async () => {
  const f = await fixture({ config: { formatProperties: properties } });
  try {
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    const path = join(f.cwd, "large.pdf");
    const bytes = Buffer.from(`%PDF-1.4\n${"a".repeat(2 * 1024 * 1024)}\n%%EOF`);
    await writeFile(path, bytes);
    await h.command(
      h.envelope("sendText", id, {
        text: "read",
        attachments: [
          { ref: path, fileName: "large.pdf", mime: "application/pdf", bytes: bytes.length },
        ],
      }),
    );
    await h.completed(id);
    assert(f.requestBodies[0]!.length > 2 * 1024 * 1024);
    await h.close();
    // 快照只在 Node 产物里；产物丢失后与 Node hydrator 一样退化为占位文本，仍然发请求。
    await rm(f.artifacts, { recursive: true });
    const restarted = f.start();
    await restarted.subscribe(`conversation/${id}`);
    await restarted.command(restarted.envelope("sendText", id, { text: "continue" }));
    await restarted.completed(id);
    assert.equal(f.requests.length, 2);
    assert(f.requestBodies[1]!.length < 1024 * 1024);
    assert.match(f.requestBodies[1]!, /\[Attached application\/pdf: large\.pdf\]/);
    assert.deepEqual(restarted.schemaErrors, []);
  } finally {
    await f.close();
  }
});

for (const apiType of ["openai-chat-completions", "openai-responses", "anthropic-messages"]) {
  test(`Rust ${apiType} sends immutable image/PDF attachments as native media, keeps base64 out of storage and reuses request bytes on retry`, async () => {
    const f = await fixture({
      config: {
        apiType,
        reasoningParameters: {},
        formatProperties: properties,
        retry: { maxRetries: 1, baseDelayMs: 1, maxDelayMs: 1, jitter: false },
      },
      respond(_req, res, attempt) {
        if (attempt === 1) {
          res.writeHead(429);
          res.end();
          return;
        }
        if (apiType === "openai-responses") responses(res);
        else if (apiType === "anthropic-messages") anthropic(res);
        else {
          res.writeHead(200, { "content-type": "text/event-stream" });
          event(res, { content: "saw attachment" });
          end(res, "stop");
        }
      },
    });
    try {
      const h = f.start();
      const id = await h.create();
      await h.subscribe(`conversation/${id}`);
      const path = join(f.cwd, "image.png");
      const pdf = join(f.cwd, "document.pdf");
      await writeFile(path, png);
      await writeFile(pdf, "%PDF-1.4\nfixture\n%%EOF");
      const ack = await h.command(
        h.envelope("sendText", id, {
          text: "describe",
          attachments: [
            { ref: path, fileName: "image.png", mime: "image/png", bytes: png.length },
            { ref: pdf, fileName: "document.pdf", mime: "application/pdf", bytes: 23 },
          ],
        }),
      );
      assert.equal(ack.status, "accepted");
      await h.completed(id);
      assert.equal(f.requests.length, 2);
      assert.equal(f.requestBodies[0], f.requestBodies[1]);
      const body = f.requests[0]!;
      let content = (apiType === "openai-responses" ? body.input : body.messages).findLast(
        (m: any) => m.role === "user",
      ).content;
      if (apiType === "anthropic-messages") {
        assert.equal(content[0].type, "text");
        assert.match(content[0].text, /^<system-reminder>/);
        content = content.slice(1);
      }
      assert.deepEqual(
        content.map((p: any) => p.type),
        apiType === "openai-responses"
          ? ["input_text", "input_image", "input_file"]
          : apiType === "anthropic-messages"
            ? ["text", "image", "document"]
            : ["text", "image_url", "file"],
      );
      assert.match(
        JSON.stringify(content),
        new RegExp(png.toString("base64").replace(/[.*+?^${}()|[\]\\]/g, "\\$&")),
      );
      assert(!f.requestBodies[0]!.includes(f.dataDir));
      assert(!f.requestBodies[0]!.includes("_zcode_attachment"));
      // 与 Node 相同：字节只在 data URL 产物里，user 消息的 file part 引用 zcode-artifact URI。
      const db = new DatabaseSync(f.db, { readOnly: true });
      const stored = db
        .prepare("SELECT data FROM part WHERE session_id=?")
        .all(id)
        .map((r) => r.data)
        .join("");
      db.close();
      assert(!stored.includes(png.toString("base64")));
      assert(stored.includes("zcode-artifact://"));
      assert.deepEqual(h.schemaErrors, []);
    } finally {
      await f.close();
    }
  });
}

test("Rust rejects unsupported media before committing a user turn and degrades an invalid local PDF to a placeholder like Node", async () => {
  for (const supportsPdf of [false, true]) {
    const f = await fixture({
      config: {
        formatProperties: {
          ...properties,
          inputFormat: { ...properties.inputFormat, supportsPdf },
        },
      },
    });
    try {
      const h = f.start();
      const id = await h.create();
      const path = join(f.cwd, "bad.pdf");
      await writeFile(path, "not a PDF");
      const send = h.command(
        h.envelope("sendText", id, {
          text: "read",
          attachments: [{ ref: path, fileName: "bad.pdf", mime: "application/pdf", bytes: 9 }],
        }),
      );
      if (!supportsPdf) {
        await assert.rejects(send, /unsupported by selected model/);
        assert.equal((await h.rows(id)).rows.length, 0);
        assert.equal(f.requests.length, 0);
        continue;
      }
      // Node `attachment-media-resolver`：文件头不是 %PDF- 的本地 PDF 退化为占位
      // （attachment_pdf_invalid），输入照常接受，模型只看到占位文本。
      await h.subscribe(`conversation/${id}`);
      assert.equal((await send).status, "accepted");
      await h.completed(id);
      const user = f.requests[0]!.messages.at(-1);
      assert.deepEqual(user.content, [
        { type: "text", text: "read" },
        { type: "text", text: `[Attached application/pdf: ${path}]` },
      ]);
    } finally {
      await f.close();
    }
  }
});

/** An RGB PNG of random pixels (it does not compress). */
function noisePng(width: number, height: number): Buffer {
  const chunk = (type: string, data: Buffer) => {
    const head = Buffer.alloc(8);
    head.writeUInt32BE(data.length);
    head.write(type, 4, "ascii");
    const crc = Buffer.alloc(4);
    crc.writeUInt32BE(crc32(Buffer.concat([head.subarray(4), data])));
    return Buffer.concat([head, data, crc]);
  };
  const header = Buffer.alloc(13);
  header.writeUInt32BE(width);
  header.writeUInt32BE(height, 4);
  header.set([8, 2, 0, 0, 0], 8);
  const rows = Buffer.concat(
    Array.from({ length: height }, () => Buffer.concat([Buffer.from([0]), randomBytes(width * 3)])),
  );
  return Buffer.concat([
    Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]),
    chunk("IHDR", header),
    chunk("IDAT", deflateSync(rows)),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}

test("Rust prepares prompt images like Node: the live request sends the prepared image and the upload artifact keeps the original", async () => {
  const f = await fixture({ config: { formatProperties: properties } });
  try {
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    // 超过 3.75 MiB 原始字节预算的上传图片：Node 转成 JPEG 发送，产物保留原图。
    const large = noisePng(1400, 1400);
    const upload = {
      connectionId: "fixture-desktop",
      sessionId: id,
      uploadId: "upload-image",
      fileName: "shot.png",
      mime: "image/png",
      totalBytes: large.length,
      totalChunks: Math.ceil(large.length / (512 * 1024)),
      checksum: `sha256:${createHash("sha256").update(large).digest("hex")}`,
    };
    const terminal = {
      connectionId: upload.connectionId,
      sessionId: id,
      uploadId: upload.uploadId,
    };
    await h.client.request("v4/attachment/begin", upload, v4AttachmentBeginResultSchema);
    for (let index = 0; index < upload.totalChunks; index++)
      await h.client.request(
        "v4/attachment/chunk",
        {
          ...terminal,
          chunkIndex: index,
          dataBase64: large
            .subarray(index * 512 * 1024, (index + 1) * 512 * 1024)
            .toString("base64"),
        },
        v4AttachmentChunkResultSchema,
      );
    const { ref } = await h.client.request(
      "v4/attachment/commit",
      terminal,
      v4AttachmentCommitResultSchema,
    );
    await h.command(
      h.envelope("sendText", id, {
        text: "look",
        attachments: [{ ref, fileName: "shot.png", mime: "image/png", bytes: large.length }],
      }),
    );
    await h.completed(id);
    const sent = JSON.stringify(f.requests[0]);
    assert.match(sent, /data:image\/jpeg;base64,/);
    assert(!sent.includes(large.subarray(0, 3000).toString("base64")));
    const db = new DatabaseSync(f.db, { readOnly: true });
    const part = db
      .prepare('SELECT data FROM part WHERE session_id=? AND data LIKE \'%"type":"file"%\'')
      .all(id)
      .map((r) => JSON.parse(String(r.data)))[0];
    db.close();
    assert.equal(part.mime, "image/jpeg");
    assert.equal(part.url, ref);
    assert.deepEqual(Object.keys(part.metadata)[0], "image");
    assert.equal(part.metadata.image.maxDimension, 2000);
    assert.equal(part.metadata.image.originalWidth, 1400);
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});
