//! 内置主题（docs/puffin-tui.md §4.2「主题」），次序即 `/theme` 列表的次序，排在 Terminal 之后。色值是各配色自己
//! 发布的值（来源在设计稿的 `palettes.json`），数据在 `schemes/catalogue_{1,2}.rs`；加一个主题只是加一条数据。
//! 移植自 packages/tui/tui/src/zcode/tui/theme/Schemes.scala（feat/tui a2bf878b34）。
mod catalogue_1;
mod catalogue_2;

use super::rgb::Rgb;
use super::scheme::Scheme;

/// 颜色取自终端的主题：默认。
pub const TERMINAL: &str = "terminal";

/// 全部具名主题，按列表的次序。
static ALL: [Scheme; 17] = [
    catalogue_1::PUFFIN,
    catalogue_1::SOLARIZED,
    catalogue_1::GRUVBOX,
    catalogue_1::NORD,
    catalogue_1::DRACULA,
    catalogue_1::CATPPUCCIN,
    catalogue_1::TOKYONIGHT,
    catalogue_1::ROSEPINE,
    catalogue_2::ONE,
    catalogue_2::GITHUB,
    catalogue_2::EVERFOREST,
    catalogue_2::KANAGAWA,
    catalogue_2::AYU,
    catalogue_2::FLEXOKI,
    catalogue_2::MODUS,
    catalogue_2::SELENIZED,
    catalogue_2::NIGHTOWL,
];

/// 每个主题的 id，按列表的次序：Terminal 在先。与 [`ALL`] 的次序一致（schemes_tests 检查）。
static IDS: [&str; 18] = [
    TERMINAL,
    "puffin",
    "solarized",
    "gruvbox",
    "nord",
    "dracula",
    "catppuccin",
    "tokyonight",
    "rosepine",
    "one",
    "github",
    "everforest",
    "kanagawa",
    "ayu",
    "flexoki",
    "modus",
    "selenized",
    "nightowl",
];

/// 全部具名主题（不含 Terminal），按列表的次序。
pub fn all() -> &'static [Scheme] {
    &ALL
}

/// 每个主题的 id，按列表的次序：Terminal 在先。
pub fn ids() -> &'static [&'static str] {
    &IDS
}

/// id 为 `id` 的具名主题；Terminal 与不认识的 id 是 None。
pub fn by_id(id: &str) -> Option<&'static Scheme> {
    ALL.iter().find(|s| s.id == id)
}

/// 列表里写的主题名。不认识的 id 原样返回。
pub fn name(id: &str) -> &str {
    match by_id(id) {
        Some(s) => s.name,
        None if id == TERMINAL => "Terminal",
        None => id,
    }
}

/// `word` 说的主题：它的 id 或名字，不分大小写，去掉两端空白。
pub fn find(word: &str) -> Option<&'static str> {
    // 与 Java 的 `trim` 相同：去掉两端不大于空格的字符。
    let w = word.trim_matches(|c: char| c <= ' ').to_lowercase();
    IDS.iter()
        .copied()
        .find(|id| *id == w || name(id).to_lowercase() == w)
}

/// `#rrggbb` 的编译期解析：数据里写错的色值是编译错误，而不是运行时的 panic。
const fn h(s: &str) -> Rgb {
    let b = s.as_bytes();
    assert!(b.len() == 7 && b[0] == b'#', "主题色值必须是 #rrggbb");
    Rgb::new(byte(b[1], b[2]), byte(b[3], b[4]), byte(b[5], b[6]))
}

const fn byte(hi: u8, lo: u8) -> u8 {
    nibble(hi) * 16 + nibble(lo)
}

const fn nibble(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'f' => c - b'a' + 10,
        b'A'..=b'F' => c - b'A' + 10,
        _ => panic!("主题色值里有不是十六进制的字符"),
    }
}
