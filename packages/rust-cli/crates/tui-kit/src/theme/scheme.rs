//! 具名主题的形状与用户的选择（docs/puffin-tui.md §4.2「主题」「存放」）。
//! 移植自 packages/tui/tui/src/zcode/tui/theme/Scheme.scala（feat/tui a2bf878b34）。
//!
//! 主题的数据是编译期常量（`schemes.rs`），所以这里的名字都是 `&'static str`，墨是 `(名字, Rgb)` 的静态切片：
//! 每侧最多八个，按名字线性查找。
use serde::Serialize;
use serde_json::Value;

use super::rgb::Rgb;
use super::schemes;

/// 一个具名主题的一侧：四个地色与它的墨。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    pub bg: Rgb,
    /// 配色自己的「抬起的面」：板与代码面板从它出发。
    pub surface: Rgb,
    pub fg: Rgb,
    /// 配色的注释色：它成为 fg3。
    pub muted: Rgb,
    /// 配色按自己的叫法命名的墨（Rosé Pine 的 foam、pine、love…）；[`Roles`] 与 [`SyntaxRoles`] 从这里取。
    pub inks: &'static [(&'static str, Rgb)],
}

impl Palette {
    /// 名为 `name` 的墨；没有这个名字是 None。
    pub fn ink(&self, name: &str) -> Option<Rgb> {
        self.inks.iter().find(|(n, _)| *n == name).map(|&(_, c)| c)
    }

    /// 有没有名为 `name` 的墨。
    pub fn has_ink(&self, name: &str) -> bool {
        self.inks.iter().any(|(n, _)| *n == name)
    }
}

/// 一侧与配色给它的名字（`Latte`、`Night Owl`、`Light`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Side {
    pub name: &'static str,
    pub palette: Palette,
}

/// 屏幕的八个墨色角色各用哪个墨。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Roles {
    pub brand: &'static str,
    pub code: &'static str,
    pub ok: &'static str,
    pub err: &'static str,
    pub skill: &'static str,
    pub file: &'static str,
    pub work: &'static str,
    pub agent: &'static str,
}

impl Roles {
    pub fn all(&self) -> [&'static str; 8] {
        [
            self.brand, self.code, self.ok, self.err, self.skill, self.file, self.work, self.agent,
        ]
    }
}

/// 代码块的语法角色各用哪个墨；注释永远是 fg3。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SyntaxRoles {
    pub keyword: &'static str,
    pub string: &'static str,
    pub number: &'static str,
    pub function: &'static str,
    /// 类型名（Scala 的 `typ`）。
    pub ty: &'static str,
    pub constant: &'static str,
    pub key: &'static str,
}

impl SyntaxRoles {
    pub fn all(&self) -> [&'static str; 7] {
        [
            self.keyword,
            self.string,
            self.number,
            self.function,
            self.ty,
            self.constant,
            self.key,
        ]
    }
}

/// 一个具名主题。两侧都是必填的：没有只有浅色或只有深色的主题。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Scheme {
    pub id: &'static str,
    pub name: &'static str,
    pub light: Side,
    pub dark: Side,
    pub roles: Roles,
    pub syntax: SyntaxRoles,
}

impl Scheme {
    pub fn side(&self, dark: bool) -> &Side {
        if dark { &self.dark } else { &self.light }
    }

    /// 列表里写的两侧：配色叫它们 Light 与 Dark 时是 `light · dark`，否则是它们自己的名字，浅色在前
    /// （`Latte · Mocha`）。
    pub fn sides(&self) -> String {
        if self.light.name == "Light" && self.dark.name == "Dark" {
            "light · dark".to_owned()
        } else {
            format!("{} · {}", self.light.name, self.dark.name)
        }
    }
}

/// 显示主题的哪一侧。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum ThemeMode {
    /// 按终端的背景，以及它的 DEC 2031 报告。
    #[default]
    Auto,
    Light,
    Dark,
}

impl ThemeMode {
    pub const ALL: [ThemeMode; 3] = [ThemeMode::Auto, ThemeMode::Light, ThemeMode::Dark];

    /// `theme.json` 里的写法：`auto`、`light`、`dark`。
    pub fn as_str(self) -> &'static str {
        match self {
            ThemeMode::Auto => "auto",
            ThemeMode::Light => "light",
            ThemeMode::Dark => "dark",
        }
    }

    /// [`ThemeMode::as_str`] 的反面；别的字都是 None（区分大小写，与 Scala 相同）。
    pub fn parse(s: &str) -> Option<ThemeMode> {
        Self::ALL.into_iter().find(|m| m.as_str() == s)
    }
}

/// 具名主题下谁涂屏幕的地。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Ground {
    /// 主题：它的 bg 与 fg 铺满屏幕与终端自己的边距。
    #[default]
    Theme,
    /// 终端：只用主题的墨。
    Terminal,
}

impl Ground {
    pub const ALL: [Ground; 2] = [Ground::Theme, Ground::Terminal];

    /// `theme.json` 里的写法：`theme`、`terminal`。
    pub fn as_str(self) -> &'static str {
        match self {
            Ground::Theme => "theme",
            Ground::Terminal => "terminal",
        }
    }

    /// [`Ground::as_str`] 的反面；别的字都是 None。
    pub fn parse(s: &str) -> Option<Ground> {
        Self::ALL.into_iter().find(|g| g.as_str() == s)
    }
}

/// 用户的选择（`theme.json`）；`theme` 是 [`schemes::ids`] 里的一个 id。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ThemeChoice {
    pub theme: &'static str,
    pub mode: ThemeMode,
    pub ground: Ground,
}

impl Default for ThemeChoice {
    fn default() -> Self {
        ThemeChoice {
            theme: schemes::TERMINAL,
            mode: ThemeMode::Auto,
            ground: Ground::Theme,
        }
    }
}

/// `theme.json` 的字段，按写出的次序：结构体的字段次序就是 JSON 里键的次序。
#[derive(Serialize)]
struct Encoded<'a> {
    theme: &'a str,
    mode: &'a str,
    ground: &'a str,
}

impl ThemeChoice {
    /// 选定 `theme`，其余是默认。
    pub fn of(theme: &'static str) -> ThemeChoice {
        ThemeChoice {
            theme,
            ..ThemeChoice::default()
        }
    }

    /// `{"theme":"gruvbox","mode":"auto","ground":"theme"}` 加换行。
    pub fn encode(&self) -> String {
        let fields = Encoded {
            theme: self.theme,
            mode: self.mode.as_str(),
            ground: self.ground.as_str(),
        };
        // 三个字段都是字符串，序列化不会失败；万一失败写出空对象，读回来是默认。
        let mut out = serde_json::to_string(&fields).unwrap_or_else(|_| "{}".to_owned());
        out.push('\n');
        out
    }

    /// `text` 里的选择。读不出的，或不认识的值，那个字段按默认（docs/puffin-tui.md §4.2「存放」）。
    pub fn decode(text: &str) -> ThemeChoice {
        let parsed: Option<Value> = serde_json::from_str(text).ok();
        let object = parsed.as_ref().and_then(Value::as_object);
        let field = |name: &str| object.and_then(|o| o.get(name)).and_then(Value::as_str);
        let d = ThemeChoice::default();
        ThemeChoice {
            // id 换成目录里的静态串：选择只持有目录认识的 id。
            theme: field("theme")
                .and_then(|t| schemes::ids().iter().copied().find(|id| *id == t))
                .unwrap_or(d.theme),
            mode: field("mode").and_then(ThemeMode::parse).unwrap_or(d.mode),
            ground: field("ground").and_then(Ground::parse).unwrap_or(d.ground),
        }
    }
}
