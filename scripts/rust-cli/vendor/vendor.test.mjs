// 第三方源码 vendor 规则的测试（docs/specs/rust-cli/embedded-search.md §9）：
//   node --test scripts/rust-cli/vendor/vendor.test.mjs
// 只测纯函数，不联网；另外核对仓库里的 sources.json 都过得了校验、改动的文件已经套上了改动。
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { test } from "node:test";

import {
  applyEdits,
  compareTrees,
  kept,
  sha256,
  stripPath,
  unsafePath,
  validateComponent,
} from "./rules.mjs";
import { render } from "./notice.mjs";

const vendor = resolve(import.meta.dirname, "../../../packages/rust-cli/vendor");

test("keep 列表按文件或目录前缀匹配", () => {
  assert.equal(kept("crates/core/main.rs", ["crates/core"]), true);
  assert.equal(kept("crates/core", ["crates/core"]), true);
  assert.equal(kept("crates/corex/a.rs", ["crates/core"]), false);
  assert.equal(kept("COPYING", ["COPYING", "src"]), true);
});

test("去掉归档的顶层目录；不安全的路径", () => {
  assert.equal(stripPath("ripgrep-15.2.0/crates/core/main.rs", 1), "crates/core/main.rs");
  assert.equal(stripPath("ripgrep-15.2.0/", 1), null);
  assert.equal(stripPath("main.c", 0), "main.c");
  for (const path of ["/etc/passwd", "a/../../b", "a\\b", ".."])
    assert.equal(unsafePath(path), true, path);
  for (const path of ["a/b", "x/.hidden", "a..b"]) assert.equal(unsafePath(path), false, path);
});

test("改动：每处恰好出现一次，按顺序做", () => {
  assert.equal(
    applyEdits("fn main() {}", [{ old: "fn main", new: "pub fn main" }], "f"),
    "pub fn main() {}",
  );
  assert.throws(() => applyEdits("x", [{ old: "y", new: "z" }], "f"), /找不到/);
  assert.throws(() => applyEdits("aa", [{ old: "a", new: "b" }], "f"), /不止一次/);
  assert.equal(
    applyEdits(
      "ab",
      [
        { old: "a", new: "c" },
        { old: "cb", new: "d" },
      ],
      "f",
    ),
    "d",
  );
});

test("组件描述：只认官方来源、SHA256 的形状、keep 里要有被改的文件", () => {
  const base = {
    id: "x",
    version: "1",
    url: "https://github.com/o/r/archive/refs/tags/1.tar.gz",
    sha256: "0".repeat(64),
    license: "MIT",
    into: "x/up",
    strip: 1,
    keep: ["src"],
  };
  assert.equal(validateComponent({ ...base }).id, "x");
  assert.throws(
    () => validateComponent({ ...base, url: "https://example.com/x.tar.gz" }),
    /官方来源/,
  );
  assert.throws(() => validateComponent({ ...base, sha256: "abc" }), /sha256/);
  assert.throws(() => validateComponent({ ...base, into: "../x" }), /into/);
  assert.throws(() => validateComponent({ ...base, edits: { "other/a.c": [] } }), /不在 keep 里/);
  const { license, ...missing } = base;
  assert.throws(() => validateComponent(missing), /license/);
});

test("比较两棵树", () => {
  const a = new Map([
    ["x", Buffer.from("1")],
    ["y", Buffer.from("2")],
  ]);
  const b = new Map([
    ["x", Buffer.from("1")],
    ["y", Buffer.from("3")],
    ["z", Buffer.from("4")],
  ]);
  assert.deepEqual(compareTrees(a, b), { missing: [], extra: ["z"], changed: ["y"] });
  assert.equal(
    sha256(Buffer.from("")),
    "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
  );
});

test("仓库里的 sources.json 过得了校验，改动已经在 vendor 的文件里", async () => {
  const sources = JSON.parse(await readFile(join(vendor, "sources.json"), "utf8"));
  assert.ok(sources.components.length >= 1);
  for (const component of sources.components) {
    validateComponent(component);
    for (const [file, edits] of Object.entries(component.edits ?? {})) {
      const text = await readFile(join(vendor, component.into, file), "utf8");
      for (const edit of edits) {
        assert.ok(text.includes(edit.new), `${component.id} ${file}：改动没有套上`);
        assert.ok(
          !edit.old || !text.includes(edit.old) || edit.new.includes(edit.old),
          `${component.id} ${file}：旧文字还在`,
        );
      }
    }
    for (const license of component.licenseFiles ?? []) {
      await readFile(join(vendor, component.into, license));
    }
  }
});

test("许可证声明：vendor 组件与依赖图里 crates.io 的包，-sys crate 带上 C 库的许可证", async () => {
  const sources = {
    components: [
      { id: "bfs", version: "4.1.4", license: "0BSD", into: "bfs/bfs", licenseFiles: ["LICENSE"] },
    ],
  };
  const metadata = {
    packages: [
      {
        id: "v-bfs",
        name: "rust-cli-vendor-bfs",
        version: "4.1.4",
        manifest_path: "/v/bfs/Cargo.toml",
      },
      {
        id: "v-rg",
        name: "rust-cli-vendor-rg",
        version: "15.2.0",
        manifest_path: "/v/rg/Cargo.toml",
      },
      {
        id: "v-ug",
        name: "rust-cli-vendor-ugrep",
        version: "7.8.4",
        manifest_path: "/v/ug/Cargo.toml",
      },
      {
        id: "z",
        name: "libz-sys",
        version: "1.1.30",
        license: "MIT OR Apache-2.0",
        source: "registry",
        manifest_path: "/r/libz-sys/Cargo.toml",
      },
      { id: "cc", name: "cc", version: "1", source: "registry", manifest_path: "/r/cc/Cargo.toml" },
    ],
    resolve: {
      nodes: [
        {
          id: "v-ug",
          deps: [
            { pkg: "z", dep_kinds: [{ kind: null }] },
            { pkg: "cc", dep_kinds: [{ kind: "build" }] },
          ],
        },
        { id: "v-rg", deps: [] },
        { id: "v-bfs", deps: [] },
      ],
    },
  };
  const files = {
    [join(vendor, "bfs/bfs/LICENSE")]: "BSD Zero Clause License",
    "/r/libz-sys/src/zlib/LICENSE": "zlib license text",
    "/r/libz-sys/LICENSE-MIT": "MIT text",
  };
  const text = await render({
    sources,
    metadata,
    read: async (path) => files[path] ?? assert.fail(`没有 ${path}`),
    list: async (dir) => (dir === "/r/libz-sys" ? ["LICENSE-MIT", "build.rs"] : []),
  });
  assert.match(text, /bfs 4\.1\.4 \(0BSD\)/);
  assert.match(text, /BSD Zero Clause License/);
  assert.match(text, /libz-sys 1\.1\.30 \(MIT OR Apache-2\.0\)/);
  assert.match(text, /--- zlib: src\/zlib\/LICENSE\n\nzlib license text/);
  assert.match(text, /MIT text/);
  // 构建依赖不进二进制，不列。
  assert.doesNotMatch(text, /\bcc 1\b/);
});

test("仓库里的许可证声明包含每个 vendor 组件的许可证文件", async () => {
  const notice = await readFile(join(vendor, "THIRD-PARTY-NOTICES.txt"), "utf8");
  const sources = JSON.parse(await readFile(join(vendor, "sources.json"), "utf8"));
  for (const component of sources.components) {
    assert.ok(
      notice.includes(`${component.id} ${component.version} (${component.license})`),
      component.id,
    );
    for (const file of component.licenseFiles) {
      const text = await readFile(join(vendor, component.into, file), "utf8");
      assert.ok(notice.includes(text.trimEnd()), `${component.id} ${file}`);
    }
  }
});
