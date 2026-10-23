// 复制自 apps/zcode-host/crates/tui/src/ui/highlight.rs（feat/rs 79b6691c55）。
//! 代码高亮（docs/specs/rust-tui.md §8.6）：深色背景用 Catppuccin Mocha，浅色用 Latte。
//! 16 色终端不高亮。语法与主题集合在第一次使用时加载一次。
use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::rc::Rc;
use std::sync::OnceLock;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, Theme};
use syntect::parsing::SyntaxSet;
use two_face::theme::{EmbeddedLazyThemeSet, EmbeddedThemeName};

use super::palette::{ColorLevel, Palette, nearest_256};

fn syntaxes() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(two_face::syntax::extra_newlines)
}

fn themes() -> &'static EmbeddedLazyThemeSet {
    static SET: OnceLock<EmbeddedLazyThemeSet> = OnceLock::new();
    SET.get_or_init(two_face::theme::extra)
}

fn theme(light: bool) -> &'static Theme {
    themes().get(if light {
        EmbeddedThemeName::CatppuccinLatte
    } else {
        EmbeddedThemeName::CatppuccinMocha
    })
}

fn color(c: syntect::highlighting::Color, level: ColorLevel) -> Option<Color> {
    match level {
        ColorLevel::TrueColor => Some(Color::Rgb(c.r, c.g, c.b)),
        ColorLevel::Ansi256 => Some(Color::Indexed(nearest_256((c.r, c.g, c.b)))),
        ColorLevel::Ansi16 => None,
    }
}

/// 一段代码的高亮结果：每行一组片段。
pub type Highlighted = Rc<Vec<Vec<Span<'static>>>>;

/// 记住的代码原文总字节数上限；超过时整体清空。
const MEMO_BYTES: usize = 4 * 1024 * 1024;

/// 高亮结果按（代码、语言、明暗、色彩级别）记忆。流式中的回复每帧重排，原来每帧把已闭合的代码块
/// 重新高亮，占流式 CPU 的 85%，总成本随回复长度平方增长（docs/specs/rust-tui.md §8.1）。高亮是这几项的
/// 纯函数，记住结果即可。绘制只在主线程，所以按线程存放。
#[derive(Default)]
struct Memo {
    /// 按内容哈希分桶；桶里比较完整的键。
    entries: HashMap<u64, Vec<Remembered>>,
    bytes: usize,
}

/// 一条记忆：（代码，语言，明暗，真彩色）与结果。
struct Remembered {
    code: String,
    language: String,
    light: bool,
    true_color: bool,
    result: Option<Highlighted>,
}

thread_local! {
    static MEMO: RefCell<Memo> = RefCell::default();
}

/// 逐行高亮；不认识的语言或 16 色终端返回 None，调用方按原文显示。每行不含换行符。
pub fn highlight(code: &str, language: &str, palette: &Palette) -> Option<Highlighted> {
    if palette.level == ColorLevel::Ansi16 {
        return None;
    }
    let language = language.trim();
    let true_color = palette.level == ColorLevel::TrueColor;
    let mut hasher = DefaultHasher::new();
    (code, language, palette.light, true_color).hash(&mut hasher);
    let hash = hasher.finish();
    let same = |e: &&Remembered| {
        e.code == code
            && e.language == language
            && e.light == palette.light
            && e.true_color == true_color
    };
    if let Some(found) = MEMO.with_borrow(|memo| {
        memo.entries
            .get(&hash)
            .and_then(|bucket| bucket.iter().find(same))
            .map(|e| e.result.clone())
    }) {
        return found;
    }
    let result = highlight_uncached(code, language, palette).map(Rc::new);
    MEMO.with_borrow_mut(|memo| {
        memo.bytes += code.len();
        if memo.bytes > MEMO_BYTES {
            memo.entries.clear();
            memo.bytes = code.len();
        }
        memo.entries.entry(hash).or_default().push(Remembered {
            code: code.to_owned(),
            language: language.to_owned(),
            light: palette.light,
            true_color,
            result: result.clone(),
        });
    });
    result
}

fn highlight_uncached(
    code: &str,
    language: &str,
    palette: &Palette,
) -> Option<Vec<Vec<Span<'static>>>> {
    let set = syntaxes();
    let syntax = set
        .find_syntax_by_token(language)
        .or_else(|| set.find_syntax_by_extension(language))?;
    let mut lines = HighlightLines::new(syntax, theme(palette.light));
    let mut out = Vec::new();
    for line in syntect::util::LinesWithEndings::from(code) {
        let ranges = lines.highlight_line(line, set).ok()?;
        out.push(
            ranges
                .into_iter()
                .map(|(style, text)| {
                    let mut s = Style::default();
                    if let Some(fg) = color(style.foreground, palette.level) {
                        s = s.fg(fg);
                    }
                    if style.font_style.contains(FontStyle::BOLD) {
                        s = s.add_modifier(Modifier::BOLD);
                    }
                    if style.font_style.contains(FontStyle::ITALIC) {
                        s = s.add_modifier(Modifier::ITALIC);
                    }
                    Span::styled(text.trim_end_matches(['\n', '\r']).to_owned(), s)
                })
                .filter(|span| !span.content.is_empty())
                .collect(),
        );
    }
    Some(out)
}

/// 主题里某类记号的前景色（取第一个有颜色的 scope），饱和度降到 85%；16 色终端返回 None。
/// status line 用它给模型与目录着色。
pub fn scope_color(scopes: &[&str], palette: &Palette) -> Option<Color> {
    if palette.level == ColorLevel::Ansi16 {
        return None;
    }
    let highlighter = syntect::highlighting::Highlighter::new(theme(palette.light));
    let c = scopes.iter().find_map(|scope| {
        let stack: syntect::parsing::ScopeStack = scope.parse().ok()?;
        let style = highlighter.style_mod_for_stack(stack.as_slice());
        style.foreground
    })?;
    let luma = 0.299 * f64::from(c.r) + 0.587 * f64::from(c.g) + 0.114 * f64::from(c.b);
    let soften = |v: u8| {
        (luma + (f64::from(v) - luma) * 0.85)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    color(
        syntect::highlighting::Color {
            r: soften(c.r),
            g: soften(c.g),
            b: soften(c.b),
            a: c.a,
        },
        palette.level,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_languages_are_colored_and_plain_terminals_are_not() {
        let palette = Palette::new(ColorLevel::TrueColor, Some((0, 0, 0)));
        let lines = highlight("fn main() {}\nlet x = 1;", "rust", &palette).unwrap();
        assert_eq!(lines.len(), 2);
        assert!(
            lines[0]
                .iter()
                .any(|span| matches!(span.style.fg, Some(Color::Rgb(..))))
        );
        assert!(highlight("x", "no-such-language", &palette).is_none());
        assert!(highlight("x", "rust", &Palette::new(ColorLevel::Ansi16, None)).is_none());
    }

    /// 流式中的回复每帧重排：同一段代码只高亮一次，之后共享结果（docs/specs/rust-tui.md §8.1）。
    #[test]
    fn the_same_code_is_highlighted_once_and_shared() {
        let dark = Palette::new(ColorLevel::TrueColor, Some((0, 0, 0)));
        let light = Palette::new(ColorLevel::TrueColor, Some((255, 255, 255)));
        let code = "fn shared() -> u64 {\n    1\n}";
        let first = highlight(code, "rust", &dark).unwrap();
        let again = highlight(code, "rust", &dark).unwrap();
        assert!(std::rc::Rc::ptr_eq(&first, &again));
        let other_theme = highlight(code, "rust", &light).unwrap();
        assert!(!std::rc::Rc::ptr_eq(&first, &other_theme));
        let other_code = highlight("fn other() {}", "rust", &dark).unwrap();
        assert!(!std::rc::Rc::ptr_eq(&first, &other_code));
    }
}
