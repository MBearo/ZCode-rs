import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
import { readFile, readdir } from "node:fs/promises";
import { join, resolve } from "node:path";
import { promisify } from "node:util";
import { metadata, platforms } from "./package.mjs";
import { npm } from "./npm.mjs";
import { NPM_REGISTRY } from "../fixture/model.mjs";

const exec = promisify(execFile);
const directory = resolve(process.argv[2]);
const dryRun = process.argv.includes("--dry-run");
const names = [...platforms.map((item) => `@mbearo/zcode-${item.id}`), metadata.name];
const files = (await readdir(directory)).filter((file) => file.endsWith(".tgz"));
const packages = new Map();
for (const file of files) {
  const tarball = join(directory, file);
  const { stdout } = await exec("tar", ["-xOf", tarball, "package/package.json"]);
  const pkg = JSON.parse(stdout);
  if (!names.includes(pkg.name) || pkg.version !== metadata.version || packages.has(pkg.name)) {
    throw new Error(`Unexpected or duplicate package: ${pkg.name}@${pkg.version}`);
  }
  packages.set(pkg.name, {
    tarball,
    integrity: `sha512-${createHash("sha512")
      .update(await readFile(tarball))
      .digest("base64")}`,
  });
}
if (packages.size !== names.length) throw new Error("Missing platform or entry packages");

// 所有平台包准备齐全后才允许发包，入口始终最后发布。
for (const name of names) {
  const artifact = packages.get(name);
  if (!dryRun) {
    let existing;
    try {
      existing = JSON.parse(
        (
          await npm([
            "view",
            `${name}@${metadata.version}`,
            "dist.integrity",
            "--json",
            `--registry=${NPM_REGISTRY}`,
          ])
        ).stdout,
      );
    } catch (error) {
      let code;
      try {
        code = JSON.parse(error.stdout).error?.code;
      } catch {
        /* 下方保留原始错误 */
      }
      if (code !== "E404") throw error;
    }
    if (existing) {
      if (existing !== artifact.integrity)
        throw new Error(`${name}@${metadata.version} already exists with different contents`);
      console.log(`Already published with matching integrity: ${name}@${metadata.version}`);
      continue;
    }
  }
  const args = [
    "publish",
    artifact.tarball,
    "--access",
    "public",
    "--ignore-scripts",
    `--registry=${NPM_REGISTRY}`,
  ];
  if (dryRun) args.push("--dry-run");
  else if (process.env.GITHUB_ACTIONS === "true") args.push("--provenance");
  const result = await npm(args);
  process.stdout.write(result.stdout);
  process.stderr.write(result.stderr);
}
