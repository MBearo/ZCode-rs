// 编内嵌的搜索工具要的 C / C++ 工具链（docs/specs/rust-cli/embedded-search.md §7.2、§9.3）：发版与检查的 workflow 在
// cargo 之前跑一次，缺了就在这里报错，不等 build.rs 编到一半才失败。
// - linux-gnu：ugrep 静态链接 libstdc++，C++ 编译器（cc 的缺省 `c++`，或 `CXX`）要能给出 `libstdc++.a` 的绝对路径；
// - win32（MSVC）：ugrep 的 AVX512BW 文件用 `/arch:AVX512`，要 cl 19.23（VS 2019 16.3）或更新；
// - darwin：Xcode 的 clang++。
import { execFile } from "node:child_process";
import { stat } from "node:fs/promises";
import { isAbsolute, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";

const exec = promisify(execFile);

/** 认 `/arch:AVX512` 的最低 cl 版本（VS 2019 16.3）。 */
export const MIN_AVX512_CL = [19, 23];

/** `c++ -print-file-name=libstdc++.a` 的输出：找到时是绝对路径，找不到时 gcc 原样打印名字。 */
export function libstdcxxPath(output) {
  const path = output.trim();
  return isAbsolute(path) ? path : null;
}

/** cl 的横幅里的版本，例如 `Compiler Version 19.44.35207 for x64` → [19, 44, 35207]。 */
export function clVersion(banner) {
  const match = /Version (\d+)\.(\d+)\.(\d+)/u.exec(banner);
  return match ? match.slice(1).map(Number) : null;
}

/** 版本是否认 `/arch:AVX512`。 */
export function supportsAvx512(version) {
  const [major, minor] = version;
  return major > MIN_AVX512_CL[0] || (major === MIN_AVX512_CL[0] && minor >= MIN_AVX512_CL[1]);
}

async function isFile(path) {
  return (await stat(path).catch(() => null))?.isFile() ?? false;
}

async function checkLinux(env) {
  const cxx = env.CXX || "c++";
  const { stdout } = await exec(cxx, ["-print-file-name=libstdc++.a"]);
  const path = libstdcxxPath(stdout);
  if (!path || !(await isFile(path))) {
    throw new Error(
      `${cxx} has no static libstdc++ (libstdc++.a); install g++ and libstdc++-*-dev (Debian / Ubuntu)`,
    );
  }
  return `${cxx}: ${path}`;
}

async function checkWindows(env) {
  const vswhere = join(
    env["ProgramFiles(x86)"] || "C:\\Program Files (x86)",
    "Microsoft Visual Studio\\Installer\\vswhere.exe",
  );
  const { stdout } = await exec(vswhere, [
    "-latest",
    "-products",
    "*",
    "-requires",
    "Microsoft.VisualStudio.Component.VC.Tools.x86.x64",
    "-find",
    "VC\\Tools\\MSVC\\**\\bin\\Hostx64\\x64\\cl.exe",
  ]);
  const compilers = stdout.split(/\r?\n/u).filter(Boolean).sort();
  if (!compilers.length) throw new Error("No MSVC x64 compiler (cl.exe) found by vswhere");
  const cl = compilers.at(-1);
  // 不带参数的 cl 把横幅写到 stderr。
  const banner = await exec(cl, [], { windowsHide: true }).then(
    (result) => result.stderr,
    (error) => error.stderr ?? "",
  );
  const version = clVersion(banner);
  if (!version || !supportsAvx512(version)) {
    throw new Error(
      `${cl} is ${version?.join(".") ?? "unknown"}; /arch:AVX512 needs cl ${MIN_AVX512_CL.join(".")} (VS 2019 16.3) or newer`,
    );
  }
  return `${cl}: ${version.join(".")}`;
}

async function checkDarwin() {
  const { stdout } = await exec("xcrun", ["--find", "clang++"]);
  return `clang++: ${stdout.trim()}`;
}

export async function checkToolchain(platform = process.platform, env = process.env) {
  if (platform === "linux") return checkLinux(env);
  if (platform === "win32") return checkWindows(env);
  if (platform === "darwin") return checkDarwin();
  throw new Error(`No release toolchain for ${platform}`);
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  console.log(`embedded search toolchain: ${await checkToolchain()}`);
}
