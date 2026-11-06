//! 样式池：把 [`Style`]（带 [`Decor`]）intern 成小整数 id（格子存 32 位 id，不存颜色），并缓存每个样式的 SGR。
//! 单线程：归渲染器的帧循环所有。预热之后每个样式与切换都在缓存里，diff 热路径上发一次样式切换是一次数组读，不分配。
//! 移植自 packages/tui/tui/src/zcode/tui/render/StylePool.scala（feat/tui a2bf878b34）。
//!
//! 与 Scala 的不同（docs/specs/rust-cli/tui-kit.md §3.4）：池带一个颜色深度，intern 与换地时把颜色按深度降采样，缓存的
//! SGR 已是终端认得的颜色；格子与 diff 不知道深度。
use std::collections::HashMap;

use super::ansi;
use super::color::{Color, ColorDepth};
use super::style::{Decor, Style};

pub struct StylePool {
    // id 0 永远是 Style::DEFAULT（一个普通的重置）。
    ids: HashMap<u64, u32>,
    // 带 Decor 的样式另有一张表：Decor 放不进 Style 的 u64。
    decor_ids: HashMap<(u64, Decor), u32>,
    styles: Vec<Style>,
    links: Vec<String>,
    // 每个 id 的 decor，换地时重建序列；bench 的屏幕检查也按 id 读回它（docs/puffin-tui.md §7「渲染基准（L0）」）。
    decors: Vec<Decor>,
    // set_seqs[id] 是那个样式自含的 SGR，intern 时建一次。`transition` 与 `set_sequence` 都读它——串只取决于目标样式——
    // 所以 diff 热路径是一次数组读。
    set_seqs: Vec<String>,
    // link_seqs[id] 是 set_seqs[id] 加进入它的链接的 OSC 8（没有链接的样式则是离开任何链接）：跨链接变化的切换写的东西。
    link_seqs: Vec<String>,
    // 涂了的地（docs/puffin-tui.md §4.2「谁涂地」）：样式的默认前景与背景写成这两个颜色，id 0 也是。已按 `depth` 降采样。
    ground: Option<(Color, Color)>,
    // 终端的颜色深度：intern 与换地时按它降采样（tui-kit.md §3.4）。
    depth: ColorDepth,
}

impl Default for StylePool {
    fn default() -> Self {
        Self::new()
    }
}

impl StylePool {
    /// 真彩终端的池：颜色原样输出。
    pub fn new() -> StylePool {
        StylePool::with_depth(ColorDepth::TrueColor)
    }

    /// 颜色深度为 `depth` 的终端的池：intern 的样式与涂的地都按它降采样（tui-kit.md §3.4）。
    pub fn with_depth(depth: ColorDepth) -> StylePool {
        let mut pool = StylePool {
            ids: HashMap::new(),
            decor_ids: HashMap::new(),
            styles: Vec::new(),
            links: Vec::new(),
            decors: Vec::new(),
            set_seqs: Vec::new(),
            link_seqs: Vec::new(),
            ground: None,
            depth,
        };
        pool.ids.insert(Style::DEFAULT.raw(), 0);
        pool.styles.push(Style::DEFAULT);
        pool.links.push(String::new());
        pool.decors.push(Decor::PLAIN);
        pool.set_seqs.push(ansi::RESET.to_string()); // == Style::DEFAULT.set_sequence()（CSI 0m）
        pool.link_seqs
            .push(format!("{}{}", ansi::RESET, ansi::LINK_END));
        pool
    }

    /// 池的颜色深度。
    pub fn depth(&self) -> ColorDepth {
        self.depth
    }

    /// `s` 带 `d` 的 id，第一次见到时分配一个新的。不是真彩的池先把前景、背景与下划线色降采样再 intern：降成同一个样子的
    /// 样式共用一个 id（原来的键也记下，再见到时不必再降），存下的样式与 SGR 是降过的。
    pub fn intern(&mut self, s: Style, d: &Decor) -> u32 {
        if d.is_plain() {
            if let Some(&id) = self.ids.get(&s.raw()) {
                return id;
            }
            let r = self.reduce_style(s);
            let id = match self.ids.get(&r.raw()) {
                Some(&id) => id,
                None => {
                    let id = self.add(r, Decor::PLAIN);
                    self.ids.insert(r.raw(), id);
                    id
                }
            };
            self.ids.insert(s.raw(), id);
            id
        } else if let Some(&id) = self.decor_ids.get(&(s.raw(), d.clone())) {
            id
        } else {
            let (r, rd) = (self.reduce_style(s), self.reduce_decor(d));
            // 下划线色在 16 色上丢掉、又没有链接时，降过的 decor 是 plain：走没有 decor 的那张表。
            let id = if rd.is_plain() {
                self.intern(r, &rd)
            } else {
                match self.decor_ids.get(&(r.raw(), rd.clone())) {
                    Some(&id) => id,
                    None => {
                        let id = self.add(r, rd.clone());
                        self.decor_ids.insert((r.raw(), rd), id);
                        id
                    }
                }
            };
            self.decor_ids.insert((s.raw(), d.clone()), id);
            id
        }
    }

    /// `s` 的前景与背景按池的深度降采样；属性不动。
    fn reduce_style(&self, s: Style) -> Style {
        if self.depth == ColorDepth::TrueColor {
            return s;
        }
        Style::new(
            self.depth.reduce(s.fg_color()),
            self.depth.reduce(s.bg_color()),
            s.attrs(),
        )
    }

    /// `d` 的下划线色按池的深度降采样；链接不动。
    fn reduce_decor(&self, d: &Decor) -> Decor {
        Decor {
            underline: self.depth.reduce(d.underline),
            link: d.link.clone(),
        }
    }

    fn add(&mut self, s: Style, d: Decor) -> u32 {
        let id = self.styles.len();
        self.styles.push(s);
        self.links.push(d.link.clone());
        self.decors.push(d);
        self.set_seqs.push(String::new());
        self.link_seqs.push(String::new());
        self.build(id);
        id as u32
    }

    fn build(&mut self, id: usize) {
        let s = self.styles[id];
        let d = &self.decors[id];
        let painted = match self.ground {
            Some((fg, bg)) => {
                let f = if s.fg_color().is_default() {
                    s.with_fg(fg)
                } else {
                    s
                };
                if f.bg_color().is_default() {
                    f.with_bg(bg)
                } else {
                    f
                }
            }
            None => s,
        };
        let set = painted.set_sequence() + &d.sequence();
        let link = if d.link.is_empty() {
            ansi::LINK_END.to_string()
        } else {
            ansi::link_start(&d.link)
        };
        self.link_seqs[id] = set.clone() + &link;
        self.set_seqs[id] = set;
    }

    /// 从现在起把默认色写成 `g` 的（前景，背景），`None` 是终端自己的；两个颜色按池的深度降采样。每个 intern 过的样式的
    /// 序列都重建：格子的 id 不变，调用方整屏重画。
    pub fn reground(&mut self, g: Option<(Color, Color)>) {
        let g = g.map(|(fg, bg)| (self.depth.reduce(fg), self.depth.reduce(bg)));
        if g != self.ground {
            self.ground = g;
            for id in 0..self.styles.len() {
                self.build(id);
            }
        }
    }

    /// 样式 id 0（没有颜色）长什么样：一个普通的重置，或涂了地的颜色。擦除前先设它，擦出的格子就取它。
    pub fn base(&self) -> &str {
        &self.set_seqs[0]
    }

    pub fn style_of(&self, id: u32) -> Style {
        self.styles[id as usize]
    }

    /// 与样式 id `id` 一起 intern 的 [`Decor`]。
    pub fn decor_of(&self, id: u32) -> &Decor {
        &self.decors[id as usize]
    }

    /// 从样式 id `from` 到 `to` 的转义串；相等时为空。总是自含的 `to` 的重置加设置，与之前发过的属性无关；链接只在变化时
    /// 进入或离开。
    pub fn transition(&self, from: u32, to: u32) -> &str {
        if from == to {
            ""
        } else if self.links[from as usize] == self.links[to as usize] {
            &self.set_seqs[to as usize]
        } else {
            &self.link_seqs[to as usize]
        }
    }

    /// 从未知状态设样式 id `to`（整个重置加设置）。未知的意思是不在任何链接里：一帧从不在链接里结束（见 [`closing`]）。
    ///
    /// [`closing`]: StylePool::closing
    pub fn set_sequence(&self, to: u32) -> &str {
        if self.links[to as usize].is_empty() {
            &self.set_seqs[to as usize]
        } else {
            &self.link_seqs[to as usize]
        }
    }

    /// 在放弃屏幕状态之前（一帧的末尾、从重置重写的一行）离开样式 id `id` 的链接的串；没有链接时为空。
    pub fn closing(&self, id: u32) -> &str {
        if self.links.get(id as usize).is_some_and(|l| !l.is_empty()) {
            ansi::LINK_END
        } else {
            ""
        }
    }
}
