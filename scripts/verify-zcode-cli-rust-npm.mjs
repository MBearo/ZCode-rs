import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { mkdtemp, mkdir, readFile, writeFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { promisify } from "node:util";

// 从消费者安装目录验证发布包，避免源码目录解析到开发机已有的平台包。
assert.ok(process.argv[2], "Pass the consumer installation directory");
const entry = join(resolve(process.argv[2]), "node_modules/@mbears/zcode-rs");
const pkg = JSON.parse(await readFile(join(entry, "package.json"), "utf8"));
const { binarySource, installBinary, platformKey, sha256File } = await import(
  pathToFileURL(join(entry, "lib/install.mjs"))
);
const source = await binarySource();
assert.equal(pkg.name, "@mbears/zcode-rs");
assert.equal(source.platform, platformKey());
assert.equal(source.version, pkg.version);
assert.equal(await sha256File(source.binary), source.sha256);
if (process.platform === "win32") {
  const pe = await readFile(source.binary);
  assert.equal(pe.readUInt16LE(0), 0x5a4d);
  const offset = pe.readUInt32LE(0x3c);
  assert.equal(pe.readUInt32LE(offset), 0x4550);
  assert.equal(pe.readUInt16LE(offset + 4), process.arch === "x64" ? 0x8664 : 0xaa64);
}
const root = await mkdtemp(join(tmpdir(), "zcode npm smoke "));
const exec = promisify(execFile);
try {
  const binary = await installBinary(source, join(root, "runtime"));
  assert.equal(await sha256File(binary), source.sha256);
  assert.equal((await exec(binary, ["--version"])).stdout.trim(), pkg.version);
  const app = join(root, "ZCode fixture");
  await mkdir(join(app, "resources/app"), { recursive: true });
  await mkdir(join(app, "resources/glm"), { recursive: true });
  await writeFile(join(app, "resources/app/package.json"), JSON.stringify({ version: "3.14.3" }));
  await writeFile(join(app, "resources/glm/zcode.cjs"), "// Metadata fixture only\n");
  const executable = join(app, process.platform === "win32" ? "ZCode.exe" : "zcode");
  await writeFile(executable, "metadata fixture");
  const { stdout } = await exec(process.execPath, [
    join(entry, "bin.mjs"),
    "app",
    "doctor",
    "--app",
    executable,
    "--runtime-dir",
    join(root, "runtime"),
  ]);
  const doctor = JSON.parse(stdout);
  assert.equal(doctor.app.version, "3.14.3");
  assert.equal(doctor.platform, source.platform);
  assert.equal(doctor.version, pkg.version);
  console.log(
    JSON.stringify({
      name: pkg.name,
      version: pkg.version,
      platform: source.platform,
      sha256: source.sha256,
      doctor: "metadata fixture passed",
    }),
  );
} finally {
  await rm(root, { recursive: true, force: true });
}
