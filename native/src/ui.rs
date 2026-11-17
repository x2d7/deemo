//! Однострочное редактируемое текстовое поле (курсор, backspace, вставка).
//!
//! Используется экраном настроек: URL сервера, никнейм, пароль. Принимает
//! любые печатные символы (пробел, `:`, `/`, `?`, `&` и т.п.); служебные
//! клавиши обрабатываются в вызывающем коде — пока поле в фокусе, глобальные
//! игровые хоткеи не должны срабатывать.

#[derive(Clone, Default)]
pub struct TextInput {
    pub text: String,
    /// позиция каретки в СИМВОЛАХ (не в байтах), 0..=кол-во символов
    pub caret: usize,
    /// максимальная длина в символах
    pub max_chars: usize,
}

impl TextInput {
    pub fn new(text: impl Into<String>, max_chars: usize) -> TextInput {
        let text = text.into();
        let mut t = TextInput {
            text,
            caret: 0,
            max_chars,
        };
        t.caret = t.len_chars();
        t
    }

    pub fn len_chars(&self) -> usize {
        self.text.chars().count()
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.caret = 0;
    }

    /// Вставить строку (может быть несколько символов — так приходят
    /// некоторые раскладки/IME и вставка). Управляющие символы отбрасываются.
    pub fn insert(&mut self, s: &str) {
        let room = self.max_chars.saturating_sub(self.len_chars());
        if room == 0 {
            return;
        }
        let take: String = s
            .chars()
            .filter(|c| !c.is_control())
            .take(room)
            .collect();
        if take.is_empty() {
            return;
        }
        let at = self.byte_of(self.caret);
        self.text.insert_str(at, &take);
        self.caret += take.chars().count();
    }

    pub fn backspace(&mut self) -> bool {
        if self.caret == 0 {
            return false;
        }
        let prev = self.caret - 1;
        let a = self.byte_of(prev);
        let b = self.byte_of(self.caret);
        self.text.replace_range(a..b, "");
        self.caret = prev;
        true
    }

    pub fn delete(&mut self) -> bool {
        if self.caret >= self.len_chars() {
            return false;
        }
        let a = self.byte_of(self.caret);
        let b = self.byte_of(self.caret + 1);
        self.text.replace_range(a..b, "");
        true
    }

    pub fn left(&mut self) {
        self.caret = self.caret.saturating_sub(1);
    }

    pub fn right(&mut self) {
        self.caret = (self.caret + 1).min(self.len_chars());
    }

    pub fn home(&mut self) {
        self.caret = 0;
    }

    pub fn end(&mut self) {
        self.caret = self.len_chars();
    }

    /// Байтовый offset символа с индексом `idx`.
    fn byte_of(&self, idx: usize) -> usize {
        self.text
            .char_indices()
            .nth(idx)
            .map(|(b, _)| b)
            .unwrap_or(self.text.len())
    }
}

#[cfg(test)]
mod tests {
    use super::TextInput;

    #[test]
    fn typing_ascii_and_symbols() {
        let mut t = TextInput::new("", 32);
        for c in "http://1.2.3.4:8090/a b?x=1&y=2".chars() {
            t.insert(&c.to_string());
        }
        assert_eq!(t.text, "http://1.2.3.4:8090/a b?x=1&y=2");
        assert_eq!(t.caret, 31);
    }

    #[test]
    fn caret_editing_unicode() {
        let mut t = TextInput::new("éé", 10);
        t.home();
        t.insert("X");
        assert_eq!(t.text, "Xéé");
        assert_eq!(t.caret, 1);
        t.right();
        assert!(t.backspace());
        assert_eq!(t.text, "Xé");
        t.end();
        assert!(!t.delete());
    }
}
