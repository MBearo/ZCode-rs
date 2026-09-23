import assert from "node:assert/strict";
import test from "node:test";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { fixture, binary } from "./zcode-cli-rust-fixture.js";

test("Rust stdio bounds malformed requests, handles split Unicode and closes on EOF/EPIPE", async () => {
  const f = await fixture();
  try {
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    h.child.stdin.write("not json\n");
    await h.wait((m) => m.error?.code === -32700);
    const bytes = Buffer.from(
      JSON.stringify({
        id: "unicode",
        method: "v4/command",
        params: h.envelope("sendText", id, { text: "你好 Rust" }),
      }) + "\n",
    );
    const split = bytes.indexOf(Buffer.from("你好")) + 1;
    h.child.stdin.write(bytes.subarray(0, split));
    h.child.stdin.write(bytes.subarray(split));
    await h.wait((m) => m.id === "unicode" && m.result?.status === "accepted");
    await h.completed(id);
    assert.deepEqual(h.schemaErrors, []);
    await h.close();

    for (const mode of ["large", "epipe"] as const) {
      // 自动发现会读取开发者的 TS 数据源；传输测试必须与 fixture 使用相同的隔离配置。
      const child = spawn(
        binary,
        ["app-server", "--stdio", "--cwd", f.cwd, "--data-dir", f.dataDir, "--config", f.config],
        {
          env: {
            ...process.env,
            ZCODE_SESSION_DB_PATH: `${f.root}/ts.sqlite`,
            ZCODE_WORKSPACE_IDENTITY: "",
          },
        },
      );
      const closed = once(child, "close");
      const watchdog = setTimeout(() => child.kill("SIGKILL"), 5000);
      child.stdin.on("error", () => {});
      child.stderr.resume();
      let stdout = "";
      child.stdout.on("data", (part) => {
        stdout += part;
      });
      if (mode === "large") child.stdin.end("x".repeat(1024 * 1024 + 1));
      else child.stdout.destroy();
      const status = await closed;
      clearTimeout(watchdog);
      assert.equal(status[1], null, `${mode} must finish without watchdog kill`);
      if (mode === "large") assert.equal(status[0], 0);
      else
        assert(
          [0, 1].includes(status[0]),
          "A closed output is either graceful shutdown or startup transport failure",
        );
      if (mode === "large")
        assert(
          stdout
            .split("\n")
            .filter(Boolean)
            .map((line) => JSON.parse(line))
            .some((m) => m.error?.code === -32600),
        );
    }
  } finally {
    await f.close();
  }
});

test(
  "Rust signals cancel a saturated output channel without hanging the Host",
  { skip: process.platform === "win32" },
  async () => {
    const f = await fixture();
    try {
      // 这里只验证背压下的退出，不让本机历史导入占据启动过程。
      const child = spawn(
        binary,
        ["app-server", "--stdio", "--cwd", f.cwd, "--data-dir", f.dataDir, "--config", f.config],
        {
          env: {
            ...process.env,
            ZCODE_SESSION_DB_PATH: `${f.root}/ts.sqlite`,
            ZCODE_WORKSPACE_IDENTITY: "",
          },
        },
      );
      const closed = once(child, "close");
      child.stderr.resume();
      child.stdin.on("error", () => {});
      child.stdout.pause();
      child.stdin.write(
        (JSON.stringify({ id: 1, method: "runtime/capabilities", params: {} }) + "\n").repeat(
          12000,
        ),
      );
      const signal = setTimeout(() => child.kill("SIGTERM"), 500);
      const watchdog = setTimeout(() => child.kill("SIGKILL"), 5000);
      const [code, exitSignal] = await closed;
      clearTimeout(signal);
      clearTimeout(watchdog);
      assert.equal(exitSignal, null);
      assert([0, 1].includes(code));
    } finally {
      await f.close();
    }
  },
);
