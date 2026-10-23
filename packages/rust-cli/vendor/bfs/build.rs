//! 编 bfs 4.1.4 与 Oniguruma 6.9.10（docs/specs/rust-cli/embedded-search.md §6.2）。不跑 configure、不联网：
//! bfs 的 `gen/config.h` 按目标族取 `config/<族>.h`（scripts/rust-cli/vendor/bfs-config.mjs 生成），`version.i` 等字符串
//! 与 Oniguruma 的 `config.h` 在这里写进 OUT_DIR。bfs 的 `main()` 改名为 `rust_cli_bfs_main`，由 lib.rs 调用。

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

/// bfs 的 configure 选项（`bfs --version` 的 CONFFLAGS 一行，embedded-search.md §6）。
const CONFFLAGS: &str = "--enable-release --with-oniguruma --without-libselinux --without-libacl --without-liburing --without-libcap";
const VERSION: &str = "4.1.4";
const ONIG_VERSION: &str = "6.9.10";

/// Oniguruma 的源码（上游 `src/Makefile.am` 的 `libonig_la_SOURCES`，不含 POSIX 接口；bfs 只用原生接口）。
const ONIG_SOURCES: &[&str] = &[
    "regparse.c",
    "regcomp.c",
    "regexec.c",
    "regenc.c",
    "regerror.c",
    "regext.c",
    "regsyntax.c",
    "regtrav.c",
    "regversion.c",
    "st.c",
    "reggnu.c",
    "unicode.c",
    "unicode_unfold_key.c",
    "unicode_fold1_key.c",
    "unicode_fold2_key.c",
    "unicode_fold3_key.c",
    "ascii.c",
    "utf8.c",
    "utf16_be.c",
    "utf16_le.c",
    "utf32_be.c",
    "utf32_le.c",
    "euc_jp.c",
    "euc_jp_prop.c",
    "sjis.c",
    "sjis_prop.c",
    "iso8859_1.c",
    "iso8859_2.c",
    "iso8859_3.c",
    "iso8859_4.c",
    "iso8859_5.c",
    "iso8859_6.c",
    "iso8859_7.c",
    "iso8859_8.c",
    "iso8859_9.c",
    "iso8859_10.c",
    "iso8859_11.c",
    "iso8859_13.c",
    "iso8859_14.c",
    "iso8859_15.c",
    "iso8859_16.c",
    "euc_tw.c",
    "euc_kr.c",
    "big5.c",
    "gb18030.c",
    "koi8_r.c",
    "cp1251.c",
    "onig_init.c",
];

fn var(name: &str) -> String {
    env::var(name).unwrap_or_default()
}

/// bfs 的 `build/embed.sh`：字符串加结尾的 NUL，写成 C 数组的初始化列表。
fn embed(text: &str) -> String {
    let mut out: String = text.bytes().map(|byte| format!("0x{byte:02x},")).collect();
    out.push_str("0x00,\n");
    out
}

/// 去掉 `__FILE__` 里的本机路径（bfs 的断言与 `bfs_bug` 消息用到它；docs/specs/rust-cli/linux.md §3.2 的检查）。
fn remap(build: &mut cc::Build, from: &Path, to: &str) {
    let flag = format!("-ffile-prefix-map={}={to}", from.display());
    if build.is_flag_supported(&flag).unwrap_or(false) {
        build.flag(&flag);
    }
}

fn write(path: &Path, text: &str) {
    fs::write(path, text).unwrap_or_else(|error| panic!("写 {}：{error}", path.display()));
}

fn main() {
    let here = PathBuf::from(var("CARGO_MANIFEST_DIR"));
    for path in ["build.rs", "lib.rs", "config", "bfs/src", "oniguruma/src"] {
        println!("cargo:rerun-if-changed={path}");
    }
    if var("CARGO_CFG_TARGET_FAMILY")
        .split(',')
        .all(|family| family != "unix")
    {
        // Windows 没有 bfs（embedded-search.md §6.4）。
        return;
    }
    let family = match (
        var("CARGO_CFG_TARGET_OS").as_str(),
        var("CARGO_CFG_TARGET_ENV").as_str(),
    ) {
        ("macos", _) => "darwin",
        ("linux", "gnu") => "linux-gnu",
        ("linux", "musl") => "linux-musl",
        (os, target_env) => panic!(
            "rust-cli-vendor-bfs：没有 {os}-{target_env} 的 config.h；先用 scripts/rust-cli/vendor/bfs-config.mjs 生成（embedded-search.md §6.2）"
        ),
    };
    let out = PathBuf::from(var("OUT_DIR"));
    let generated = out.join("bfs-gen");
    let onig_config = out.join("onig-config");
    fs::create_dir_all(&generated).expect("建 OUT_DIR/bfs-gen");
    fs::create_dir_all(&onig_config).expect("建 OUT_DIR/onig-config");

    // Oniguruma 的 config.h（上游 `src/config.h.cmake.in` 的项）：Unix 目标都有这些头文件；long 与指针同宽（LP64）。
    let width: usize = var("CARGO_CFG_TARGET_POINTER_WIDTH")
        .parse()
        .expect("指针宽度");
    let bytes = width / 8;
    write(
        &onig_config.join("config.h"),
        &format!(
            "#define HAVE_ALLOCA 1\n#define HAVE_ALLOCA_H 1\n#define HAVE_STDINT_H 1\n#define HAVE_SYS_TIMES_H 1\n\
             #define HAVE_SYS_TIME_H 1\n#define HAVE_SYS_TYPES_H 1\n#define HAVE_UNISTD_H 1\n#define HAVE_INTTYPES_H 1\n\
             #define PACKAGE \"onig\"\n#define PACKAGE_VERSION \"{ONIG_VERSION}\"\n#define VERSION \"{ONIG_VERSION}\"\n\
             #define SIZEOF_INT 4\n#define SIZEOF_LONG {bytes}\n#define SIZEOF_LONG_LONG 8\n#define SIZEOF_VOIDP {bytes}\n"
        ),
    );
    let onig_src = here.join("oniguruma/src");
    let mut onig = cc::Build::new();
    onig.warnings(false)
        .include(&onig_config)
        .include(&onig_src)
        .files(ONIG_SOURCES.iter().map(|file| onig_src.join(file)));
    remap(&mut onig, &here, "rust-cli-vendor-bfs");
    remap(&mut onig, &out, "out");

    // bfs：config.h 与 `version.c` 引用的几段字符串。编译器与参数的几行只是 `bfs --version` 的显示。
    let config = fs::read_to_string(here.join("config").join(format!("{family}.h")))
        .unwrap_or_else(|error| panic!("读 config/{family}.h：{error}"));
    write(&generated.join("config.h"), &config);
    let bfs_src = here.join("bfs/src");
    let prelude = bfs_src.join("prelude.h");
    let mut bfs = cc::Build::new();
    bfs.warnings(false)
        .include(&bfs_src)
        .include(&generated)
        .include(&onig_src)
        .define("NDEBUG", None)
        // bfs 的 `main()` 改名，由 lib.rs 调用（源码里别处的 `main` 只在注释里）。
        .define("main", "rust_cli_bfs_main")
        .flag("-include")
        .flag(prelude.to_str().expect("prelude.h 的路径是 UTF-8"))
        .flag("-pthread");
    remap(&mut bfs, &here, "rust-cli-vendor-bfs");
    remap(&mut bfs, &out, "out");
    // bfs 的 configure 依次试 C23、C2x、C17（build/flags/std.c）。
    for std in ["-std=c23", "-std=c2x", "-std=c17"] {
        if bfs.is_flag_supported(std).unwrap_or(false) {
            bfs.flag(std);
            break;
        }
    }
    let compiler = bfs.get_compiler();
    let strings = [
        ("version.i", VERSION.to_owned()),
        ("confflags.i", CONFFLAGS.to_owned()),
        // 只写编译器的文件名：完整路径是本机路径（交叉编译时是 zig 的包装脚本）。
        (
            "cc.i",
            compiler
                .path()
                .file_name()
                .map_or_else(String::new, |name| name.to_string_lossy().into_owned()),
        ),
        ("cppflags.i", "-include src/prelude.h -DNDEBUG".to_owned()),
        ("cflags.i", "-O3 -pthread".to_owned()),
        ("ldflags.i", String::new()),
        ("ldlibs.i", "-lonig".to_owned()),
    ];
    for (name, text) in strings {
        write(&generated.join(name), &embed(&text));
    }
    let mut sources: Vec<PathBuf> = fs::read_dir(&bfs_src)
        .expect("读 bfs/src")
        .map(|entry| entry.expect("bfs/src 的项").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "c"))
        .collect();
    sources.sort();
    bfs.files(sources);
    // 链接顺序：bfs 引用 Oniguruma，先给 bfs。
    bfs.compile("rust_cli_bfs");
    onig.compile("rust_cli_onig");
}
