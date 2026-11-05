//! Language-related value types.

use std::error::Error;
use std::fmt;
use std::str::FromStr;

/// A normalised (lower-case) BCP-47-style language code such as `tr`, `en` or `pt-br`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LanguageCode(String);

/// The given text is not a valid language code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidLanguageCode(pub String);

impl fmt::Display for InvalidLanguageCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid language code: {:?}", self.0)
    }
}

impl Error for InvalidLanguageCode {}

impl LanguageCode {
    /// Validates and normalises a language code.
    ///
    /// Accepts a 2-3 letter primary subtag optionally followed by `-` separated alphanumeric
    /// subtags of 2-8 characters. The result is lower-cased.
    pub fn new(code: &str) -> Result<Self, InvalidLanguageCode> {
        let mut parts = code.split('-');
        let primary_ok = parts.next().is_some_and(|p| {
            (2..=3).contains(&p.len()) && p.chars().all(|c| c.is_ascii_alphabetic())
        });
        let rest_ok = parts
            .all(|p| (2..=8).contains(&p.len()) && p.chars().all(|c| c.is_ascii_alphanumeric()));
        if primary_ok && rest_ok {
            Ok(Self(code.to_ascii_lowercase()))
        } else {
            Err(InvalidLanguageCode(code.to_owned()))
        }
    }

    /// Turkish (`tr`), the product's default language.
    #[must_use]
    pub fn turkish() -> Self {
        Self("tr".to_owned())
    }

    /// English (`en`).
    #[must_use]
    pub fn english() -> Self {
        Self("en".to_owned())
    }

    /// The code as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for LanguageCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for LanguageCode {
    type Err = InvalidLanguageCode;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

/// Spoken-language setting for transcription: detect it, or force one language.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum LanguageSetting {
    /// Let the transcriber detect the language.
    Auto,
    /// Force the given language.
    Fixed(LanguageCode),
}

impl LanguageSetting {
    /// Textual form used for persistence: `auto` or the language code.
    #[must_use]
    pub fn to_storage_string(&self) -> String {
        match self {
            Self::Auto => "auto".to_owned(),
            Self::Fixed(code) => code.as_str().to_owned(),
        }
    }
}

impl fmt::Display for LanguageSetting {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Auto => f.write_str("auto"),
            Self::Fixed(code) => fmt::Display::fmt(code, f),
        }
    }
}

impl FromStr for LanguageSetting {
    type Err = InvalidLanguageCode;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s == "auto" {
            Ok(Self::Auto)
        } else {
            LanguageCode::new(s).map(Self::Fixed)
        }
    }
}

string_enum! {
    /// Languages the application UI is translated into.
    pub enum UiLanguage {
        /// Turkish (default).
        Tr => "tr",
        /// English.
        En => "en",
    }
}

#[cfg(test)]
mod tests {
    use super::{LanguageCode, LanguageSetting, UiLanguage};

    #[test]
    fn language_code_is_validated_and_lowercased() {
        assert_eq!(LanguageCode::new("TR").unwrap().as_str(), "tr");
        assert_eq!(LanguageCode::new("pt-BR").unwrap().as_str(), "pt-br");
        assert!(LanguageCode::new("").is_err());
        assert!(LanguageCode::new("t").is_err());
        assert!(LanguageCode::new("turk").is_err());
        assert!(LanguageCode::new("tr-").is_err());
        assert!(LanguageCode::new("t r").is_err());
    }

    #[test]
    fn language_setting_round_trips_through_text() {
        assert_eq!(
            "auto".parse::<LanguageSetting>().unwrap(),
            LanguageSetting::Auto
        );
        let fixed: LanguageSetting = "tr".parse().unwrap();
        assert_eq!(fixed, LanguageSetting::Fixed(LanguageCode::turkish()));
        assert_eq!(fixed.to_storage_string(), "tr");
        assert_eq!(LanguageSetting::Auto.to_storage_string(), "auto");
        assert!("not a code".parse::<LanguageSetting>().is_err());
    }

    #[test]
    fn ui_language_parses() {
        assert_eq!("tr".parse::<UiLanguage>().unwrap(), UiLanguage::Tr);
        assert!("de".parse::<UiLanguage>().is_err());
    }
}
