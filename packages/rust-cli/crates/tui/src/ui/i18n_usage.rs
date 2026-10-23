//! `/usage` 页面的文字（docs/specs/rust-cli/usage.md §4、§5）：英文照原文（产品名换成 ZCode），
//! 中文按同样的结构。
use super::i18n::Lang;

/// 一种语言的固定文字。
pub struct UsageStrings {
    pub title: &'static str,
    pub usage_tab: &'static str,
    pub stats_tab: &'static str,
    pub session: &'static str,
    pub esc_cancel: &'static str,
    pub overview: &'static str,
    pub models: &'static str,
    pub all_time: &'static str,
    pub last_7: &'static str,
    pub last_30: &'static str,
    pub favorite: &'static str,
    pub total_tokens: &'static str,
    pub sessions: &'static str,
    pub longest_session: &'static str,
    pub active_days: &'static str,
    pub longest_streak: &'static str,
    pub most_active: &'static str,
    pub current_streak: &'static str,
    pub less: &'static str,
    pub more: &'static str,
    pub empty: &'static str,
    pub loading: &'static str,
    pub no_models: &'static str,
    pub usage_hint: &'static str,
    pub stats_hint: &'static str,
}

const EN: UsageStrings = UsageStrings {
    title: "Usage",
    usage_tab: "Usage",
    stats_tab: "Stats",
    session: "Session",
    esc_cancel: "{Esc} to cancel",
    overview: "Overview",
    models: "Models",
    all_time: "All time",
    last_7: "Last 7 days",
    last_30: "Last 30 days",
    favorite: "Favorite model:",
    total_tokens: "Total tokens:",
    sessions: "Sessions:",
    longest_session: "Longest session:",
    active_days: "Active days:",
    longest_streak: "Longest streak:",
    most_active: "Most active day:",
    current_streak: "Current streak:",
    less: "Less",
    more: "More",
    empty: "No stats available yet. Start using ZCode!",
    loading: "Loading your ZCode stats\u{2026}",
    no_models: "No model usage data available",
    usage_hint: "{tab} to switch tabs",
    stats_hint: "{tab} to switch tabs \u{b7} {\u{2190}/\u{2192}} to switch views \u{b7} {r} to cycle dates \u{b7} {esc} to close",
};

const ZH: UsageStrings = UsageStrings {
    title: "用量",
    usage_tab: "用量",
    stats_tab: "统计",
    session: "会话",
    esc_cancel: "{Esc} 取消",
    overview: "概览",
    models: "模型",
    all_time: "全部",
    last_7: "最近 7 天",
    last_30: "最近 30 天",
    favorite: "最常用的模型：",
    total_tokens: "总 token：",
    sessions: "会话数：",
    longest_session: "最长的会话：",
    active_days: "活跃天数：",
    longest_streak: "最长连续：",
    most_active: "最活跃的一天：",
    current_streak: "当前连续：",
    less: "少",
    more: "多",
    empty: "还没有统计数据。开始使用 ZCode 吧！",
    loading: "正在读取 ZCode 的统计\u{2026}",
    no_models: "没有模型用量数据",
    usage_hint: "{tab} 切换标签页",
    stats_hint: "{tab} 切换标签页 \u{b7} {\u{2190}/\u{2192}} 切换视图 \u{b7} {r} 切换日期范围 \u{b7} {esc} 关闭",
};

pub fn strings(lang: Lang) -> &'static UsageStrings {
    match lang {
        Lang::En => &EN,
        Lang::Zh => &ZH,
    }
}

/// `N day(s)` / `N 天`。
pub fn days(lang: Lang, n: u64) -> String {
    match lang {
        Lang::En if n == 1 => "1 day".to_owned(),
        Lang::En => format!("{n} days"),
        Lang::Zh => format!("{n} 天"),
    }
}

/// `toLocaleDateString("en-US", {month: "short", day: "numeric"})`；中文 `10月3日`。
pub fn short_date(lang: Lang, month: u32, day: u32) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    match lang {
        Lang::En => format!(
            "{} {day}",
            MONTHS[usize::try_from(month.saturating_sub(1))
                .unwrap_or(0)
                .min(11)]
        ),
        Lang::Zh => format!("{month}月{day}日"),
    }
}

/// `Input X · Output Y · Cache read Z · Cache write W`。
pub fn token_line(lang: Lang, values: [&str; 4]) -> String {
    let [input, output, read, write] = values;
    match lang {
        Lang::En => {
            format!(
                "Input {input} \u{b7} Output {output} \u{b7} Cache read {read} \u{b7} Cache write {write}"
            )
        }
        Lang::Zh => {
            format!("输入 {input} \u{b7} 输出 {output} \u{b7} 缓存读 {read} \u{b7} 缓存写 {write}")
        }
    }
}

/// Models 视图的两行明细。
pub fn model_lines(lang: Lang, values: [&str; 4]) -> (String, String) {
    let [input, output, read, write] = values;
    match lang {
        Lang::En => (
            format!("  In: {input} \u{b7} Out: {output}"),
            format!("  Cache: {read} read \u{b7} {write} write"),
        ),
        Lang::Zh => (
            format!("  输入：{input} \u{b7} 输出：{output}"),
            format!("  缓存：读 {read} \u{b7} 写 {write}"),
        ),
    }
}

/// Models 视图超过 4 个模型时的位置行（`↑ ↓ 1-4 of 6 models (↑/↓ to scroll)`）。
pub fn models_window(lang: Lang, first: usize, last: usize, total: usize) -> String {
    match lang {
        Lang::En => format!("{first}-{last} of {total} models (\u{2191}/\u{2193} to scroll)"),
        Lang::Zh => format!("第 {first}-{last} 个，共 {total} 个模型（\u{2191}/\u{2193} 滚动）"),
    }
}

/// 书名与 token 数。
pub const BOOKS: [(&str, &str, u64); 24] = [
    ("The Little Prince", "《小王子》", 22_000),
    ("The Old Man and the Sea", "《老人与海》", 35_000),
    ("A Christmas Carol", "《圣诞颂歌》", 37_000),
    ("Animal Farm", "《动物农场》", 39_000),
    ("Fahrenheit 451", "《华氏 451》", 60_000),
    ("The Great Gatsby", "《了不起的盖茨比》", 62_000),
    ("Slaughterhouse-Five", "《五号屠场》", 64_000),
    ("Brave New World", "《美丽新世界》", 83_000),
    ("The Catcher in the Rye", "《麦田里的守望者》", 95_000),
    (
        "Harry Potter and the Philosopher's Stone",
        "《哈利·波特与魔法石》",
        103_000,
    ),
    ("The Hobbit", "《霍比特人》", 123_000),
    ("1984", "《一九八四》", 123_000),
    ("To Kill a Mockingbird", "《杀死一只知更鸟》", 130_000),
    ("Pride and Prejudice", "《傲慢与偏见》", 156_000),
    ("Dune", "《沙丘》", 244_000),
    ("Moby-Dick", "《白鲸》", 268_000),
    ("Crime and Punishment", "《罪与罚》", 274_000),
    ("A Game of Thrones", "《权力的游戏》", 381_000),
    ("Anna Karenina", "《安娜·卡列尼娜》", 468_000),
    ("Don Quixote", "《堂吉诃德》", 520_000),
    ("The Lord of the Rings", "《指环王》", 576_000),
    ("The Count of Monte Cristo", "《基督山伯爵》", 603_000),
    ("Les Mis\u{e9}rables", "《悲惨世界》", 689_000),
    ("War and Peace", "《战争与和平》", 730_000),
];

/// 活动与分钟数。
pub const ACTIVITIES: [(&str, &str, u64); 10] = [
    ("a TED talk", "一场 TED 演讲", 18),
    ("an episode of The Office", "一集《办公室》", 22),
    ("listening to Abbey Road", "听完一张《艾比路》", 47),
    ("a yoga class", "一节瑜伽课", 60),
    ("a World Cup soccer match", "一场世界杯足球赛", 90),
    (
        "a half marathon (average time)",
        "一次半程马拉松（平均用时）",
        120,
    ),
    ("the movie Inception", "电影《盗梦空间》", 148),
    ("watching Titanic", "看完《泰坦尼克号》", 195),
    ("a transatlantic flight", "一次跨大西洋航班", 420),
    ("a full night of sleep", "一整夜的睡眠", 480),
];

/// 对比句的三种写法。
pub fn tokens_times(lang: Lang, times: u64, book: (&str, &str)) -> String {
    match lang {
        Lang::En => format!(
            "Your input and output are ~{times}x the tokens in {}",
            book.0
        ),
        Lang::Zh => format!("你的输入与输出约是{}的 {times} 倍 token", book.1),
    }
}

pub fn tokens_about(lang: Lang, book: (&str, &str)) -> String {
    match lang {
        Lang::En => format!(
            "Your input and output are about as many tokens as {}",
            book.0
        ),
        Lang::Zh => format!("你的输入与输出和{}的 token 数差不多", book.1),
    }
}

pub fn session_times(lang: Lang, times: u64, activity: (&str, &str)) -> String {
    match lang {
        Lang::En => format!(
            "Your longest session is ~{times}x longer than {}",
            activity.0
        ),
        Lang::Zh => format!("你最长的会话约是{}的 {times} 倍长", activity.1),
    }
}
