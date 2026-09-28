import { createHash, randomUUID } from "node:crypto";
import { createReadStream } from "node:fs";
import { access, chmod, copyFile, mkdir, readFile, rename, rm } from "node:fs/promises";
import { join, dirname, resolve } from "node:path";
import { createRequire } from "node:module";

export async function sha256File(path) {
  const hash = createHash("sha256");
  for await (const chunk of createReadStream(path)) hash.update(chunk);
  return hash.digest("hex");
}
export function platformKey() {
  if (
    !["darwin", "win32", "linux"].includes(process.platform) ||
    !["x64", "arm64"].includes(process.arch)
  )
    throw Error("Unsupported operating system or architecture");
  const suffix =
    process.platform === "linux"
      ? process.report.getReport().header.glibcVersionRuntime
        ? "-gnu"
        : "-musl"
      : "";
  return `${process.platform}-${process.arch}${suffix}`;
}
export async function binarySource(localBinary) {
  if (localBinary) {
    const binary = resolve(localBinary);
    return { binary, sha256: await sha256File(binary), version: "local", platform: platformKey() };
  }
  const pkg = JSON.parse(await readFile(new URL("../package.json", import.meta.url), "utf8"));
  const name = `${pkg.name}-${platformKey()}`;
  let root;
  try {
    root = dirname(createRequire(import.meta.url).resolve(`${name}/package.json`));
  } catch {
    throw Error(
      `Native package ${name}@${pkg.version} is missing. Reinstall with optional dependencies enabled; this platform may not have a release yet.`,
    );
  }
  const manifest = JSON.parse(await readFile(join(root, "runtime.json"), "utf8"));
  if (manifest.version !== pkg.version || manifest.platform !== platformKey())
    throw Error("Native package version/platform mismatch");
  return {
    ...manifest,
    binary: join(
      root,
      "bin",
      process.platform === "win32" ? "zcode-cli-rust.exe" : "zcode-cli-rust",
    ),
  };
}
export async function installBinary(source, runtimeDir) {
  if (
    !/^[a-f0-9]{64}$/.test(source.sha256) ||
    !/^[\w.-]+$/.test(source.version) ||
    !/^[\w.-]+$/.test(source.platform)
  )
    throw Error("Invalid runtime manifest");
  const target = join(
    runtimeDir,
    "versions",
    `${source.version}-${source.platform}-${source.sha256}`,
  );
  const binary = join(
    target,
    process.platform === "win32" ? "zcode-cli-rust.exe" : "zcode-cli-rust",
  );
  const verify = async () => {
    if ((await sha256File(binary)) !== source.sha256)
      throw Error("Installed binary checksum mismatch");
    return binary;
  };
  try {
    await access(binary);
    return await verify();
  } catch (error) {
    if (error.code !== "ENOENT") throw error;
  }
  await mkdir(dirname(target), { recursive: true, mode: 0o700 });
  const staging = `${target}.tmp-${randomUUID()}`;
  await mkdir(staging, { mode: 0o700 });
  try {
    const copy = join(
      staging,
      process.platform === "win32" ? "zcode-cli-rust.exe" : "zcode-cli-rust",
    );
    await copyFile(source.binary, copy);
    if ((await sha256File(copy)) !== source.sha256)
      throw Error("Downloaded binary checksum mismatch");
    await chmod(copy, 0o755);
    try {
      await rename(staging, target);
    } catch (error) {
      if (!["EEXIST", "ENOTEMPTY", "EPERM"].includes(error.code)) throw error;
    }
    return await verify();
  } finally {
    await rm(staging, { recursive: true, force: true });
  }
}
