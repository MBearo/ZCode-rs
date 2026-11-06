//! 设置页输入框里的一行字（搜索与键入的值；照 Puffin `shell/draft.rs` 的单行部分）：文字与光标（字节下标，总在字素的
//! 边界上）。值只有一行：粘贴的换行换成空格。
use unicode_segmentation::UnicodeSegmentation;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Field {
    pub text: String,
    pub cursor: usize,
}

impl Field {
    pub fn new(text: impl Into<String>, cursor: usize) -> Field {
        let text = text.into();
        let cursor = cursor.min(text.len());
        Field { text, cursor }
    }

    pub fn at_end(text: impl Into<String>) -> Field {
        let text = text.into();
        let cursor = text.len();
        Field { text, cursor }
    }

    /// 字素的边界（含 0 与末尾）。
    fn bounds(&self) -> Vec<usize> {
        let mut out: Vec<usize> = self.text.grapheme_indices(true).map(|(i, _)| i).collect();
        out.push(self.text.len());
        out
    }

    pub fn insert(&self, s: &str) -> Field {
        let s = s.replace("\r\n", " ").replace(['\n', '\r'], " ");
        let mut text = self.text.clone();
        text.insert_str(self.cursor, &s);
        Field::new(text, self.cursor + s.len())
    }

    fn before(&self) -> usize {
        self.bounds()
            .into_iter()
            .rev()
            .find(|b| *b < self.cursor)
            .unwrap_or(0)
    }

    fn after(&self) -> usize {
        self.bounds()
            .into_iter()
            .find(|b| *b > self.cursor)
            .unwrap_or(self.text.len())
    }

    pub fn left(&self) -> Field {
        Field::new(self.text.clone(), self.before())
    }

    pub fn right(&self) -> Field {
        Field::new(self.text.clone(), self.after())
    }

    pub fn backspace(&self) -> Field {
        let from = self.before();
        let mut text = self.text.clone();
        text.replace_range(from..self.cursor, "");
        Field::new(text, from)
    }

    pub fn delete_forward(&self) -> Field {
        let to = self.after();
        let mut text = self.text.clone();
        text.replace_range(self.cursor..to, "");
        Field::new(text, self.cursor)
    }

    pub fn to_start(&self) -> Field {
        Field::new(self.text.clone(), 0)
    }

    pub fn to_end(&self) -> Field {
        Field::at_end(self.text.clone())
    }

    pub fn cut_to_start(&self) -> Field {
        Field::new(self.text[self.cursor..].to_owned(), 0)
    }

    pub fn cut_to_end(&self) -> Field {
        Field::new(self.text[..self.cursor].to_owned(), self.cursor)
    }

    /// 光标前一个词的开头（先跳过空白）。
    fn word_start(&self) -> usize {
        let head = &self.text[..self.cursor];
        let trimmed = head.trim_end();
        trimmed.rfind(char::is_whitespace).map_or(0, |i| {
            i + trimmed[i..].chars().next().map_or(1, char::len_utf8)
        })
    }

    /// 光标后一个词的末尾（先跳过空白）。
    fn word_end(&self) -> usize {
        let tail = &self.text[self.cursor..];
        let lead = tail.len() - tail.trim_start().len();
        let rest = &tail[lead..];
        self.cursor + lead + rest.find(char::is_whitespace).unwrap_or(rest.len())
    }

    pub fn word_left(&self) -> Field {
        Field::new(self.text.clone(), self.word_start())
    }

    pub fn word_right(&self) -> Field {
        Field::new(self.text.clone(), self.word_end())
    }

    pub fn cut_word_left(&self) -> Field {
        let from = self.word_start();
        let mut text = self.text.clone();
        text.replace_range(from..self.cursor, "");
        Field::new(text, from)
    }
}
