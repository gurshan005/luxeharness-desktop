use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub text: String,
    pub attachments: Vec<PathBuf>,
    #[serde(default = "user_role")]
    pub role: String,
    #[serde(default)]
    pub run_id: Option<String>,
}

fn user_role() -> String {
    "user".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    #[serde(default)]
    pub workspace: Option<PathBuf>,
    pub messages: Vec<Entry>,
    pub attachments: Vec<PathBuf>,
    pub draft: String,
    #[serde(default)]
    pub run_id: Option<String>,
}

impl Session {
    pub fn new() -> Self {
        Self {
            workspace: None,
            messages: Vec::new(),
            attachments: Vec::new(),
            draft: String::new(),
            run_id: None,
        }
    }

    pub fn title(&self) -> String {
        self.messages
            .first()
            .and_then(|entry| {
                entry
                    .text
                    .lines()
                    .map(str::trim)
                    .find(|line| !line.is_empty())
            })
            .or_else(|| {
                self.draft
                    .lines()
                    .map(str::trim)
                    .find(|line| !line.is_empty())
            })
            .unwrap_or("New chat")
            .to_owned()
    }

    pub fn submit(&mut self, prompt: &str) -> bool {
        let prompt = prompt.trim();
        if prompt.is_empty() {
            return false;
        }

        self.messages.push(Entry {
            text: prompt.to_owned(),
            attachments: std::mem::take(&mut self.attachments),
            role: "user".into(),
            run_id: None,
        });
        self.draft.clear();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whitespace_does_not_create_a_message_or_consume_attachments() {
        let mut session = Session::new();
        let file = PathBuf::from("design-notes.txt");
        session.attachments.push(file.clone());
        session.draft = "An unsent draft".into();

        assert!(!session.submit("  \n \t"));
        assert!(session.messages.is_empty());
        assert_eq!(session.attachments, vec![file]);
        assert_eq!(session.draft, "An unsent draft");
    }

    #[test]
    fn submission_preserves_unicode_and_trims_only_outer_whitespace() {
        let mut session = Session::new();
        let prompt = "你好 🌿\nKeep this spacing:  café";

        assert!(session.submit(&format!("  {prompt}\n")));
        assert_eq!(session.messages.len(), 1);
        assert_eq!(session.messages[0].text, prompt);
    }

    #[test]
    fn attachments_transfer_to_the_submitted_message_once() {
        let mut session = Session::new();
        let file = PathBuf::from("design-notes.txt");
        session.attachments.push(file.clone());

        assert!(session.submit("Consider these notes"));
        assert_eq!(session.messages.len(), 1);
        assert_eq!(session.messages[0].attachments, vec![file.clone()]);
        assert!(session.attachments.is_empty());

        assert!(session.submit("A follow-up"));
        assert_eq!(session.messages.len(), 2);
        assert!(session.messages[1].attachments.is_empty());
        assert_eq!(session.messages[0].attachments, vec![file]);
    }

    #[test]
    fn draft_supplies_the_first_nonempty_line_as_title() {
        let mut session = Session::new();
        assert_eq!(session.title(), "New chat");

        session.draft =
            "  \n  Refine the composer without changing its behavior  \nMore detail".into();
        assert_eq!(
            session.title(),
            "Refine the composer without changing its behavior"
        );

        session.draft = " \n\t".into();
        assert_eq!(session.title(), "New chat");
    }

    #[test]
    fn first_submitted_message_keeps_the_title_after_followups() {
        let mut session = Session::new();
        session.draft = "  \n  Refine the composer  \nMore detail".into();
        let prompt = session.draft.clone();

        assert!(session.submit(&prompt));
        assert!(session.draft.is_empty());
        assert_eq!(session.title(), "Refine the composer");

        session.draft = "A different draft".into();
        assert_eq!(session.title(), "Refine the composer");
        assert!(session.submit("A follow-up"));
        assert!(session.draft.is_empty());
        assert_eq!(session.title(), "Refine the composer");
    }
}
