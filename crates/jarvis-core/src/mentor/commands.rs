//! Spoken commands that steer mentor mode without reaching the model.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// A whole phrase that is a command for JARVIS, not a message to the mentor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "core.ts")]
pub enum MentorCommand {
    /// Stop listening; the session stays.
    Pause,
    /// Listen again after a pause.
    Resume,
    /// End the lesson.
    End,
    /// Look at the screen with the next turn.
    Look,
    /// Approve the change waiting for approval.
    Yes,
    /// Decline the change waiting for approval.
    No,
    /// Stop speaking or typing.
    Stop,
}

const PHRASES: &[(&str, MentorCommand)] = &[
    ("пауза", MentorCommand::Pause),
    ("pause", MentorCommand::Pause),
    ("продовжуємо", MentorCommand::Resume),
    ("продовжуй", MentorCommand::Resume),
    ("continue", MentorCommand::Resume),
    ("закінчуємо урок", MentorCommand::End),
    ("кінець уроку", MentorCommand::End),
    ("end the lesson", MentorCommand::End),
    ("подивись на екран", MentorCommand::Look),
    ("що на екрані", MentorCommand::Look),
    ("look at the screen", MentorCommand::Look),
    ("так", MentorCommand::Yes),
    ("так пиши", MentorCommand::Yes),
    ("пиши", MentorCommand::Yes),
    ("давай", MentorCommand::Yes),
    ("yes", MentorCommand::Yes),
    ("write it", MentorCommand::Yes),
    ("ні", MentorCommand::No),
    ("не треба", MentorCommand::No),
    ("no", MentorCommand::No),
    ("стоп", MentorCommand::Stop),
    ("stop", MentorCommand::Stop),
];

/// The command a recognized phrase is, when the whole phrase is one
/// (case, punctuation and spacing do not matter).
pub fn parse_command(text: &str) -> Option<MentorCommand> {
    let normalized: String = text
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '\'' {
                c.to_lowercase().next().unwrap_or(c)
            } else {
                ' '
            }
        })
        .collect();
    let normalized = normalized.split_whitespace().collect::<Vec<_>>().join(" ");
    PHRASES
        .iter()
        .find(|(phrase, _)| *phrase == normalized)
        .map(|&(_, command)| command)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_match_whole_phrases() {
        let cases = [
            ("Пауза.", Some(MentorCommand::Pause)),
            ("пауза в коді", None),
            ("Pause", Some(MentorCommand::Pause)),
            ("Продовжуємо!", Some(MentorCommand::Resume)),
            ("continue", Some(MentorCommand::Resume)),
            ("Закінчуємо урок.", Some(MentorCommand::End)),
            ("кінець уроку", Some(MentorCommand::End)),
            ("End the lesson", Some(MentorCommand::End)),
            ("Подивись на екран", Some(MentorCommand::Look)),
            ("що на екрані?", Some(MentorCommand::Look)),
            ("Look at the screen!", Some(MentorCommand::Look)),
            ("так", Some(MentorCommand::Yes)),
            ("Так, пиши.", Some(MentorCommand::Yes)),
            ("пиши", Some(MentorCommand::Yes)),
            ("давай", Some(MentorCommand::Yes)),
            ("yes", Some(MentorCommand::Yes)),
            ("write it", Some(MentorCommand::Yes)),
            ("ні", Some(MentorCommand::No)),
            ("Не треба.", Some(MentorCommand::No)),
            ("No", Some(MentorCommand::No)),
            ("Стоп!", Some(MentorCommand::Stop)),
            ("stop", Some(MentorCommand::Stop)),
            ("так, але спершу поясни навіщо", None),
            ("", None),
        ];
        for (text, expected) in cases {
            assert_eq!(parse_command(text), expected, "{text:?}");
        }
    }
}
