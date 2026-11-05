// `assert!(x.is_empty())` / `assert!(a == b)` read fine in tests; pedantic clippy would rewrite them.
#![allow(clippy::assert_is_empty, clippy::manual_assert_eq)]

mod common;

use common::{block_on, segment, session, test_db, TestDb};
use tan_application::ports::{
    NoteRepository, SearchIndex, SearchSource, SessionRepository, TranscriptRepository,
};
use tan_domain::{EpochMs, LanguageCode, Note, NoteId, SessionId};
use tan_infra_storage::{
    SqliteNoteRepository, SqliteSearchIndex, SqliteSessionRepository, SqliteTranscriptRepository,
};

/// Two Turkish sessions: one with transcript text, one with only a note.
fn seeded() -> TestDb {
    let t = test_db();
    let sessions = SqliteSessionRepository::new(t.db.clone());
    block_on(sessions.create(&session("s1", 1))).unwrap();
    block_on(sessions.create(&session("s2", 2))).unwrap();

    let transcripts = SqliteTranscriptRepository::new(t.db.clone());
    block_on(transcripts.replace_for_session(
        &SessionId::new("s1"),
        &[
            segment("s1", 0, "Bugün toplantıda iş planını konuştuk."),
            segment("s1", 5_000, "Müşteriyle yarın görüşme yapacağız."),
            segment("s1", 9_000, "Çağrı ve ığdır şehri öğle yemeği."),
        ],
    ))
    .unwrap();

    let notes = SqliteNoteRepository::new(t.db.clone());
    let note = Note {
        id: NoteId::new("n1"),
        session_id: SessionId::new("s2"),
        language: LanguageCode::turkish(),
        summary: "İŞ PLANI onaylandı.".to_owned(),
        decisions: vec!["Bütçe artırılacak".to_owned()],
        open_questions: vec!["Lansman tarihi nedir?".to_owned()],
        frame_refs: vec![],
        created_at: EpochMs::new(10),
    };
    block_on(notes.save(&note, &[])).unwrap();
    t
}

fn search(t: &TestDb, query: &str) -> Vec<tan_application::ports::SearchHit> {
    block_on(SqliteSearchIndex::new(t.db.clone()).search(query, 20)).unwrap()
}

#[test]
fn turkish_words_match_in_transcripts() {
    let t = seeded();

    let hits = search(&t, "toplantı");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].session_id, SessionId::new("s1"));
    assert_eq!(hits[0].source, SearchSource::Transcript);
    assert_eq!(hits[0].start_ms, Some(0));
    assert!(hits[0].excerpt.contains("toplantıda"));

    let hits = search(&t, "görüşme");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].start_ms, Some(5_000));
}

#[test]
fn query_case_and_dotted_i_do_not_matter() {
    let t = seeded();

    // "İŞ PLANI" (upper case, dotted İ) and "iş planı" find the same things: the transcript
    // segment "iş planını" in s1 and the note "İŞ PLANI onaylandı." in s2.
    let upper = search(&t, "İŞ PLANI");
    let lower = search(&t, "iş planı");
    assert_eq!(upper.len(), 2, "{upper:?}");
    assert_eq!(upper, lower);

    let sources: Vec<_> = upper
        .iter()
        .map(|h| (h.session_id.as_str().to_owned(), h.source))
        .collect();
    assert!(sources.contains(&("s1".to_owned(), SearchSource::Transcript)));
    assert!(sources.contains(&("s2".to_owned(), SearchSource::Note)));

    assert_eq!(search(&t, "GÖRÜŞME").len(), 1);
    assert_eq!(search(&t, "ÇAĞRI").len(), 1);
    assert_eq!(
        search(&t, "IĞDIR").len(),
        1,
        "dotless capital I finds ığdır"
    );
}

#[test]
fn turkish_letters_stay_distinct_from_their_ascii_look_alikes() {
    let t = seeded();
    // remove_diacritics 0: "gorusme" is a different word than "görüşme".
    assert!(search(&t, "gorusme").is_empty());
    assert!(search(&t, "toplantı").len() == 1);
}

#[test]
fn note_decisions_and_questions_are_searchable() {
    let t = seeded();
    let hits = search(&t, "lansman tarihi");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].source, SearchSource::Note);
    assert_eq!(hits[0].session_id, SessionId::new("s2"));
    assert_eq!(hits[0].start_ms, None);
    assert_eq!(search(&t, "bütçe").len(), 1);
}

#[test]
fn all_words_must_match() {
    let t = seeded();
    assert_eq!(search(&t, "toplantı müşteri").len(), 0);
    assert_eq!(search(&t, "müşteri görüşme").len(), 1);
}

#[test]
fn free_text_with_search_syntax_is_safe() {
    let t = seeded();
    for query in [
        "",
        "   ",
        "\"",
        "toplantı OR",
        "(toplantı",
        "toplantı*",
        "NEAR(a b)",
        "a:b",
        "-toplantı",
        "'",
    ] {
        // Must not error out; the contents of the result do not matter here.
        let _ = search(&t, query);
    }
    assert!(search(&t, "").is_empty());
    assert_eq!(search(&t, "\"toplantı\"").len(), 1);
}

#[test]
fn limit_is_respected() {
    let t = seeded();
    let index = SqliteSearchIndex::new(t.db.clone());
    assert_eq!(block_on(index.search("iş planı", 1)).unwrap().len(), 1);
    assert!(block_on(index.search("iş planı", 0)).unwrap().is_empty());
}

#[test]
fn replacing_a_transcript_replaces_its_index_entries() {
    let t = seeded();
    let transcripts = SqliteTranscriptRepository::new(t.db.clone());
    block_on(transcripts.replace_for_session(
        &SessionId::new("s1"),
        &[segment("s1", 0, "Tamamen yeni içerik")],
    ))
    .unwrap();
    assert!(search(&t, "toplantı").is_empty());
    assert_eq!(search(&t, "yeni içerik").len(), 1);

    block_on(transcripts.replace_for_session(&SessionId::new("s1"), &[])).unwrap();
    assert!(search(&t, "yeni").is_empty());
}

#[test]
fn resaving_and_deleting_a_note_updates_the_index() {
    let t = seeded();
    let notes = SqliteNoteRepository::new(t.db.clone());
    let mut note = block_on(notes.get(&NoteId::new("n1"))).unwrap().unwrap();
    note.summary = "Revize özet metni".to_owned();
    block_on(notes.save(&note, &[])).unwrap();
    assert_eq!(search(&t, "revize").len(), 1);
    assert!(search(&t, "onaylandı").is_empty());

    block_on(notes.delete(&NoteId::new("n1"))).unwrap();
    assert!(search(&t, "revize").is_empty());
    assert!(search(&t, "bütçe").is_empty());
}

#[test]
fn deleting_a_session_removes_its_search_entries() {
    let t = seeded();
    let sessions = SqliteSessionRepository::new(t.db.clone());
    block_on(sessions.delete(&SessionId::new("s1"))).unwrap();
    assert!(search(&t, "toplantı").is_empty());
    assert!(search(&t, "görüşme").is_empty());
    // The other session is untouched.
    assert_eq!(search(&t, "iş planı").len(), 1);

    block_on(sessions.delete(&SessionId::new("s2"))).unwrap();
    assert!(search(&t, "iş planı").is_empty());
}

#[test]
fn moving_a_note_to_another_session_leaves_no_stale_index_entry() {
    let t = seeded();
    let notes = SqliteNoteRepository::new(t.db.clone());
    let mut note = block_on(notes.get(&NoteId::new("n1"))).unwrap().unwrap();
    assert_eq!(note.session_id, SessionId::new("s2"));
    note.session_id = SessionId::new("s1");
    block_on(notes.save(&note, &[])).unwrap();

    let hits = search(&t, "bütçe");
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!(hits[0].session_id, SessionId::new("s1"));
    assert_eq!(hits[0].source, SearchSource::Note);

    // Nothing is left behind under the old session: deleting the *new* owner removes it all.
    let sessions = SqliteSessionRepository::new(t.db.clone());
    block_on(sessions.delete(&SessionId::new("s1"))).unwrap();
    assert!(search(&t, "bütçe").is_empty());
    assert!(search(&t, "onaylandı").is_empty());
}
