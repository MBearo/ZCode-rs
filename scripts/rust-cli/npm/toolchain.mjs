// 编内嵌的搜索工具要的 C / C++ 工具链（docs/specs/rust-cli/embedded-search.md §7.2、§9.3）：发版与检查的 workflow 在
// cargo 之前跑一次，缺了就在这里报错，不等 build.rs 编到一半才失败。
// - linux-gnu：ugrep 静态链接 libstdc++，C++ 编译器（cc 的缺省 `c++`，或 `CXX`）要能给出 `libstdc++.a` 的绝对路径；
// - win32 x64（MSVC）：ugrep 的 AVX512BW 文件用 `/arch:AVX512`，要 cl 19.23（VS 2019 16.3）或更新；
// - win32 arm64（MSVC，工作线 F111）：ugrep 走 NEON、不编 AVX 的文件，只要 ARM64 原生的 cl；ring 在这个 target 上用
//   `clang` 编 C 与汇编，要 `clang` 在 PATH 里；
// - darwin：Xcode 的 clang++。
import { execFile } from "node:child_process";
import { stat } from "node:fs/promises";
import { isAbsolute, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";

const exec = promisify(execFile);

/** 认 `/arch:AVX512` 的最低 cl 版本（VS 2019 16.3）。 */
export const MIN_AVX512_CL = [19, 23];

/** 各架构的 runner 上找哪个 MSVC 编译器（vswhere 的组件与路径），以及要不要核对 `/arch:AVX512`。 */
const MSVC = {
  x64: {
    component: "Microsoft.VisualStudio.Component.VC.Tools.x86.x64",
    path: "VC\\Tools\\MSVC\\**\\bin\\Hostx64\\x64\\cl.exe",
    avx512: true,
  },
  arm64: {
    component: "Microsoft.VisualStudio.Component.VC.Tools.ARM64",
    path: "VC\\Tools\\MSVC\\**\\bin\\Hostarm64\\arm64\\cl.exe",
    avx512: false,
  },
};

/** runner 架构（Node 的 `process.arch`）对应的 MSVC 编译器。 */
export function msvcCompiler(arch) {
  const compiler = MSVC[arch];
  if (!compiler) throw new Error(`No MSVC release toolchain for ${arch}`);
  return compiler;
}

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

async function checkWindows(env, arch) {
  const compiler = msvcCompiler(arch);
  const vswhere = join(
    env["ProgramFiles(x86)"] || "C:\\Program Files (x86)",
    "Microsoft Visual Studio\\Installer\\vswhere.exe",
  );
  const { stdout } = await exec(vswhere, [
    "-latest",
    "-products",
    "*",
    "-requires",
    compiler.component,
    "-find",
    compiler.path,
  ]);
  const compilers = stdout.split(/\r?\n/u).filter(Boolean).sort();
  if (!compilers.length) throw new Error(`No MSVC ${arch} compiler (cl.exe) found by vswhere`);
  const cl = compilers.at(-1);
  // 不带参数的 cl 把横幅写到 stderr。
  const banner = await exec(cl, [], { windowsHide: true }).then(
    (result) => result.stderr,
    (error) => error.stderr ?? "",
  );
  const version = clVersion(banner);
  if (!version) throw new Error(`${cl} printed no version banner`);
  if (!compiler.avx512) return `${cl}: ${version.join(".")}; ${await checkClang()}`;
  if (!supportsAvx512(version)) {
    throw new Error(
      `${cl} is ${version.join(".")}; /arch:AVX512 needs cl ${MIN_AVX512_CL.join(".")} (VS 2019 16.3) or newer`,
    );
  }
  return `${cl}: ${version.join(".")}`;
}

/** ring 在 Windows ARM64 上要的 clang（runner 镜像带 LLVM）：取 `clang --version` 的第一行。 */
async function checkClang() {
  const { stdout } = await exec("clang", ["--version"], { windowsHide: true }).catch(() => {
    throw new Error("ring needs clang on Windows ARM64; install LLVM and put clang on PATH");
  });
  return `clang: ${stdout.split(/\r?\n/u)[0]}`;
}

async function checkDarwin() {
  const { stdout } = await exec("xcrun", ["--find", "clang++"]);
  return `clang++: ${stdout.trim()}`;
}

export async function checkToolchain(
  platform = process.platform,
  env = process.env,
  arch = process.arch,
) {
  if (platform === "linux") return checkLinux(env);
  if (platform === "win32") return checkWindows(env, arch);
  if (platform === "darwin") return checkDarwin();
  throw new Error(`No release toolchain for ${platform}`);
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  console.log(`embedded search toolchain: ${await checkToolchain()}`);
}
