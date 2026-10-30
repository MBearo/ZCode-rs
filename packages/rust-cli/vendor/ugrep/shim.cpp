// ugrep 的入口（docs/specs/rust-cli/embedded-search.md §7.3）：ugrep.cpp 编译时 `-Dmain=rust_cli_ugrep_main_cpp`
// （Windows `-Dwmain=rust_cli_ugrep_wmain_cpp`），这里给 Rust 一个 C 链接的函数。改名后的函数仍是 C++ 链接，签名与
// ugrep.cpp 的定义相同。
#ifdef _WIN32
#include <wchar.h>
#if defined(__MINGW32__) || defined(__MINGW64__)
// MinGW（OS_WIN_MINGW 分支）：ugrep.cpp 的入口是无参的 `int main()`，宽参数由它内部用 CommandLineToArgvW 自取。
int rust_cli_ugrep_main_cpp();
extern "C" int rust_cli_ugrep_wmain(int argc, const wchar_t **wargv)
{
  (void)argc;
  (void)wargv;
  return rust_cli_ugrep_main_cpp();
}
#else
int rust_cli_ugrep_wmain_cpp(int argc, const wchar_t **wargv);
extern "C" int rust_cli_ugrep_wmain(int argc, const wchar_t **wargv)
{
  return rust_cli_ugrep_wmain_cpp(argc, wargv);
}
#endif
#else
int rust_cli_ugrep_main_cpp(int argc, const char **argv);
extern "C" int rust_cli_ugrep_main(int argc, const char **argv)
{
  return rust_cli_ugrep_main_cpp(argc, argv);
}
#endif
