/* ugrep 7.8.4 在 Windows（MSVC）上的 config.h（docs/specs/rust-cli/embedded-search.md §7.2）。手写：ugrep 在 Windows 上
   不跑 configure；内容取自上游 msvc/include_ugrep/config.h 与 msvc/ugrep.vcxproj 的预处理定义，库按 Unix 构建的同一组特性
   （-P:pcre2jit; -z:zlib,bzip2,zstd,brotli,7z）。SIMD 的 HAVE_AVX2 / HAVE_AVX512BW 由 build.rs 按架构给。 */
#define _CRT_SECURE_NO_WARNINGS
#define _CRT_NONSTDC_NO_WARNINGS
#define PCRE2_STATIC
#define WITH_COLOR
#define WITH_NO_INDENT
#define HAVE_PCRE2 1
#define HAVE_LIBZ 1
#define HAVE_LIBBZ2 1
#define HAVE_LIBZSTD 1
#define HAVE_LIBBROTLI 1
#define HAVE_7ZIP 1
#define PACKAGE "ugrep"
#define VERSION "7.8.4"
