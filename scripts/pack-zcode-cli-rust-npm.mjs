import { parseArgs, promisify } from "node:util";
import { execFile } from "node:child_process";
import {
  mkdir,
  mkdtemp,
  readFile,
  writeFile,
  copyFile,
  cp,
  chmod,
  access,
  rm,
} from "node:fs/promises";
import { join, resolve, dirname, basename } from "node:path";
import { platformKey, sha256File } from "../apps/zcode-cli-rust/npm/lib/install.mjs";

const { values } = parseArgs({
  options: {
    binary: { type: "string" },
    platform: { type: "string" },
    scope: { type: "string" },
    out: { type: "string", default: "dist-release/rust-npm" },
    help: { type: "boolean" },
  },
});
if (values.help) {
  console.log(
    "node scripts/pack-zcode-cli-rust-npm.mjs --binary <release executable> [--platform darwin-arm64] [--scope @your-scope] [--out directory]",
  );
  process.exit(0);
}
if (!values.binary) throw Error("--binary is required. Build the release binary first.");
if (values.scope !== undefined && !/^@[a-z0-9][a-z0-9-]*$/.test(values.scope))
  throw Error("Invalid npm scope");
const targets = {
  "darwin-arm64": { os: ["darwin"], cpu: ["arm64"] },
  "darwin-x64": { os: ["darwin"], cpu: ["x64"] },
  "win32-x64": { os: ["win32"], cpu: ["x64"] },
  "win32-arm64": { os: ["win32"], cpu: ["arm64"] },
  "linux-x64-gnu": { os: ["linux"], cpu: ["x64"], libc: ["glibc"] },
  "linux-arm64-gnu": { os: ["linux"], cpu: ["arm64"], libc: ["glibc"] },
};
const platform = values.platform ?? platformKey();
if (!targets[platform]) throw Error(`Unsupported release target: ${platform}`);
const root = resolve(import.meta.dirname, ".."),
  source = join(root, "apps/zcode-cli-rust/npm");
const pkg = JSON.parse(await readFile(join(source, "package.json"), "utf8"));
const binary = resolve(values.binary),
  out = resolve(values.out);
const exec = promisify(execFile);
// 本机构建必须验证版本；跨平台产物的运行检查由相应 OS 的 release job 完成。
if (platform === platformKey()) {
  const { stdout } = await exec(binary, ["--version"]);
  if (stdout.trim() !== pkg.version)
    throw Error(`Binary version ${stdout.trim()} does not match npm version ${pkg.version}`);
}
// 包名由入口 manifest 统一拥有；覆盖 scope 时保留包名，避免平台依赖仍使用旧名称。
const name = values.scope ? pkg.name.replace(/^@[^/]+/, values.scope) : pkg.name,
  nativeName = `${name}-${platform}`;
await mkdir(out, { recursive: true });
// 每次使用全新 staging，避免重复打包混入上一版本已经删除的文件。
const staging = await mkdtemp(join(out, ".pack-"));
try {
  const entry = join(staging, "entry"),
    native = join(staging, platform);
  await mkdir(entry, { recursive: true });
  await mkdir(join(native, "bin"), { recursive: true });
  for (const file of ["bin.mjs", "bridge.cjs", "README.md"])
    await copyFile(join(source, file), join(entry, file));
  await cp(join(source, "lib"), join(entry, "lib"), { recursive: true });
  await chmod(join(entry, "bin.mjs"), 0o755);
  const dependencies = Object.fromEntries(
    Object.keys(targets).map((target) => [`${name}-${target}`, pkg.version]),
  );
  await writeFile(
    join(entry, "package.json"),
    JSON.stringify(
      { ...pkg, name, scripts: undefined, optionalDependencies: dependencies },
      null,
      2,
    ) + "\n",
  );
  const filename = platform.startsWith("win32") ? "zcode-cli-rust.exe" : "zcode-cli-rust";
  await copyFile(binary, join(native, "bin", filename));
  await chmod(join(native, "bin", filename), 0o755);
  await writeFile(
    join(native, "package.json"),
    JSON.stringify(
      {
        name: nativeName,
        version: pkg.version,
        description: `ZCode Rust executable for ${platform}`,
        license: "Apache-2.0",
        ...targets[platform],
        files: ["bin", "runtime.json", "LICENSE"],
        publishConfig: { access: "public" },
      },
      null,
      2,
    ) + "\n",
  );
  await writeFile(
    join(native, "runtime.json"),
    JSON.stringify({ version: pkg.version, platform, sha256: await sha256File(binary) }, null, 2) +
      "\n",
  );
  for (const directory of [entry, native])
    await copyFile(join(root, "LICENSE"), join(directory, "LICENSE"));
  // Windows 的 npm.cmd 不能直接 execFile，也不能把用户提供的输出路径拼入 shell。
  let npm = "npm",
    npmArgs = [];
  if (process.platform === "win32") {
    const { stdout } = await exec("where.exe", ["npm.cmd"]);
    const candidates = [
      process.env.npm_execpath,
      join(dirname(process.execPath), "node_modules/npm/bin/npm-cli.js"),
      ...stdout
        .trim()
        .split(/\r?\n/)
        .map((file) => join(dirname(file), "node_modules/npm/bin/npm-cli.js")),
    ];
    let cli;
    for (const candidate of candidates)
      if (candidate && basename(candidate) === "npm-cli.js") {
        try {
          await access(candidate);
          cli = candidate;
          break;
        } catch {
          /* 尝试下一份 npm 安装。 */
        }
      }
    if (!cli) throw Error("Cannot locate npm-cli.js. Install Node.js with npm before packing.");
    npm = process.execPath;
    npmArgs = [cli];
  }
  for (const directory of [native, entry]) {
    const { stdout } = await exec(npm, [...npmArgs, "pack", "--json", "--pack-destination", out], {
      cwd: directory,
      maxBuffer: 4 * 1024 * 1024,
    });
    const [packed] = JSON.parse(stdout);
    console.log(
      JSON.stringify({
        name: packed.name,
        version: packed.version,
        file: join(out, packed.filename),
        integrity: packed.integrity,
      }),
    );
  }
} finally {
  await rm(staging, { recursive: true, force: true });
}
