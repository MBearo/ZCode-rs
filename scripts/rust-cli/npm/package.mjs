import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
import { chmod, copyFile, cp, mkdir, readFile, stat, writeFile } from "node:fs/promises";
import { createRequire } from "node:module";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { currentVersion } from "../build-number.mjs";
import { npm } from "./npm.mjs";

const exec = promisify(execFile);
// 平台包里的可执行文件名只在启动器里定义（命令名 `zcode`；可执行文件 `zcode-rs`，Windows `zcode.exe`）。传进来的是 cargo
// 产物（三个平台都叫 zcode-rs），复制时改成这个名字。
const { executableFile } = createRequire(import.meta.url)("./package/bin/zcode.cjs");
export const root = resolve(import.meta.dirname, "../../..");
export const template = join(import.meta.dirname, "package");
/**
 * 内嵌的 rg、bfs、ugrep 与它们依赖的第三方许可证声明（packages/rust-cli/vendor 下生成并提交，每个平台包都带同一份；
 * 入口包不带二进制，不需要）。
 */
export const NOTICES = join(root, "packages/rust-cli/vendor/THIRD-PARTY-NOTICES.txt");
export const platforms = JSON.parse(await readFile(join(template, "platforms.json"), "utf8"));
const declared = JSON.parse(await readFile(join(template, "package.json"), "utf8"));

export function validateBinary(bytes, platform) {
  let os;
  let cpu;
  if (bytes.length >= 64 && bytes.readUInt32LE(0) === 0xfeedfacf) {
    os = "darwin";
    cpu = { 0x1000007: "x64", 0x100000c: "arm64" }[bytes.readUInt32LE(4)];
  } else if (
    bytes.length >= 64 &&
    bytes.subarray(0, 4).equals(Buffer.from([127, 69, 76, 70])) &&
    bytes[4] === 2 &&
    bytes[5] === 1
  ) {
    os = "linux";
    cpu = { 62: "x64", 183: "arm64" }[bytes.readUInt16LE(18)];
  } else if (bytes.length >= 64 && bytes.readUInt16LE(0) === 0x5a4d) {
    const offset = bytes.readUInt32LE(0x3c);
    if (offset + 6 <= bytes.length && bytes.readUInt32LE(offset) === 0x4550) {
      os = "win32";
      cpu = { 0x8664: "x64", 0xaa64: "arm64" }[bytes.readUInt16LE(offset + 4)];
    }
  }
  if (os !== platform.os || cpu !== platform.cpu) {
    throw new Error(
      `Binary does not match ${platform.id}: detected ${os ?? "unknown"}/${cpu ?? "unknown"}`,
    );
  }
}

/**
 * 入口包与平台包的版本：Cargo workspace 的 major.minor 加 packages/rust-cli/build-number，
 * 与 release 编译校验 ZCODE_RELEASE_VERSION 用的是同一个号。模板 package.json 不写版本。
 */
export async function verifyVersion({ manifest, counter } = {}) {
  if ("version" in declared) {
    throw new Error(
      "scripts/rust-cli/npm/package/package.json must not declare a version; it comes from packages/rust-cli/build-number",
    );
  }
  return currentVersion({ manifest, counter });
}

export const metadata = { name: declared.name, version: await verifyVersion(), ...declared };

export async function stagePackage({ id, binary, output, sourceCommit, notices = NOTICES }) {
  await verifyVersion();
  if (!/^[a-f\d]{40}$/u.test(sourceCommit)) throw new Error("Expected a full Git source commit");
  const platform = id === "entry" ? undefined : platforms.find((item) => item.id === id);
  if (id !== "entry" && !platform) throw new Error(`Unknown platform ${id}`);
  let bytes;
  if (platform) {
    if (!(await stat(binary)).isFile()) throw new Error("Expected a binary file");
    bytes = await readFile(binary);
    validateBinary(bytes, platform);
    if (!(await stat(notices).catch(() => null))?.isFile()) {
      throw new Error(
        `Missing ${notices}: platform packages ship THIRD-PARTY-NOTICES.txt for the bundled search tools`,
      );
    }
  }
  // 不复用已有目录，避免上次发布的二进制或文件混入新包。
  await mkdir(output, { recursive: false });
  let pkg;
  if (platform) {
    const executable = executableFile(platform.os);
    await mkdir(join(output, "bin"));
    await copyFile(binary, join(output, "bin", executable));
    await chmod(join(output, "bin", executable), 0o755);
    pkg = {
      name: `@mbearo/zcode-${id}`,
      version: metadata.version,
      description: `ZCode Rust CLI binary for ${id}`,
      license: metadata.license,
      os: [platform.os],
      cpu: [platform.cpu],
      ...(platform.libc ? { libc: [platform.libc] } : {}),
      files: ["bin", "build.json", "LICENSE", "README.md", "THIRD-PARTY-NOTICES.txt"],
      repository: metadata.repository,
      publishConfig: metadata.publishConfig,
    };
    await writeFile(
      join(output, "build.json"),
      JSON.stringify(
        {
          version: metadata.version,
          sourceCommit,
          target: platform.target,
          sha256: createHash("sha256").update(bytes).digest("hex"),
        },
        null,
        2,
      ) + "\n",
    );
    await writeFile(
      join(output, "README.md"),
      `# ${pkg.name}\n\nPlatform binary for [@mbearo/zcode](https://www.npmjs.com/package/@mbearo/zcode). Install the entry package instead.\n`,
    );
    // 原样复制：许可证原文里有 CRLF，不做换行转换。
    await copyFile(notices, join(output, "THIRD-PARTY-NOTICES.txt"));
  } else {
    pkg = {
      ...metadata,
      optionalDependencies: Object.fromEntries(
        platforms.map((item) => [`@mbearo/zcode-${item.id}`, metadata.version]),
      ),
    };
    await cp(join(template, "bin"), join(output, "bin"), { recursive: true });
    await chmod(join(output, "bin/zcode.cjs"), 0o755);
    await copyFile(join(template, "platforms.json"), join(output, "platforms.json"));
    await copyFile(join(template, "README.md"), join(output, "README.md"));
  }
  await copyFile(join(root, "LICENSE"), join(output, "LICENSE"));
  await writeFile(join(output, "package.json"), JSON.stringify(pkg, null, 2) + "\n");
  return pkg;
}

export async function pack({ id, binary, destination, sourceCommit, notices }) {
  const folder = join(destination, id);
  await mkdir(destination, { recursive: true });
  await stagePackage({ id, binary, output: folder, sourceCommit, notices });
  const { stdout } = await npm(["pack", "--json", "--pack-destination", destination], {
    cwd: folder,
  });
  return JSON.parse(stdout)[0];
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const [id, binary, destination = "dist/rust-cli-npm"] = process.argv.slice(2);
  const sourceCommit = (await exec("git", ["rev-parse", "HEAD"], { cwd: root })).stdout.trim();
  const artifact = await pack({ id, binary, destination: resolve(destination), sourceCommit });
  console.log(JSON.stringify(artifact, null, 2));
}
