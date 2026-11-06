// `assert!(x.is_empty())` / `assert!(a == b)` read fine in tests; pedantic clippy would rewrite them.
#![allow(clippy::assert_is_empty, clippy::manual_assert_eq)]

mod common;

use common::{block_on, segment, session, test_db, TestDb};
use tan_application::ports::{
    FrameRepository, NoteRepository, ProviderProfileRepository, SecretMetadataRepository,
    SessionRepository, TaskAssignmentRepository, TranscriptRepository, VariableRepository,
};
use tan_application::RepositoryError;
use tan_domain::{
    ActionItem, ActionItemId, AudioTrack, EpochMs, Frame, FrameId, LanguageCode, LanguageSetting,
    Note, NoteId, ProviderKind, ProviderProfile, ProviderProfileId, SecretId, SecretRef, SessionId,
    SessionStatus, TaskAssignment, TaskKind, TrackKind, Variable, VariableId,
};
use tan_infra_storage::{
    new_id, SqliteFrameRepository, SqliteNoteRepository, SqliteProviderProfileRepository,
    SqliteSecretMetadataRepository, SqliteSessionRepository, SqliteTaskAssignmentRepository,
    SqliteTranscriptRepository, SqliteVariableRepository,
};

fn sid(id: &str) -> SessionId {
    SessionId::new(id)
}

fn with_session(t: &TestDb, id: &str) {
    block_on(SqliteSessionRepository::new(t.db.clone()).create(&session(id, 1_000))).unwrap();
}

// ---------------------------------------------------------------------------------------------
// sessions
// ---------------------------------------------------------------------------------------------

#[test]
fn session_crud_round_trip() {
    let t = test_db();
    let repo = SqliteSessionRepository::new(t.db.clone());

    let mut s = session("s1", 1_000);
    s.language = LanguageSetting::Fixed(LanguageCode::new("en").unwrap());
    block_on(repo.create(&s)).unwrap();
    assert_eq!(block_on(repo.get(&sid("s1"))).unwrap(), Some(s.clone()));

    s.finish_recording(EpochMs::new(9_000)).unwrap();
    s.title = "Renamed".to_owned();
    s.note_language = LanguageCode::english();
    block_on(repo.update(&s)).unwrap();
    let loaded = block_on(repo.get(&sid("s1"))).unwrap().unwrap();
    assert_eq!(loaded, s);
    assert_eq!(loaded.status(), SessionStatus::Recorded);
    assert_eq!(loaded.ended_at(), Some(EpochMs::new(9_000)));

    block_on(repo.delete(&sid("s1"))).unwrap();
    assert_eq!(block_on(repo.get(&sid("s1"))).unwrap(), None);
}

#[test]
fn session_list_is_newest_first_and_errors_are_typed() {
    let t = test_db();
    let repo = SqliteSessionRepository::new(t.db.clone());
    block_on(repo.create(&session("old", 1_000))).unwrap();
    block_on(repo.create(&session("new", 5_000))).unwrap();
    let ids: Vec<_> = block_on(repo.list())
        .unwrap()
        .into_iter()
        .map(|s| s.id)
        .collect();
    assert_eq!(ids, [sid("new"), sid("old")]);

    assert!(matches!(
        block_on(repo.create(&session("new", 1))),
        Err(RepositoryError::Conflict(_))
    ));
    assert!(matches!(
        block_on(repo.update(&session("missing", 1))),
        Err(RepositoryError::NotFound {
            entity: "session",
            ..
        })
    ));
    assert!(matches!(
        block_on(repo.delete(&sid("missing"))),
        Err(RepositoryError::NotFound { .. })
    ));
}

#[test]
fn audio_tracks_upsert_per_kind() {
    let t = test_db();
    with_session(&t, "s1");
    let repo = SqliteSessionRepository::new(t.db.clone());

    let mic = AudioTrack {
        session_id: sid("s1"),
        kind: TrackKind::Microphone,
        path: "audio/mic.wav".to_owned(),
        sample_rate: 48_000,
        duration_ms: 3_600_000,
    };
    let system = AudioTrack {
        kind: TrackKind::System,
        path: "audio/system.wav".to_owned(),
        ..mic.clone()
    };
    block_on(repo.save_audio_track(&mic)).unwrap();
    block_on(repo.save_audio_track(&system)).unwrap();

    let longer_mic = AudioTrack {
        duration_ms: 4_000_000,
        ..mic.clone()
    };
    block_on(repo.save_audio_track(&longer_mic)).unwrap();

    let tracks = block_on(repo.list_audio_tracks(&sid("s1"))).unwrap();
    assert_eq!(tracks.len(), 2, "one track per kind");
    assert!(tracks.contains(&longer_mic));
    assert!(tracks.contains(&system));

    // A track for a nonexistent session violates the foreign key.
    let orphan = AudioTrack {
        session_id: sid("nope"),
        ..mic
    };
    assert!(matches!(
        block_on(repo.save_audio_track(&orphan)),
        Err(RepositoryError::Conflict(_))
    ));
}

// ---------------------------------------------------------------------------------------------
// frames
// ---------------------------------------------------------------------------------------------

fn frame(id: &str, session_id: &str, timestamp_ms: u64, hash: u64) -> Frame {
    Frame {
        id: FrameId::new(id),
        session_id: sid(session_id),
        timestamp_ms,
        path: format!("frames/{id}.png"),
        perceptual_hash: hash,
        description: None,
    }
}

#[test]
fn frame_crud_round_trip_with_full_range_hash() {
    let t = test_db();
    with_session(&t, "s1");
    let repo = SqliteFrameRepository::new(t.db.clone());

    let late = frame("f2", "s1", 9_000, u64::MAX);
    let early = frame("f1", "s1", 3_000, 0x8000_0000_0000_0001);
    block_on(repo.add(&late)).unwrap();
    block_on(repo.add(&early)).unwrap();

    assert_eq!(
        block_on(repo.get(&FrameId::new("f2"))).unwrap(),
        Some(late.clone())
    );
    let listed = block_on(repo.list_for_session(&sid("s1"))).unwrap();
    assert_eq!(
        listed,
        [early.clone(), late.clone()],
        "ordered by timestamp"
    );

    block_on(repo.set_description(&early.id, Some("Slide: Q3 roadmap"))).unwrap();
    let described = block_on(repo.get(&early.id)).unwrap().unwrap();
    assert_eq!(described.description.as_deref(), Some("Slide: Q3 roadmap"));
    block_on(repo.set_description(&early.id, None)).unwrap();
    assert_eq!(
        block_on(repo.get(&early.id)).unwrap().unwrap().description,
        None
    );

    block_on(repo.delete(&late.id)).unwrap();
    assert_eq!(block_on(repo.get(&late.id)).unwrap(), None);

    assert!(matches!(
        block_on(repo.add(&early)),
        Err(RepositoryError::Conflict(_))
    ));
    assert!(matches!(
        block_on(repo.set_description(&FrameId::new("nope"), Some("x"))),
        Err(RepositoryError::NotFound { .. })
    ));
    assert!(matches!(
        block_on(repo.add(&frame("f9", "no-session", 1, 1))),
        Err(RepositoryError::Conflict(_))
    ));
}

// ---------------------------------------------------------------------------------------------
// transcripts
// ---------------------------------------------------------------------------------------------

#[test]
fn transcript_replace_and_list_round_trip() {
    let t = test_db();
    with_session(&t, "s1");
    with_session(&t, "s2");
    let repo = SqliteTranscriptRepository::new(t.db.clone());

    let mut second = segment("s1", 5_000, "ikinci");
    second.language = None;
    second.speaker = None;
    second.track = TrackKind::Microphone;
    let first = segment("s1", 1_000, "birinci");
    block_on(repo.replace_for_session(&sid("s1"), &[second.clone(), first.clone()])).unwrap();
    block_on(repo.replace_for_session(&sid("s2"), &[segment("s2", 0, "başka")])).unwrap();

    assert_eq!(
        block_on(repo.list_for_session(&sid("s1"))).unwrap(),
        [first, second.clone()]
    );

    // Replace overwrites.
    block_on(repo.replace_for_session(&sid("s1"), std::slice::from_ref(&second))).unwrap();
    assert_eq!(
        block_on(repo.list_for_session(&sid("s1"))).unwrap(),
        [second]
    );
    // Clear.
    block_on(repo.replace_for_session(&sid("s1"), &[])).unwrap();
    assert!(block_on(repo.list_for_session(&sid("s1")))
        .unwrap()
        .is_empty());
    // The other session is unaffected.
    assert_eq!(
        block_on(repo.list_for_session(&sid("s2"))).unwrap().len(),
        1
    );

    // A segment for a different session is rejected without touching the data.
    let err =
        block_on(repo.replace_for_session(&sid("s2"), &[segment("s1", 0, "yanlış")])).unwrap_err();
    assert!(matches!(err, RepositoryError::Invalid(_)));
    assert_eq!(
        block_on(repo.list_for_session(&sid("s2"))).unwrap().len(),
        1
    );
}

// ---------------------------------------------------------------------------------------------
// notes and action items
// ---------------------------------------------------------------------------------------------

fn note(id: &str, session_id: &str, created_at: i64) -> Note {
    Note {
        id: NoteId::new(id),
        session_id: sid(session_id),
        language: LanguageCode::turkish(),
        summary: format!("Özet {id}"),
        decisions: vec!["Karar 1".to_owned(), "Karar 2".to_owned()],
        open_questions: vec!["Soru 1".to_owned()],
        frame_refs: vec![],
        created_at: EpochMs::new(created_at),
    }
}

fn item(id: &str, note_id: &str, task: &str) -> ActionItem {
    ActionItem {
        id: ActionItemId::new(id),
        note_id: NoteId::new(note_id),
        task: task.to_owned(),
        owner: Some("Ayşe".to_owned()),
        due: Some(EpochMs::new(1_800_000_000_000)),
        done: false,
    }
}

#[test]
fn note_with_frame_refs_and_action_items_round_trips() {
    let t = test_db();
    with_session(&t, "s1");
    let frames = SqliteFrameRepository::new(t.db.clone());
    block_on(frames.add(&frame("f1", "s1", 1_000, 1))).unwrap();
    block_on(frames.add(&frame("f2", "s1", 2_000, 2))).unwrap();
    let repo = SqliteNoteRepository::new(t.db.clone());

    let mut n = note("n1", "s1", 100);
    n.frame_refs = vec![FrameId::new("f2"), FrameId::new("f1")];
    let items = vec![
        item("a2", "n1", "İkinci iş"),
        ActionItem {
            owner: None,
            due: None,
            ..item("a1", "n1", "Birinci iş")
        },
    ];
    block_on(repo.save(&n, &items)).unwrap();

    assert_eq!(block_on(repo.get(&n.id)).unwrap(), Some(n.clone()));
    assert_eq!(
        block_on(repo.list_action_items(&n.id)).unwrap(),
        items,
        "order is preserved"
    );

    // Re-saving updates the note and replaces the items.
    n.summary = "Güncel özet".to_owned();
    n.decisions = vec!["Tek karar".to_owned()];
    n.frame_refs = vec![FrameId::new("f1")];
    let replacement = vec![item("a3", "n1", "Yeni iş")];
    block_on(repo.save(&n, &replacement)).unwrap();
    assert_eq!(block_on(repo.get(&n.id)).unwrap(), Some(n.clone()));
    assert_eq!(
        block_on(repo.list_action_items(&n.id)).unwrap(),
        replacement
    );
}

#[test]
fn replacing_a_sessions_note_leaves_exactly_the_new_note_and_is_atomic() {
    let t = test_db();
    with_session(&t, "s1");
    with_session(&t, "s2");
    let repo = SqliteNoteRepository::new(t.db.clone());
    block_on(repo.save(&note("old-1", "s1", 1), &[item("a1", "old-1", "Eski")])).unwrap();
    block_on(repo.save(&note("old-2", "s1", 2), &[])).unwrap();
    block_on(repo.save(&note("other", "s2", 3), &[])).unwrap();

    // A failing replacement (dangling frame reference) changes nothing.
    let mut dangling = note("new", "s1", 10);
    dangling.frame_refs = vec![FrameId::new("no-such-frame")];
    assert!(block_on(repo.replace_for_session(&dangling, &[])).is_err());
    assert!(block_on(repo.get(&NoteId::new("old-1"))).unwrap().is_some());
    assert!(block_on(repo.get(&NoteId::new("old-2"))).unwrap().is_some());

    let fresh = note("new", "s1", 10);
    let items = vec![item("a2", "new", "Yeni")];
    block_on(repo.replace_for_session(&fresh, &items)).unwrap();
    assert_eq!(block_on(repo.get(&fresh.id)).unwrap(), Some(fresh.clone()));
    assert_eq!(block_on(repo.list_action_items(&fresh.id)).unwrap(), items);
    assert!(block_on(repo.get(&NoteId::new("old-1"))).unwrap().is_none());
    assert!(block_on(repo.get(&NoteId::new("old-2"))).unwrap().is_none());
    assert!(block_on(repo.list_action_items(&NoteId::new("old-1")))
        .unwrap()
        .is_empty());
    assert!(block_on(repo.get(&NoteId::new("other"))).unwrap().is_some());

    // Running it again with the same note id keeps it (an update, not a delete).
    block_on(repo.replace_for_session(&fresh, &items)).unwrap();
    assert_eq!(block_on(repo.get(&fresh.id)).unwrap(), Some(fresh));
}

#[test]
fn action_items_can_be_toggled_edited_and_removed() {
    let t = test_db();
    with_session(&t, "s1");
    let repo = SqliteNoteRepository::new(t.db.clone());
    block_on(repo.save(&note("n1", "s1", 1), &[item("a1", "n1", "Birinci")])).unwrap();

    let id = ActionItemId::new("a1");
    block_on(repo.set_action_item_done(&id, true)).unwrap();
    assert!(block_on(repo.list_action_items(&NoteId::new("n1"))).unwrap()[0].done);
    block_on(repo.set_action_item_done(&id, false)).unwrap();
    assert!(!block_on(repo.list_action_items(&NoteId::new("n1"))).unwrap()[0].done);

    // Upsert: edit an existing item in place, append a new one after it.
    let edited = ActionItem {
        task: "Düzeltilmiş".to_owned(),
        done: true,
        ..item("a1", "n1", "")
    };
    block_on(repo.upsert_action_item(&edited)).unwrap();
    block_on(repo.upsert_action_item(&item("a2", "n1", "İkinci"))).unwrap();
    let listed = block_on(repo.list_action_items(&NoteId::new("n1"))).unwrap();
    assert_eq!(
        listed.iter().map(|i| i.task.as_str()).collect::<Vec<_>>(),
        ["Düzeltilmiş", "İkinci"]
    );
    assert!(listed[0].done);

    block_on(repo.delete_action_item(&id)).unwrap();
    assert_eq!(
        block_on(repo.list_action_items(&NoteId::new("n1")))
            .unwrap()
            .len(),
        1
    );

    assert!(matches!(
        block_on(repo.set_action_item_done(&ActionItemId::new("nope"), true)),
        Err(RepositoryError::NotFound { .. })
    ));
    assert!(matches!(
        block_on(repo.delete_action_item(&id)),
        Err(RepositoryError::NotFound { .. })
    ));
    // An item for a nonexistent note violates the foreign key.
    assert!(matches!(
        block_on(repo.upsert_action_item(&item("a9", "ghost", "x"))),
        Err(RepositoryError::Conflict(_))
    ));
}

#[test]
fn latest_note_for_session_and_delete() {
    let t = test_db();
    with_session(&t, "s1");
    let repo = SqliteNoteRepository::new(t.db.clone());
    assert_eq!(block_on(repo.latest_for_session(&sid("s1"))).unwrap(), None);

    block_on(repo.save(&note("n-old", "s1", 100), &[item("a1", "n-old", "x")])).unwrap();
    block_on(repo.save(&note("n-new", "s1", 200), &[])).unwrap();
    let latest = block_on(repo.latest_for_session(&sid("s1")))
        .unwrap()
        .unwrap();
    assert_eq!(latest.id, NoteId::new("n-new"));

    block_on(repo.delete(&NoteId::new("n-old"))).unwrap();
    assert_eq!(block_on(repo.get(&NoteId::new("n-old"))).unwrap(), None);
    assert!(block_on(repo.list_action_items(&NoteId::new("n-old")))
        .unwrap()
        .is_empty());
    assert!(matches!(
        block_on(repo.delete(&NoteId::new("n-old"))),
        Err(RepositoryError::NotFound { .. })
    ));
}

#[test]
fn note_save_rejects_foreign_items_and_dangling_references() {
    let t = test_db();
    with_session(&t, "s1");
    let repo = SqliteNoteRepository::new(t.db.clone());

    let err =
        block_on(repo.save(&note("n1", "s1", 1), &[item("a1", "other-note", "x")])).unwrap_err();
    assert!(matches!(err, RepositoryError::Invalid(_)));

    let mut dangling = note("n2", "s1", 1);
    dangling.frame_refs = vec![FrameId::new("no-such-frame")];
    assert!(matches!(
        block_on(repo.save(&dangling, &[])),
        Err(RepositoryError::Conflict(_))
    ));
    // The failed save is atomic: nothing of it was stored.
    assert_eq!(block_on(repo.get(&NoteId::new("n2"))).unwrap(), None);

    assert!(matches!(
        block_on(repo.save(&note("n3", "no-session", 1), &[])),
        Err(RepositoryError::Conflict(_))
    ));
}

#[test]
fn deleting_a_session_removes_everything_derived_from_it() {
    let t = test_db();
    with_session(&t, "s1");
    let frames = SqliteFrameRepository::new(t.db.clone());
    let transcripts = SqliteTranscriptRepository::new(t.db.clone());
    let notes = SqliteNoteRepository::new(t.db.clone());
    let sessions = SqliteSessionRepository::new(t.db.clone());

    block_on(frames.add(&frame("f1", "s1", 1, 1))).unwrap();
    block_on(transcripts.replace_for_session(&sid("s1"), &[segment("s1", 0, "merhaba")])).unwrap();
    block_on(notes.save(&note("n1", "s1", 1), &[item("a1", "n1", "x")])).unwrap();

    block_on(sessions.delete(&sid("s1"))).unwrap();

    assert_eq!(block_on(frames.get(&FrameId::new("f1"))).unwrap(), None);
    assert!(block_on(transcripts.list_for_session(&sid("s1")))
        .unwrap()
        .is_empty());
    assert_eq!(block_on(notes.get(&NoteId::new("n1"))).unwrap(), None);
    assert!(block_on(notes.list_action_items(&NoteId::new("n1")))
        .unwrap()
        .is_empty());
}

// ---------------------------------------------------------------------------------------------
// variables
// ---------------------------------------------------------------------------------------------

fn variable(id: &str, key: &str, value: &str) -> Variable {
    Variable {
        id: VariableId::new(id),
        key: key.to_owned(),
        value: value.to_owned(),
        description: format!("about {key}"),
    }
}

#[test]
fn variable_crud_round_trip() {
    let t = test_db();
    let repo = SqliteVariableRepository::new(t.db.clone());

    let b = variable("v2", "B_KEY", "2");
    let a = variable("v1", "A_KEY", "1");
    block_on(repo.upsert(&b)).unwrap();
    block_on(repo.upsert(&a)).unwrap();

    assert_eq!(block_on(repo.get(&a.id)).unwrap(), Some(a.clone()));
    assert_eq!(block_on(repo.get_by_key("B_KEY")).unwrap(), Some(b.clone()));
    assert_eq!(block_on(repo.get_by_key("nope")).unwrap(), None);
    assert_eq!(
        block_on(repo.list()).unwrap(),
        [a.clone(), b.clone()],
        "ordered by key"
    );

    let updated = Variable {
        value: "changed".to_owned(),
        ..a.clone()
    };
    block_on(repo.upsert(&updated)).unwrap();
    assert_eq!(block_on(repo.get(&a.id)).unwrap(), Some(updated));
    assert_eq!(block_on(repo.list()).unwrap().len(), 2);

    // A second variable cannot take an existing key.
    assert!(matches!(
        block_on(repo.upsert(&variable("v3", "B_KEY", "x"))),
        Err(RepositoryError::Conflict(_))
    ));

    block_on(repo.delete(&b.id)).unwrap();
    assert_eq!(block_on(repo.get(&b.id)).unwrap(), None);
    assert!(matches!(
        block_on(repo.delete(&b.id)),
        Err(RepositoryError::NotFound { .. })
    ));
}

// ---------------------------------------------------------------------------------------------
// secrets, provider profiles, task assignments
// ---------------------------------------------------------------------------------------------

fn secret(id: &str, name: &str) -> SecretRef {
    SecretRef {
        id: SecretId::new(id),
        name: name.to_owned(),
        description: format!("about {name}"),
        created_at: EpochMs::new(1_700_000_000_000),
    }
}

fn profile(id: &str, name: &str, secret_ref: Option<&str>) -> ProviderProfile {
    ProviderProfile {
        id: ProviderProfileId::new(id),
        kind: ProviderKind::OpenAi,
        display_name: name.to_owned(),
        model: "gpt-4o-mini".to_owned(),
        base_url: Some("https://example.invalid/v1".to_owned()),
        secret_ref: secret_ref.map(SecretId::new),
        supports_vision: true,
    }
}

#[test]
fn secret_metadata_crud_round_trip() {
    let t = test_db();
    let repo = SqliteSecretMetadataRepository::new(t.db.clone());

    let z = secret("s2", "Z key");
    let a = secret("s1", "A key");
    block_on(repo.create(&z)).unwrap();
    block_on(repo.create(&a)).unwrap();
    assert_eq!(block_on(repo.get(&a.id)).unwrap(), Some(a.clone()));
    assert_eq!(
        block_on(repo.list()).unwrap(),
        [a.clone(), z.clone()],
        "ordered by name"
    );

    let renamed = SecretRef {
        name: "Renamed".to_owned(),
        description: "new".to_owned(),
        created_at: EpochMs::new(0), // created_at is immutable
        ..a.clone()
    };
    block_on(repo.update(&renamed)).unwrap();
    let loaded = block_on(repo.get(&a.id)).unwrap().unwrap();
    assert_eq!(loaded.name, "Renamed");
    assert_eq!(loaded.description, "new");
    assert_eq!(loaded.created_at, a.created_at);

    assert!(matches!(
        block_on(repo.create(&secret("s3", "Z key"))),
        Err(RepositoryError::Conflict(_))
    ));
    assert!(matches!(
        block_on(repo.create(&z)),
        Err(RepositoryError::Conflict(_))
    ));
    assert!(matches!(
        block_on(repo.update(&secret("ghost", "x"))),
        Err(RepositoryError::NotFound { .. })
    ));

    block_on(repo.delete(&z.id)).unwrap();
    assert_eq!(block_on(repo.get(&z.id)).unwrap(), None);
    assert!(matches!(
        block_on(repo.delete(&z.id)),
        Err(RepositoryError::NotFound { .. })
    ));
}

#[test]
fn provider_profile_crud_round_trip_and_secret_reference_rules() {
    let t = test_db();
    let secrets = SqliteSecretMetadataRepository::new(t.db.clone());
    let profiles = SqliteProviderProfileRepository::new(t.db.clone());
    block_on(secrets.create(&secret("k1", "OpenAI key"))).unwrap();

    let with_key = profile("p1", "Cloud", Some("k1"));
    let local = ProviderProfile {
        kind: ProviderKind::Ollama,
        base_url: None,
        supports_vision: false,
        ..profile("p2", "Local", None)
    };
    block_on(profiles.upsert(&with_key)).unwrap();
    block_on(profiles.upsert(&local)).unwrap();
    assert_eq!(
        block_on(profiles.get(&with_key.id)).unwrap(),
        Some(with_key.clone())
    );
    assert_eq!(
        block_on(profiles.get(&local.id)).unwrap(),
        Some(local.clone())
    );
    assert_eq!(
        block_on(profiles.list()).unwrap(),
        [with_key.clone(), local.clone()]
    );

    let edited = ProviderProfile {
        model: "gpt-4o".to_owned(),
        ..with_key.clone()
    };
    block_on(profiles.upsert(&edited)).unwrap();
    assert_eq!(
        block_on(profiles.get(&with_key.id)).unwrap().unwrap().model,
        "gpt-4o"
    );

    // A dangling secret reference is rejected.
    assert!(matches!(
        block_on(profiles.upsert(&profile("p3", "Bad", Some("missing")))),
        Err(RepositoryError::Conflict(_))
    ));

    // Deleting the secret detaches it from the profile instead of deleting the profile.
    block_on(secrets.delete(&SecretId::new("k1"))).unwrap();
    assert_eq!(
        block_on(profiles.get(&with_key.id))
            .unwrap()
            .unwrap()
            .secret_ref,
        None
    );

    block_on(profiles.delete(&local.id)).unwrap();
    assert_eq!(block_on(profiles.get(&local.id)).unwrap(), None);
    assert!(matches!(
        block_on(profiles.delete(&local.id)),
        Err(RepositoryError::NotFound { .. })
    ));
}

#[test]
fn task_assignments_set_replace_clear_and_cascade() {
    let t = test_db();
    let profiles = SqliteProviderProfileRepository::new(t.db.clone());
    let assignments = SqliteTaskAssignmentRepository::new(t.db.clone());
    block_on(profiles.upsert(&profile("p1", "One", None))).unwrap();
    block_on(profiles.upsert(&profile("p2", "Two", None))).unwrap();

    assert_eq!(block_on(assignments.get(TaskKind::Summary)).unwrap(), None);

    let summary = TaskAssignment {
        task: TaskKind::Summary,
        profile_id: ProviderProfileId::new("p1"),
    };
    let vision = TaskAssignment {
        task: TaskKind::Vision,
        profile_id: ProviderProfileId::new("p1"),
    };
    block_on(assignments.set(&summary)).unwrap();
    block_on(assignments.set(&vision)).unwrap();
    assert_eq!(
        block_on(assignments.get(TaskKind::Summary)).unwrap(),
        Some(summary.clone())
    );

    // Re-assigning replaces rather than duplicates.
    let moved = TaskAssignment {
        profile_id: ProviderProfileId::new("p2"),
        ..summary
    };
    block_on(assignments.set(&moved)).unwrap();
    assert_eq!(
        block_on(assignments.get(TaskKind::Summary)).unwrap(),
        Some(moved.clone())
    );
    assert_eq!(block_on(assignments.list()).unwrap().len(), 2);

    // Dangling profile id is rejected.
    let dangling = TaskAssignment {
        task: TaskKind::Transcription,
        profile_id: ProviderProfileId::new("ghost"),
    };
    assert!(matches!(
        block_on(assignments.set(&dangling)),
        Err(RepositoryError::Conflict(_))
    ));

    // Clearing is idempotent.
    block_on(assignments.clear(TaskKind::Vision)).unwrap();
    block_on(assignments.clear(TaskKind::Vision)).unwrap();
    assert_eq!(block_on(assignments.get(TaskKind::Vision)).unwrap(), None);

    // Deleting a profile removes the assignments pointing at it.
    block_on(profiles.delete(&ProviderProfileId::new("p2"))).unwrap();
    assert_eq!(block_on(assignments.get(TaskKind::Summary)).unwrap(), None);
}

#[test]
fn minted_ids_work_as_primary_keys() {
    let t = test_db();
    let repo = SqliteSessionRepository::new(t.db.clone());
    let a = session(&new_id(), 1);
    let b = session(&new_id(), 2);
    block_on(repo.create(&a)).unwrap();
    block_on(repo.create(&b)).unwrap();
    assert_eq!(block_on(repo.list()).unwrap().len(), 2);
}
