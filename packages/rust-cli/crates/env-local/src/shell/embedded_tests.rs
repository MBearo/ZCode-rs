//! prelude 的原文（embedded-search.md §8.2）：逐行对照。

use std::path::PathBuf;

use super::*;

fn search(path: &str, find_and_grep: bool) -> EmbeddedSearch {
    EmbeddedSearch {
        executable: PathBuf::from(path),
        find_and_grep,
    }
}

const RG: &str = r#"# Check for rg availability
if ! hash rg 2>/dev/null && ! typeset -f rg >/dev/null 2>&1; then
  function rg {
  local _zc_bin="${ZCODE_EXECPATH:-}"
  [[ -x $_zc_bin ]] || _zc_bin='/opt/Z Code/zcode'
  if [[ ! -x $_zc_bin ]]; then command rg ${1+"$@"}; return; fi
  if [[ -n ${ZSH_VERSION:-} ]]; then
    ARGV0=rg "$_zc_bin" ${1+"$@"}
  elif [[ "$OSTYPE" == "msys" ]] || [[ "$OSTYPE" == "cygwin" ]] || [[ "$OSTYPE" == "win32" ]]; then
    ARGV0=rg "$_zc_bin" ${1+"$@"}
  else
    (exec -a rg "$_zc_bin" ${1+"$@"})
  fi
}
fi"#;

const FIND_GREP: &str = r#"# Shadow find/grep with embedded bfs/ugrep
unalias find 2>/dev/null || true
unalias grep 2>/dev/null || true
function find {
  local _zc_bin="${ZCODE_EXECPATH:-}"
  [[ -x $_zc_bin ]] || _zc_bin='/opt/Z Code/zcode'
  if [[ ! -x $_zc_bin ]]; then command find ${1+"$@"}; return; fi
  if [[ -n ${ZSH_VERSION:-} ]]; then
    ARGV0=bfs "$_zc_bin" -S dfs -regextype findutils-default ${1+"$@"}
  elif [[ "$OSTYPE" == "msys" ]] || [[ "$OSTYPE" == "cygwin" ]] || [[ "$OSTYPE" == "win32" ]]; then
    ARGV0=bfs "$_zc_bin" -S dfs -regextype findutils-default ${1+"$@"}
  else
    (exec -a bfs "$_zc_bin" -S dfs -regextype findutils-default ${1+"$@"})
  fi
}
function grep {
  local _zc_a
  for _zc_a in ${1+"$@"}; do
    case "$_zc_a" in -*-filter*|-*-pager*|-*-view*|-*-format-open*|-*-config*|---*|-@*|-*-save-config*|-[Zz]*|-[!-]*[Zz]*|--null|--null-data) command grep ${1+"$@"}; return ;; esac
  done
  local _zc_bin="${ZCODE_EXECPATH:-}"
  [[ -x $_zc_bin ]] || _zc_bin='/opt/Z Code/zcode'
  if [[ ! -x $_zc_bin ]]; then command grep ${1+"$@"}; return; fi
  if [[ -n ${ZSH_VERSION:-} ]]; then
    ARGV0=ugrep "$_zc_bin" -G --ignore-files --hidden -I --exclude-dir=.git --exclude-dir=.svn --exclude-dir=.hg --exclude-dir=.bzr --exclude-dir=.jj --exclude-dir=.sl ${1+"$@"}
  elif [[ "$OSTYPE" == "msys" ]] || [[ "$OSTYPE" == "cygwin" ]] || [[ "$OSTYPE" == "win32" ]]; then
    ARGV0=ugrep "$_zc_bin" -G --ignore-files --hidden -I --exclude-dir=.git --exclude-dir=.svn --exclude-dir=.hg --exclude-dir=.bzr --exclude-dir=.jj --exclude-dir=.sl ${1+"$@"}
  else
    (exec -a ugrep "$_zc_bin" -G --ignore-files --hidden -I --exclude-dir=.git --exclude-dir=.svn --exclude-dir=.hg --exclude-dir=.bzr --exclude-dir=.jj --exclude-dir=.sl ${1+"$@"})
  fi
}"#;

#[test]
fn the_unix_prelude_is_the_verbatim_text() {
    assert_eq!(
        prelude(&search("/opt/Z Code/zcode", true), false),
        format!("{RG}\n{FIND_GREP}")
    );
}

/// 启动参数点名了 Glob / Grep：只有 rg（快照里 rg 的一段照常）。
#[test]
fn without_find_and_grep_only_rg_is_defined() {
    assert_eq!(prelude(&search("/opt/Z Code/zcode", false), false), RG);
}

/// Git Bash：没有 bfs，不定义 find；回退路径写成 Git Bash 的形式。
#[test]
fn git_bash_has_grep_and_rg_but_no_find() {
    let text = prelude(&search(r"C:\Program Files\ZCode\zcode.exe", true), true);
    let rg = RG.replace("'/opt/Z Code/zcode'", "'/c/Program Files/ZCode/zcode.exe'");
    let grep_only: String = FIND_GREP
        .replace("'/opt/Z Code/zcode'", "'/c/Program Files/ZCode/zcode.exe'")
        .replace("unalias find 2>/dev/null || true\n", "");
    let start = grep_only.find("function find {").unwrap();
    let end = grep_only.find("function grep {").unwrap();
    let grep_only = format!("{}{}", &grep_only[..start], &grep_only[end..]);
    assert_eq!(text, format!("{rg}\n{grep_only}"));
    assert!(!text.contains("function find") && !text.contains("ARGV0=bfs"));
}

#[test]
fn paths_are_quoted_for_the_shell() {
    assert_eq!(quote("/usr/local/bin/zcode"), "/usr/local/bin/zcode");
    assert_eq!(quote(""), "''");
    assert_eq!(quote("/a b/zcode"), "'/a b/zcode'");
    assert_eq!(quote("/it's/zcode"), r#"'/it'"'"'s/zcode'"#);
    assert_eq!(quote("C:/x+y,z@w=v"), "C:/x+y,z@w=v");
}
