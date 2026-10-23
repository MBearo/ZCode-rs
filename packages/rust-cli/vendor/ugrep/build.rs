//! 编 ugrep 7.8.4（C++）与它用到的 7z 解码（ugrep 自带的 LZMA SDK）、brotli 解码（docs/specs/rust-cli/embedded-search.md
//! §7.2）。不跑 configure、不联网：`config.h` 按目标族取 `config/<族>.h`；PCRE2、zlib、bzip2、zstd 的静态库来自
//! pcre2-sys、libz-sys、bzip2-sys、zstd-sys（头文件目录由它们的 `links` 交给这里，PCRE2 用 vendor 的官方头文件）。
//! ugrep 的 `main()` / `wmain()` 改名，由 `shim.cpp` 给出 C 链接的入口。

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// RE/flex（上游 `lib/Makefile.am` 的 `libreflex_a_SOURCES`）。`*_avx2` / `*_avx512bw` 在 x86_64 上另编（运行时选）。
const REFLEX: &[&str] = &[
    "convert.cpp",
    "debug.cpp",
    "error.cpp",
    "input.cpp",
    "matcher.cpp",
    "pattern.cpp",
    "posix.cpp",
    "simd.cpp",
    "unicode.cpp",
    "utf8.cpp",
    "block_scripts.cpp",
    "language_scripts.cpp",
    "letter_scripts.cpp",
    "letter_case.cpp",
    "composer.cpp",
];
const REFLEX_AVX2: &[&str] = &["matcher_avx2.cpp", "simd_avx2.cpp"];
const REFLEX_AVX512BW: &[&str] = &["matcher_avx512bw.cpp", "simd_avx512bw.cpp"];

/// ugrep 本身（上游 `src/Makefile.am` 的 `ugrep_SOURCES`，`zopen.c` 是 C）。
const UGREP: &[&str] = &[
    "ugrep.cpp",
    "cnf.cpp",
    "glob.cpp",
    "output.cpp",
    "query.cpp",
    "screen.cpp",
    "stats.cpp",
    "vkey.cpp",
];

/// 7z 解码（上游 `lzma/C/Makefile.am` 的 `libviiz_a_SOURCES`）。
const VIIZ: &[&str] = &[
    "viizip.c",
    "7zAlloc.c",
    "7zArcIn.c",
    "7zBuf.c",
    "7zBuf2.c",
    "7zCrc.c",
    "7zCrcOpt.c",
    "7zDec.c",
    "7zFile.c",
    "7zStream.c",
    "Bcj2.c",
    "Bra.c",
    "Bra86.c",
    "BraIA64.c",
    "CpuArch.c",
    "Delta.c",
    "Lzma2Dec.c",
    "LzmaDec.c",
    "Ppmd7.c",
    "Ppmd7Dec.c",
];

fn var(name: &str) -> String {
    env::var(name).unwrap_or_default()
}

fn sources(dir: &Path, names: &[&str]) -> Vec<PathBuf> {
    names.iter().map(|name| dir.join(name)).collect()
}

/// 目录下的全部 `.c`（brotli 的 common / dec）。
fn c_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap_or_else(|error| panic!("读 {}：{error}", dir.display()))
        .map(|entry| entry.expect("目录项").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "c"))
        .collect();
    files.sort();
    files
}

/// 去掉 `__FILE__` 里的本机路径（docs/specs/rust-cli/linux.md §3.2 的检查）。
fn remap(build: &mut cc::Build, from: &Path, to: &str) {
    if build.get_compiler().is_like_msvc() {
        return;
    }
    let flag = format!("-ffile-prefix-map={}={to}", from.display());
    if build.is_flag_supported(&flag).unwrap_or(false) {
        build.flag(&flag);
    }
}

/// 所有 C / C++ 文件共用的设置。
fn base(here: &Path, out: &Path, includes: &[PathBuf], cpp: bool) -> cc::Build {
    let mut build = cc::Build::new();
    build.warnings(false).cpp(cpp);
    for dir in includes {
        build.include(dir);
    }
    build.define("HAVE_CONFIG_H", None).define("NDEBUG", None);
    if build.get_compiler().is_like_msvc() {
        // 上游源码是没有 BOM 的 UTF-8（TS 版的 Windows 构建同样要 /utf-8）。
        build.flag("/utf-8");
        if cpp {
            build.flag("/EHsc");
        }
    }
    if cpp {
        build.std("c++17");
    }
    remap(&mut build, here, "rust-cli-vendor-ugrep");
    remap(&mut build, out, "out");
    build
}

/// linux-gnu 上静态链接 libstdc++（embedded-search.md §7.2）：问 C++ 编译器 `libstdc++.a` 在哪里，返回它所在的目录。
/// `CXXSTDLIB` 设了时按 cc 的规则办（本机用 zig 交叉检查时设成 `c++`），返回 None。
fn static_libstdcxx(build: &cc::Build) -> Option<PathBuf> {
    if env::var_os("CXXSTDLIB").is_some()
        || env::var_os(format!("CXXSTDLIB_{}", var("TARGET").replace('-', "_"))).is_some()
    {
        return None;
    }
    let compiler = build.get_compiler();
    let output = Command::new(compiler.path())
        .arg("-print-file-name=libstdc++.a")
        .output()
        .unwrap_or_else(|error| {
            panic!("跑 {} -print-file-name：{error}", compiler.path().display())
        });
    let path = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    let dir = path
        .parent()
        .filter(|dir| path.is_absolute() && path.is_file() && !dir.as_os_str().is_empty());
    let Some(dir) = dir else {
        panic!(
            "rust-cli-vendor-ugrep：找不到 libstdc++.a（{} -print-file-name=libstdc++.a 给的是 {}）；装上 libstdc++ 的静态库（Debian / Ubuntu 的 libstdc++-*-dev）",
            compiler.path().display(),
            path.display()
        );
    };
    Some(dir.to_path_buf())
}

fn main() {
    let here = PathBuf::from(var("CARGO_MANIFEST_DIR"));
    for path in [
        "build.rs", "lib.rs", "shim.cpp", "config", "ugrep", "brotli", "pcre2",
    ] {
        println!("cargo:rerun-if-changed={path}");
    }
    println!("cargo:rerun-if-env-changed=CXXSTDLIB");
    let target = var("TARGET");
    let os = var("CARGO_CFG_TARGET_OS");
    let target_env = var("CARGO_CFG_TARGET_ENV");
    let arch = var("CARGO_CFG_TARGET_ARCH");
    let family = match (os.as_str(), target_env.as_str()) {
        ("macos", _) => "darwin",
        ("linux", "gnu") => "linux-gnu",
        ("linux", "musl") => "linux-musl",
        ("windows", "msvc") => "windows",
        (os, target_env) => panic!(
            "rust-cli-vendor-ugrep：没有 {os}-{target_env} 的 config.h；Unix 先用 scripts/rust-cli/vendor/ugrep-config.mjs 生成（embedded-search.md §7.2）"
        ),
    };
    let out = PathBuf::from(var("OUT_DIR"));
    let config_dir = out.join("ugrep-config");
    let pcre2_dir = out.join("pcre2");
    fs::create_dir_all(&config_dir).expect("建 OUT_DIR/ugrep-config");
    fs::create_dir_all(&pcre2_dir).expect("建 OUT_DIR/pcre2");
    fs::copy(
        here.join("config").join(format!("{family}.h")),
        config_dir.join("config.h"),
    )
    .unwrap_or_else(|error| panic!("复制 config/{family}.h：{error}"));
    // PCRE2 10.46 的官方头文件（与 pcre2-sys 0.2.10 自带的逐字相同）。
    fs::copy(
        here.join("pcre2/src/pcre2.h.generic"),
        pcre2_dir.join("pcre2.h"),
    )
    .expect("复制 pcre2.h");

    let upstream = here.join("ugrep");
    let brotli = here.join("brotli/c");
    let mut includes = vec![
        config_dir.clone(),
        upstream.join("include"),
        upstream.join("src"),
        upstream.join("lzma/C"),
        pcre2_dir.clone(),
        brotli.join("include"),
    ];
    for dependency in ["Z", "BZIP2", "ZSTD"] {
        let dir = env::var_os(format!("DEP_{dependency}_INCLUDE")).unwrap_or_else(|| {
            panic!("{dependency} 的 -sys crate 没有给出头文件目录（DEP_{dependency}_INCLUDE）")
        });
        includes.push(PathBuf::from(dir));
    }

    // SIMD：同上游 configure（x86_64：SSE2 打底，AVX2 / AVX512BW 的文件另编、运行时选；aarch64：NEON）。
    let x86 = arch == "x86_64";
    let simd: &[&str] = match arch.as_str() {
        "x86_64" => &["HAVE_AVX512BW"],
        "aarch64" => &["HAVE_NEON"],
        _ => &[],
    };
    // `ugrep --version` 的平台一行（configure 的 `PLATFORM=${host}`）。
    let platform = format!("\"{target}\"");
    let defines = |build: &mut cc::Build| {
        for name in simd {
            build.define(name, None);
        }
        build
            .define("WITH_NO_INDENT", None)
            .define("WITH_COLOR", None)
            .define("PCRE2_STATIC", None)
            .define("PCRE2_CODE_UNIT_WIDTH", "8");
        if x86 && !build.get_compiler().is_like_msvc() {
            build.flag("-msse2");
        }
    };

    let mut reflex = base(&here, &out, &includes, true);
    defines(&mut reflex);
    reflex
        .define("WITH_NO_CODEGEN", None)
        .files(sources(&upstream.join("lib"), REFLEX));
    let mut wide: Vec<(cc::Build, &str)> = Vec::new();
    if x86 {
        for (files, gnu, msvc, name) in [
            (REFLEX_AVX2, "-mavx2", "/arch:AVX2", "rust_cli_reflex_avx2"),
            (
                REFLEX_AVX512BW,
                "-mavx512bw",
                "/arch:AVX512",
                "rust_cli_reflex_avx512bw",
            ),
        ] {
            let mut build = base(&here, &out, &includes, true);
            defines(&mut build);
            let msvc_like = build.get_compiler().is_like_msvc();
            build.flag(if msvc_like { msvc } else { gnu });
            // LLVM 18 起 512 位的指令另要 `evex512`，zig 的 baseline CPU 不带（`-mavx512bw` 不再隐含它）；GCC 11 没有这个
            // 选项也不需要。
            if !msvc_like && gnu == "-mavx512bw" {
                build.flag_if_supported("-mevex512");
            }
            build
                .define("WITH_NO_CODEGEN", None)
                .files(sources(&upstream.join("lib"), files));
            wide.push((build, name));
        }
    }

    let mut ugrep = base(&here, &out, &includes, true);
    defines(&mut ugrep);
    ugrep
        .define("PLATFORM", platform.as_str())
        .define("GREP_PATH", "\"/usr/local/share/ugrep/patterns\"")
        // 改名，由 shim.cpp 调用（embedded-search.md §7.3）。
        .define("main", "rust_cli_ugrep_main_cpp")
        .define("wmain", "rust_cli_ugrep_wmain_cpp")
        .files(sources(&upstream.join("src"), UGREP))
        .file(here.join("shim.cpp"));

    let mut zopen = base(&here, &out, &includes, false);
    defines(&mut zopen);
    zopen.file(upstream.join("src/zopen.c"));

    let mut viiz = base(&here, &out, &includes, false);
    viiz.define("Z7_PPMD_SUPPORT", None)
        .define("Z7_EXTRACT_ONLY", None)
        .define("_REENTRANT", None)
        .define("_FILE_OFFSET_BITS", "64")
        .define("_LARGEFILE_SOURCE", None)
        .files(sources(&upstream.join("lzma/C"), VIIZ));

    let mut brotli_dec = base(&here, &out, &includes, false);
    brotli_dec
        .files(c_files(&brotli.join("common")))
        .files(c_files(&brotli.join("dec")));

    let libstdcxx = if family == "linux-gnu" {
        static_libstdcxx(&ugrep)
    } else {
        None
    };
    if libstdcxx.is_some() {
        ugrep.cpp_link_stdlib(None);
        reflex.cpp_link_stdlib(None);
        for (build, _) in &mut wide {
            build.cpp_link_stdlib(None);
        }
    }
    // 链接顺序：引用别人的在前（静态库按顺序解析）。
    ugrep.compile("rust_cli_ugrep");
    zopen.compile("rust_cli_ugrep_zopen");
    reflex.compile("rust_cli_reflex");
    for (build, name) in wide {
        build.compile(name);
    }
    viiz.compile("rust_cli_viiz");
    brotli_dec.compile("rust_cli_brotli_dec");
    if let Some(dir) = libstdcxx {
        println!("cargo:rustc-link-search=native={}", dir.display());
        println!("cargo:rustc-link-lib=static=stdc++");
    }
}
