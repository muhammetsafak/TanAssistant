//! AI provider profiles and the task-to-profile assignment (ADR-008).
//!
//! Without a `Transcription` assignment the application falls back to local whisper (ADR-003).

use crate::ids::{ProviderProfileId, SecretId};

string_enum! {
    /// Supported AI providers.
    pub enum ProviderKind {
        /// `OpenAI` API.
        OpenAi => "openai",
        /// Google Gemini API.
        Gemini => "gemini",
        /// Anthropic API.
        Anthropic => "anthropic",
        /// `DeepSeek` API.
        DeepSeek => "deepseek",
        /// Local Ollama server.
        Ollama => "ollama",
        /// The Claude Code CLI (`claude`) installed and signed in on this machine (ADR-013).
        ClaudeCli => "claude_cli",
        /// The `OpenAI` Codex CLI (`codex`) installed and signed in on this machine (ADR-013).
        CodexCli => "codex_cli",
    }
}

string_enum! {
    /// The jobs a provider profile can be assigned to.
    pub enum TaskKind {
        /// Speech-to-text.
        Transcription => "transcription",
        /// Screen-frame understanding.
        Vision => "vision",
        /// Note/summary generation.
        Summary => "summary",
    }
}

/// A configured way to reach a provider and model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderProfile {
    /// Identity.
    pub id: ProviderProfileId,
    /// Which provider.
    pub kind: ProviderKind,
    /// Name shown in the UI.
    pub display_name: String,
    /// Model identifier.
    pub model: String,
    /// Custom endpoint (e.g. a remote Ollama host); `None` uses the provider default.
    pub base_url: Option<String>,
    /// API-key secret, when the provider needs one.
    pub secret_ref: Option<SecretId>,
    /// Whether the model understands images. Only Ollama's answer comes from here (it depends
    /// on the model that is served, default `false`); for every other provider the capability
    /// matrix decides and the flag is kept in line with it.
    pub supports_vision: bool,
}

/// Which profile performs a task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskAssignment {
    /// The task.
    pub task: TaskKind,
    /// The profile that performs it.
    pub profile_id: ProviderProfileId,
}

#[cfg(test)]
mod tests {
    use super::{ProviderKind, TaskKind};

    #[test]
    fn enum_text_round_trips() {
        for &kind in ProviderKind::ALL {
            assert_eq!(kind.as_str().parse::<ProviderKind>().unwrap(), kind);
        }
        for &task in TaskKind::ALL {
            assert_eq!(task.as_str().parse::<TaskKind>().unwrap(), task);
        }
    }
}
