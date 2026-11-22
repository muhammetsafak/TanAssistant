//! The person directory (ADR-014): who the people in the user's meetings are, who took part in
//! which session, and who owns an action item.
//!
//! Names are compared through one rule, [`normalize_name`]: case folding with the Turkish
//! rules (`İ`/`i` and `I`/`ı` are two pairs), trimmed and with runs of whitespace collapsed.
//! Accents are kept, so "Ayşe" and "Ayse" are two people; [`search_key`] is the looser key a
//! search uses to find both.
//!
//! Every error type here is safe to show to the user.

use std::sync::Arc;

use tan_domain::{
    ActionItemId, EpochMs, ParticipantSource, Person, PersonId, SessionId, SessionParticipant,
};

use crate::error::RepositoryResult;
use crate::ports::{Clock, IdGenerator, NoteRepository, PersonRepository, SessionRepository};
use crate::sessions::{SessionsError, SessionsResult};

/// Longest accepted person name, in characters.
pub const MAX_NAME_CHARS: usize = 100;

/// What the AI writes as the owner (or participant) when it means the person who made the
/// recording. The prompts ask for exactly this word.
pub const SELF_MARKER: &str = "me";

/// Lower-cases `text` with the Turkish rules. Rust's own `to_lowercase` maps `I` to `i` and
/// `İ` to `i` plus a combining dot, which would make "ILKER" and "İlker" different people.
fn fold_case(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            // A decomposed capital dotted I: `I` followed by U+0307.
            'I' if chars.peek() == Some(&'\u{307}') => {
                chars.next();
                out.push('i');
            }
            'I' => out.push('ı'),
            'İ' => out.push('i'),
            // A lower-case `i` with a stray combining dot is still an `i`.
            'i' => {
                if chars.peek() == Some(&'\u{307}') {
                    chars.next();
                }
                out.push('i');
            }
            other => out.extend(other.to_lowercase()),
        }
    }
    out
}

/// Trims `text` and collapses every run of whitespace into one space.
fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The key two spellings of one name share: Turkish case folding, trimmed, single spaces.
/// Accents are kept. This is the single source of truth for "is this the same person".
#[must_use]
pub fn normalize_name(name: &str) -> String {
    fold_case(&collapse_whitespace(name))
}

/// The key two spellings of one task share. It is the same rule as [`normalize_name`]; a
/// regenerated note carries the user's edits over to the action item whose key is unchanged.
#[must_use]
pub fn normalize_task(task: &str) -> String {
    normalize_name(task)
}

/// The key a search compares: [`normalize_name`] with the Turkish letters and the circumflex
/// vowels reduced to their plain Latin letter, so "Ayse" finds "Ayşe" and the reverse.
#[must_use]
pub fn search_key(text: &str) -> String {
    normalize_name(text)
        .chars()
        .map(|c| match c {
            'ç' => 'c',
            'ğ' => 'g',
            'ı' | 'î' => 'i',
            'ö' => 'o',
            'ş' => 's',
            'ü' | 'û' => 'u',
            'â' => 'a',
            'ê' => 'e',
            other => other,
        })
        .collect()
}

/// Whether `query` finds `name` in a search (an empty query finds everyone).
#[must_use]
pub fn matches_query(name: &str, query: &str) -> bool {
    search_key(name).contains(&search_key(query))
}

/// Whether `name` is the word the AI uses for the person who made the recording.
#[must_use]
pub fn is_self_marker(name: &str) -> bool {
    normalize_name(name) == SELF_MARKER
}

/// A name ready to be shown and stored: trimmed, whitespace collapsed. `None` when it is empty,
/// longer than [`MAX_NAME_CHARS`] or contains a control character.
#[must_use]
pub fn clean_display_name(name: &str) -> Option<String> {
    let cleaned = collapse_whitespace(name);
    let acceptable = !cleaned.is_empty()
        && cleaned.chars().count() <= MAX_NAME_CHARS
        && !cleaned.chars().any(char::is_control);
    acceptable.then_some(cleaned)
}

/// Finds the person a name stands for, creating them when the directory has no such person.
/// The word [`SELF_MARKER`] stands for the "me" person. `None` when the name is unusable
/// (see [`clean_display_name`]).
pub(crate) async fn resolve_person<D: PersonRepository>(
    people: &D,
    ids: &dyn IdGenerator,
    now: EpochMs,
    name: &str,
) -> RepositoryResult<Option<Person>> {
    if is_self_marker(name) {
        return people.self_person().await.map(Some);
    }
    let Some(display_name) = clean_display_name(name) else {
        return Ok(None);
    };
    let candidate = Person {
        id: PersonId::new(ids.new_id()),
        display_name,
        is_self: false,
        created_at: now,
    };
    people
        .find_or_create(&normalize_name(name), &candidate)
        .await
        .map(Some)
}

/// A person the user points at: one already in the directory, or a name to look up or add.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PersonRef {
    /// A person of the directory.
    Existing(PersonId),
    /// A name; it finds the person already known by it or adds a new one.
    NewName(String),
}

/// Lists everyone in the directory: the "me" person first, then by name.
pub struct ListPeople<D> {
    people: Arc<D>,
}

impl<D: PersonRepository> ListPeople<D> {
    /// Creates the use case.
    #[must_use]
    pub fn new(people: Arc<D>) -> Self {
        Self { people }
    }

    /// Runs it.
    pub async fn execute(&self) -> SessionsResult<Vec<Person>> {
        Ok(self.people.list().await?)
    }
}

/// The directory entry a [`PersonRef`] stands for.
async fn resolve_ref<D: PersonRepository>(
    people: &D,
    ids: &dyn IdGenerator,
    clock: &dyn Clock,
    person: PersonRef,
) -> SessionsResult<Person> {
    match person {
        PersonRef::Existing(id) => people
            .get(&id)
            .await?
            .ok_or(SessionsError::NotFound("person")),
        PersonRef::NewName(name) => resolve_person(people, ids, clock.now(), &name)
            .await?
            .ok_or_else(|| {
                SessionsError::Invalid(format!(
                    "a person's name must have 1 to {MAX_NAME_CHARS} characters and no control characters"
                ))
            }),
    }
}

/// Sets, replaces or clears the owner of an action item. The choice is the user's, so it
/// survives regenerating the note.
pub struct SetActionItemOwner<N, D> {
    notes: Arc<N>,
    people: Arc<D>,
    ids: Arc<dyn IdGenerator>,
    clock: Arc<dyn Clock>,
}

impl<N: NoteRepository, D: PersonRepository> SetActionItemOwner<N, D> {
    /// Creates the use case.
    #[must_use]
    pub fn new(
        notes: Arc<N>,
        people: Arc<D>,
        ids: Arc<dyn IdGenerator>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            notes,
            people,
            ids,
            clock,
        }
    }

    /// Makes `owner` the owner of the item (`None` clears it) and returns that person. The
    /// person does not become a participant of the session: they stay in the directory.
    pub async fn execute(
        &self,
        id: &ActionItemId,
        owner: Option<PersonRef>,
    ) -> SessionsResult<Option<Person>> {
        // Check the item first: a name must not enter the directory for an item that is gone
        // (regenerating a note gives its action items new ids).
        self.notes
            .get_action_item(id)
            .await?
            .ok_or(SessionsError::NotFound("action item"))?;
        let person = match owner {
            Some(owner) => Some(resolve_ref(&*self.people, &*self.ids, &*self.clock, owner).await?),
            None => None,
        };
        self.notes
            .set_action_item_owner(id, person.as_ref().map(|p| &p.id))
            .await?;
        Ok(person)
    }
}

/// Adds a person to the participants of a session, as the user's own choice.
pub struct AddSessionParticipant<S, D> {
    sessions: Arc<S>,
    people: Arc<D>,
    ids: Arc<dyn IdGenerator>,
    clock: Arc<dyn Clock>,
}

impl<S: SessionRepository, D: PersonRepository> AddSessionParticipant<S, D> {
    /// Creates the use case.
    #[must_use]
    pub fn new(
        sessions: Arc<S>,
        people: Arc<D>,
        ids: Arc<dyn IdGenerator>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            sessions,
            people,
            ids,
            clock,
        }
    }

    /// Adds the person and returns them. A person who already took part is not duplicated, and
    /// an AI-extracted participant becomes a manual one (reprocessing keeps it).
    pub async fn execute(
        &self,
        session_id: &SessionId,
        person: PersonRef,
    ) -> SessionsResult<Person> {
        self.sessions
            .get(session_id)
            .await?
            .ok_or(SessionsError::NotFound("session"))?;
        let person = resolve_ref(&*self.people, &*self.ids, &*self.clock, person).await?;
        self.people
            .add_participant(&SessionParticipant {
                session_id: session_id.clone(),
                person_id: person.id.clone(),
                source: ParticipantSource::Manual,
            })
            .await?;
        Ok(person)
    }
}

/// Removes a person from the participants of a session. The person stays in the directory and
/// keeps owning the action items they own.
pub struct RemoveSessionParticipant<D> {
    people: Arc<D>,
}

impl<D: PersonRepository> RemoveSessionParticipant<D> {
    /// Creates the use case.
    #[must_use]
    pub fn new(people: Arc<D>) -> Self {
        Self { people }
    }

    /// Runs it; a person who did not take part is [`SessionsError::NotFound`].
    pub async fn execute(
        &self,
        session_id: &SessionId,
        person_id: &PersonId,
    ) -> SessionsResult<()> {
        Ok(self
            .people
            .remove_participant(session_id, person_id)
            .await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spelling_variants_of_one_name_share_a_key() {
        assert_eq!(normalize_name("ayşe "), normalize_name("AYŞE"));
        assert_eq!(normalize_name("İlker"), normalize_name("ilker"));
        assert_eq!(normalize_name("İLKER"), normalize_name("ilker"));
        assert_eq!(normalize_name("  Ali   Veli\t"), "ali veli");
        assert_eq!(normalize_name("ÖZGE"), normalize_name("Özge"));
    }

    #[test]
    fn the_turkish_dotted_and_dotless_i_are_kept_apart() {
        assert_eq!(normalize_name("I"), "ı");
        assert_eq!(normalize_name("İ"), "i");
        assert_ne!(normalize_name("IŞIK"), normalize_name("işik"));
        assert_eq!(normalize_name("IŞIK"), "ışık");
        // A decomposed capital dotted I folds like the precomposed one.
        assert_eq!(normalize_name("I\u{307}lker"), "ilker");
    }

    #[test]
    fn accents_are_kept_by_the_key_and_ignored_by_search() {
        assert_ne!(normalize_name("Ayşe"), normalize_name("Ayse"));
        assert_eq!(search_key("Ayşe"), search_key("AYSE"));
        assert_eq!(search_key("Çağrı"), "cagri");
        assert!(matches_query("Ayşe Yılmaz", "ayse"));
        assert!(matches_query("Ayse Yilmaz", "AYŞE yıl"));
        assert!(matches_query("Ayşe", ""));
        assert!(!matches_query("Ayşe", "mehmet"));
    }

    #[test]
    fn tasks_are_keyed_like_names() {
        assert_eq!(normalize_task("  Raporu   HAZIRLA "), "raporu hazırla");
        assert_eq!(normalize_task("Raporu hazırla"), "raporu hazırla");
        assert_ne!(
            normalize_task("Raporu hazirla"),
            normalize_task("Raporu hazırla")
        );
    }

    #[test]
    fn the_self_marker_is_recognised_whatever_its_case() {
        assert!(is_self_marker("me"));
        assert!(is_self_marker(" Me "));
        assert!(!is_self_marker("Mehmet"));
        assert!(!is_self_marker(""));
    }

    #[test]
    fn display_names_are_cleaned_or_refused() {
        assert_eq!(
            clean_display_name("  Ayşe \n Yılmaz ").unwrap(),
            "Ayşe Yılmaz"
        );
        assert_eq!(clean_display_name("   "), None);
        assert_eq!(clean_display_name("a\u{0}b"), None);
        assert_eq!(
            clean_display_name(&"x".repeat(MAX_NAME_CHARS)).map(|n| n.len()),
            Some(MAX_NAME_CHARS)
        );
        assert_eq!(clean_display_name(&"x".repeat(MAX_NAME_CHARS + 1)), None);
    }
}
