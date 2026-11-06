//! 24 位颜色：终端报告的（OSC 10/11）或我们派生的。移植自 packages/tui/tui/src/zcode/tui/Theme.scala 的 `Rgb`
//! （feat/tui a2bf878b34）。算式与 Scala 相同（JS 的 double 就是 f64），画面门禁靠它逐值相等。
use crate::render::Color;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Rgb {
        Rgb { r, g, b }
    }

    pub const fn color(self) -> Color {
        Color::TrueColor(self.r, self.g, self.b)
    }

    /// sRGB 里的线性混合：`t = 0` 是这个颜色，`t = 1` 是 `o`。每通道四舍五入，与设计稿用的算式相同。
    pub fn mix(self, o: Rgb, t: f64) -> Rgb {
        let ch = |a: u8, b: u8| -> u8 {
            let v = (f64::from(a) + (f64::from(b) - f64::from(a)) * t).round();
            v.clamp(0.0, 255.0) as u8
        };
        Rgb::new(ch(self.r, o.r), ch(self.g, o.g), ch(self.b, o.b))
    }

    /// 感知亮度（OKLab 的 L），0 黑，1 白。
    pub fn lightness(self) -> f64 {
        self.oklab().0
    }

    /// OKLab（L, a, b）。
    fn oklab(self) -> (f64, f64, f64) {
        let (lr, lg, lb) = (linear(self.r), linear(self.g), linear(self.b));
        let l = (0.412_221_470_8 * lr + 0.536_332_536_3 * lg + 0.051_445_992_9 * lb).cbrt();
        let m = (0.211_903_498_2 * lr + 0.680_699_545_1 * lg + 0.107_396_956_6 * lb).cbrt();
        let s = (0.088_302_461_9 * lr + 0.281_718_837_6 * lg + 0.629_978_700_5 * lb).cbrt();
        (
            0.210_454_255_3 * l + 0.793_617_785 * m - 0.004_072_046_8 * s,
            1.977_998_495_1 * l - 2.428_592_205 * m + 0.450_593_709_9 * s,
            0.025_904_037_1 * l + 0.782_771_766_2 * m - 0.808_675_766 * s,
        )
    }

    /// OKLCH（亮度、彩度、色相角 0–360）。
    pub fn to_oklch(self) -> (f64, f64, f64) {
        let (l, a, bb) = self.oklab();
        (l, a.hypot(bb), (bb.atan2(a).to_degrees() + 360.0) % 360.0)
    }

    /// WCAG 2 相对亮度。
    pub fn luminance(self) -> f64 {
        0.2126 * linear(self.r) + 0.7152 * linear(self.g) + 0.0722 * linear(self.b)
    }

    /// 与 `o` 的 WCAG 2 对比度，1 到 21。
    pub fn contrast(self, o: Rgb) -> f64 {
        let (x, y) = (self.luminance(), o.luminance());
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    /// `#rrggbb`。
    pub fn hex(self) -> String {
        format!("#{}", rust_cli_base::hash::hex(&[self.r, self.g, self.b]))
    }

    /// `#rrggbb`，大小写都行；别的是 None。
    pub fn parse_hex(s: &str) -> Option<Rgb> {
        let digits = s.strip_prefix('#')?;
        if digits.len() != 6 || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let ch = |i: usize| u8::from_str_radix(&digits[i..i + 2], 16).ok();
        Some(Rgb::new(ch(0)?, ch(2)?, ch(4)?))
    }

    /// OKLCH → sRGB，每通道在 gamma 之后夹到 0..255（与设计稿相同）。
    pub fn oklch(l: f64, c: f64, h: f64) -> Rgb {
        let a = c * h.to_radians().cos();
        let b = c * h.to_radians().sin();
        let l_ = l + 0.396_337_777_4 * a + 0.215_803_757_3 * b;
        let m_ = l - 0.105_561_345_8 * a - 0.063_854_172_8 * b;
        let s_ = l - 0.089_484_177_5 * a - 1.291_485_548 * b;
        let (ll, mm, ss) = (l_ * l_ * l_, m_ * m_ * m_, s_ * s_ * s_);
        let gamma = |x: f64| -> u8 {
            let v = if x <= 0.003_130_8 {
                12.92 * x
            } else {
                1.055 * x.powf(1.0 / 2.4) - 0.055
            };
            (v.clamp(0.0, 1.0) * 255.0).round() as u8
        };
        Rgb::new(
            gamma(4.076_741_662_1 * ll - 3.307_711_591_3 * mm + 0.230_969_929_2 * ss),
            gamma(-1.268_438_004_6 * ll + 2.609_757_401_1 * mm - 0.341_319_396_5 * ss),
            gamma(-0.004_196_086_3 * ll - 0.703_418_614_7 * mm + 1.707_614_701 * ss),
        )
    }
}

fn linear(c: u8) -> f64 {
    let x = f64::from(c) / 255.0;
    if x <= 0.04045 {
        x / 12.92
    } else {
        ((x + 0.055) / 1.055).powf(2.4)
    }
}

#[cfg(test)]
mod tests {
    use super::Rgb;

    #[test]
    fn hex_round_trips_and_rejects_bad_text() {
        assert_eq!(Rgb::parse_hex("#1A2b3C"), Some(Rgb::new(0x1a, 0x2b, 0x3c)));
        assert_eq!(Rgb::new(0x1a, 0x2b, 0x3c).hex(), "#1a2b3c");
        assert_eq!(Rgb::parse_hex("1a2b3c"), None);
        assert_eq!(Rgb::parse_hex("#1a2b3"), None);
        assert_eq!(Rgb::parse_hex("#1a2b3g"), None);
    }

    #[test]
    fn mix_lightness_and_contrast() {
        let black = Rgb::new(0, 0, 0);
        let white = Rgb::new(255, 255, 255);
        assert_eq!(black.mix(white, 0.5), Rgb::new(128, 128, 128));
        assert!(black.lightness() < 0.01 && white.lightness() > 0.99);
        assert!((white.contrast(black) - 21.0).abs() < 1e-9);
        // Scala 的 `math.round` 是四舍五入到正无穷；Rust 的 `round` 远离零。这里的值都非负，两边相同。
        assert_eq!(
            Rgb::new(10, 10, 10).mix(Rgb::new(11, 11, 11), 0.5),
            Rgb::new(11, 11, 11)
        );
    }

    #[test]
    fn oklch_round_trips_a_grey() {
        let (l, c, _) = Rgb::new(128, 128, 128).to_oklch();
        assert!(c < 1e-6);
        assert_eq!(Rgb::oklch(l, 0.0, 0.0), Rgb::new(128, 128, 128));
    }
}
