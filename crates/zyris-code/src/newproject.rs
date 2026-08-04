//! 새 프로젝트 양식. ← 목록에서 "＋ 새 프로젝트" 줄을 고르면 열린다.
//!
//! **이름과 설명을 두 칸에 나눠 받는다.** 예전에는 `/project `를 입력란에 넣어 주고
//! 이름을 이어 치게 했는데 설명을 받을 자리가 없었다. 양식이 그 자리를 대신한다 —
//! Enter로 만들고 Esc로 닫으면 목록(아래에 그대로 열려 있다)으로 돌아온다.
//!
//! 여기는 순수하다. 서버를 부르는 것은 I/O 자리가 한다(`app.rs`의 `project_out`).

use crate::input::Input;

/// 양식의 칸.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Field {
    #[default]
    Name,
    Description,
}

/// 새 프로젝트 양식 상태.
#[derive(Debug, Clone, Default)]
pub struct Form {
    pub name: Input,
    pub description: Input,
    pub field: Field,
    /// 만들기를 눌렀다가 서버가 거절한 사유. **칸을 옮기면 지운다.**
    pub error: Option<String>,
}

impl Form {
    pub fn new() -> Self {
        Self::default()
    }

    /// 지금 글자가 들어갈 칸.
    pub fn active(&mut self) -> &mut Input {
        match self.field {
            Field::Name => &mut self.name,
            Field::Description => &mut self.description,
        }
    }

    /// 다음 칸으로. 끝이면 처음으로 돌아온다.
    pub fn next(&mut self) {
        self.field = match self.field {
            Field::Name => Field::Description,
            Field::Description => Field::Name,
        };
        self.error = None;
    }

    /// 앞 칸으로.
    pub fn prev(&mut self) {
        self.field = match self.field {
            Field::Name => Field::Description,
            Field::Description => Field::Name,
        };
        self.error = None;
    }

    /// 만들기. **이름이 비면 서버를 부르지 않는다** — 무엇을 만들지 모르고, 목록에
    /// 이름 없는 줄이 하나 생기면 지우는 길이 이 앱에 없다. 비었으면 사유를 담고
    /// `None`을 준다. 설명은 비워도 된다.
    pub fn submit(&mut self, lang: crate::lang::Lang) -> Option<(String, String)> {
        let name = self.name.text.trim().to_string();
        if name.is_empty() {
            self.error = Some(lang.project_name_required().to_string());
            return None;
        }
        self.error = None;
        Some((name, self.description.text.trim().to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form() -> Form {
        Form::new()
    }

    #[test]
    fn a_fresh_form_starts_on_the_name_field() {
        let f = form();
        assert_eq!(f.field, Field::Name);
    }

    #[test]
    fn switching_fields_wraps_around() {
        let mut f = form();
        f.next();
        assert_eq!(f.field, Field::Description);
        f.next();
        assert_eq!(f.field, Field::Name);
        f.prev();
        assert_eq!(f.field, Field::Description);
    }

    /// **이름이 비면 만들지 않는다.** 목록에 이름 없는 줄이 생기면 지우는 길이 없다.
    #[test]
    fn an_empty_name_is_refused_on_the_spot() {
        let mut f = form();
        assert!(f.submit(crate::lang::Lang::Ko).is_none());
        assert!(f.error.is_some(), "왜 안 되는지 말해 줘야 한다");
    }

    #[test]
    fn submitting_gives_the_trimmed_pair() {
        let mut f = form();
        for c in "새 프로젝트".chars() {
            f.name.insert(c);
        }
        for c in "설명".chars() {
            f.description.insert(c);
        }
        assert_eq!(f.submit(crate::lang::Lang::Ko), Some(("새 프로젝트".into(), "설명".into())));
        assert!(f.error.is_none());
    }

    /// 설명은 비워도 된다 — 이름만 있으면 만든다.
    #[test]
    fn an_empty_description_is_fine() {
        let mut f = form();
        f.name.insert_str("이름만");
        assert_eq!(f.submit(crate::lang::Lang::Ko), Some(("이름만".into(), String::new())));
    }

    /// 오류는 칸을 옮기면 지운다 — 사유가 남은 채 다시 만들면 헷갈린다.
    #[test]
    fn switching_fields_clears_the_error() {
        let mut f = form();
        assert!(f.submit(crate::lang::Lang::Ko).is_none());
        assert!(f.error.is_some());
        f.next();
        assert!(f.error.is_none());
    }
}
