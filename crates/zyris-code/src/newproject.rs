//! The new-project form. ← Opens when the "+ new project" row is picked in the list.
//!
//! **Name and description are collected in two fields.** It used to drop `/project ` into the
//! input and let you keep typing the name, but there was no room for a description. The form
//! takes that place — Enter creates, Esc closes and returns to the list (still open below).
//!
//! This stays pure. Calling the server is the I/O site's job (`project_out` in `app.rs`).

use crate::input::Input;

/// A field of the form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Field {
    #[default]
    Name,
    Description,
}

/// State of the new-project form.
#[derive(Debug, Clone, Default)]
pub struct Form {
    pub name: Input,
    pub description: Input,
    pub field: Field,
    /// Why the server rejected a create attempt. **Cleared when the field changes.**
    pub error: Option<String>,
}

impl Form {
    pub fn new() -> Self {
        Self::default()
    }

    /// The field the current keystrokes go into.
    pub fn active(&mut self) -> &mut Input {
        match self.field {
            Field::Name => &mut self.name,
            Field::Description => &mut self.description,
        }
    }

    /// Move to the next field. Wraps to the start at the end.
    pub fn next(&mut self) {
        self.field = match self.field {
            Field::Name => Field::Description,
            Field::Description => Field::Name,
        };
        self.error = None;
    }

    /// Move to the previous field. With two fields that is the other one, the same as `next`.
    pub fn prev(&mut self) {
        self.next();
    }

    /// Submit. **An empty name never calls the server** — we would not know what to create, and
    /// once an unnamed row appears in the list there is no way to remove it in this app. If
    /// empty, carries the reason and returns `None`. The description may be empty.
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

    /// **An empty name is not created.** An unnamed row in the list could never be removed.
    #[test]
    fn an_empty_name_is_refused_on_the_spot() {
        let mut f = form();
        assert!(f.submit(crate::lang::Lang::Ko).is_none());
        assert!(f.error.is_some(), "it must say why it cannot");
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

    /// The description may be empty — a name alone is enough to create.
    #[test]
    fn an_empty_description_is_fine() {
        let mut f = form();
        f.name.insert_str("이름만");
        assert_eq!(f.submit(crate::lang::Lang::Ko), Some(("이름만".into(), String::new())));
    }

    /// The error is cleared when the field changes — creating again with a stale reason left up would confuse.
    #[test]
    fn switching_fields_clears_the_error() {
        let mut f = form();
        assert!(f.submit(crate::lang::Lang::Ko).is_none());
        assert!(f.error.is_some());
        f.next();
        assert!(f.error.is_none());
    }
}
