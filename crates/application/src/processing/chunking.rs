//! Token budgets and the time-ordered text the note is built from.
//!
//! A long meeting does not fit one model call, so the transcript (with the frame descriptions
//! woven in at their timestamps) is cut into chunks. Cuts only ever fall *between* timeline
//! entries: a transcript segment is never split in the middle.

use tan_domain::{EpochMs, Frame, ProviderKind, ProviderProfile, TranscriptSegment};

/// Rough size of a token in characters. Deliberately low (Turkish and code tokenize into
/// shorter pieces than English prose), so budgets err on the safe side.
pub const CHARS_PER_TOKEN: usize = 3;

/// Tokens reserved in every call for the system prompt, the header and the schema.
pub const PROMPT_OVERHEAD_TOKENS: u32 = 1_500;

/// How much a model call may carry. Defaults live here and nowhere else; a deployment can
/// swap [`TokenBudget::for_profile`] for its own function per provider/model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenBudget {
    /// Largest prompt the pipeline sends in one call, in tokens (system prompt included).
    pub input_tokens: u32,
    /// Largest reply the pipeline asks for, in tokens.
    pub reply_tokens: u32,
}

impl TokenBudget {
    /// The default budget of a provider kind. These sit well below each provider's real
    /// context window: the aim is reliable answers and a bounded cost per call, not filling
    /// the window. A local Ollama model usually serves a small context (a few thousand
    /// tokens unless the server was configured otherwise).
    #[must_use]
    pub const fn for_provider(kind: ProviderKind) -> Self {
        match kind {
            ProviderKind::OpenAi
            | ProviderKind::Anthropic
            | ProviderKind::ClaudeCli
            | ProviderKind::CodexCli => Self {
                input_tokens: 48_000,
                reply_tokens: 4_096,
            },
            ProviderKind::Gemini => Self {
                input_tokens: 96_000,
                reply_tokens: 8_192,
            },
            ProviderKind::DeepSeek => Self {
                input_tokens: 32_000,
                reply_tokens: 4_096,
            },
            ProviderKind::Ollama => Self {
                input_tokens: 6_000,
                reply_tokens: 2_048,
            },
        }
    }

    /// The budget for a configured profile (today: its provider's default).
    #[must_use]
    pub const fn for_profile(profile: &ProviderProfile) -> Self {
        Self::for_provider(profile.kind)
    }

    /// How many characters of transcript fit one call after the fixed prompt overhead.
    #[must_use]
    pub fn content_chars(self) -> usize {
        let tokens = self
            .input_tokens
            .saturating_sub(PROMPT_OVERHEAD_TOKENS)
            .max(256);
        usize::try_from(tokens).unwrap_or(usize::MAX) * CHARS_PER_TOKEN
    }
}

/// Estimated tokens of a text.
#[must_use]
pub fn estimate_tokens(text: &str) -> usize {
    text.chars().count().div_ceil(CHARS_PER_TOKEN)
}

/// `HH:MM:SS` of an offset in milliseconds.
#[must_use]
pub fn format_offset(ms: u64) -> String {
    let total = ms / 1_000;
    format!(
        "{:02}:{:02}:{:02}",
        total / 3_600,
        total / 60 % 60,
        total % 60
    )
}

/// One line of the merged transcript/frame timeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineEntry {
    /// Offset from the session start, in milliseconds.
    pub at_ms: u64,
    /// The rendered line (no trailing newline).
    pub text: String,
}

/// The short id a frame gets in prompts (`F1`, `F2`, ...): far cheaper than a UUID and easy for
/// a model to copy back.
#[must_use]
pub fn frame_alias(index: usize) -> String {
    format!("F{}", index + 1)
}

fn single_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Renders the transcript and the described frames as one list ordered by time. Frames are
/// numbered `F1..` in the order given (the caller keeps the alias-to-frame mapping).
///
/// Segments that start together keep their input order, and a frame comes after the segments
/// that start at the same moment.
#[must_use]
pub fn build_timeline(segments: &[TranscriptSegment], frames: &[&Frame]) -> Vec<TimelineEntry> {
    let mut entries: Vec<(u64, u8, TimelineEntry)> = Vec::new();
    for segment in segments {
        let speaker = segment.speaker.as_deref().unwrap_or("unknown");
        entries.push((
            segment.start_ms,
            0,
            TimelineEntry {
                at_ms: segment.start_ms,
                text: format!(
                    "[{}] {speaker}: {}",
                    format_offset(segment.start_ms),
                    single_line(&segment.text)
                ),
            },
        ));
    }
    for (index, frame) in frames.iter().enumerate() {
        let description = frame
            .description
            .as_deref()
            .map_or_else(String::new, single_line);
        entries.push((
            frame.timestamp_ms,
            1,
            TimelineEntry {
                at_ms: frame.timestamp_ms,
                text: format!(
                    "[{}] (screen {}) {description}",
                    format_offset(frame.timestamp_ms),
                    frame_alias(index)
                ),
            },
        ));
    }
    // Stable sort: equal keys keep their relative (input) order.
    entries.sort_by_key(|(at, rank, _)| (*at, *rank));
    entries.into_iter().map(|(_, _, entry)| entry).collect()
}

/// Splits `items` into consecutive runs whose combined `size_of` stays within `max_size`.
///
/// An item is never cut: one that is larger than `max_size` on its own gets a run to itself.
/// The runs cover every item exactly once, in order.
#[must_use]
pub fn split_into_chunks<T>(
    items: &[T],
    max_size: usize,
    size_of: impl Fn(&T) -> usize,
) -> Vec<&[T]> {
    let mut chunks = Vec::new();
    let mut start = 0;
    let mut size = 0;
    for (index, item) in items.iter().enumerate() {
        // `+ 1` accounts for the line break between entries.
        let item_size = size_of(item) + 1;
        if index > start && size + item_size > max_size {
            chunks.push(&items[start..index]);
            start = index;
            size = 0;
        }
        size += item_size;
    }
    if start < items.len() {
        chunks.push(&items[start..]);
    }
    chunks
}

/// The text of a timeline slice, one entry per line.
#[must_use]
pub fn join_entries(entries: &[TimelineEntry]) -> String {
    entries
        .iter()
        .map(|entry| entry.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

// Calendar helpers (UTC). The application layer has no time-zone database, so dates in notes
// and exports are UTC; the algorithms are Howard Hinnant's `days_from_civil`/`civil_from_days`.

const MS_PER_DAY: i64 = 86_400_000;

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

const fn is_leap_year(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

const fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if is_leap_year(year) => 29,
        _ => 28,
    }
}

/// Parses `YYYY-MM-DD` (a trailing time part is ignored) into midnight UTC. Impossible dates
/// and years outside 1970..=2200 give `None`.
#[must_use]
pub fn parse_iso_date(text: &str) -> Option<EpochMs> {
    let text = text.trim();
    let date = match text.split_once(['T', ' ']) {
        Some((date, _)) => date,
        None => text,
    };
    let mut parts = date.split('-');
    let (year, month, day) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() || year.len() != 4 || month.len() != 2 || day.len() != 2 {
        return None;
    }
    let digits = |s: &str| s.chars().all(|c| c.is_ascii_digit());
    if !(digits(year) && digits(month) && digits(day)) {
        return None;
    }
    let (year, month, day) = (
        year.parse::<i64>().ok()?,
        month.parse::<i64>().ok()?,
        day.parse::<i64>().ok()?,
    );
    if !(1970..=2200).contains(&year)
        || !(1..=12).contains(&month)
        || !(1..=days_in_month(year, month)).contains(&day)
    {
        return None;
    }
    Some(EpochMs::new(days_from_civil(year, month, day) * MS_PER_DAY))
}

/// `YYYY-MM-DD` (UTC) of an instant.
#[must_use]
pub fn utc_date_string(at: EpochMs) -> String {
    let (year, month, day) = civil_from_days(at.get().div_euclid(MS_PER_DAY));
    format!("{year:04}-{month:02}-{day:02}")
}

/// `YYYY-MM-DD HH:MM UTC` of an instant.
#[must_use]
pub fn utc_datetime_string(at: EpochMs) -> String {
    let minutes = at.get().div_euclid(60_000).rem_euclid(1_440);
    format!(
        "{} {:02}:{:02} UTC",
        utc_date_string(at),
        minutes / 60,
        minutes % 60
    )
}

/// The first and last offset of a non-empty timeline slice.
#[must_use]
pub fn span_of(entries: &[TimelineEntry]) -> Option<(u64, u64)> {
    Some((entries.first()?.at_ms, entries.last()?.at_ms))
}

#[cfg(test)]
mod tests {
    use tan_domain::{FrameId, SessionId, TrackKind};

    use super::*;

    fn segment(start_ms: u64, speaker: &str, text: &str) -> TranscriptSegment {
        TranscriptSegment {
            session_id: SessionId::new("s"),
            track: TrackKind::System,
            start_ms,
            end_ms: start_ms + 1_000,
            language: None,
            text: text.to_owned(),
            speaker: Some(speaker.to_owned()),
        }
    }

    fn frame(ts: u64, description: &str) -> Frame {
        Frame {
            id: FrameId::new(format!("f{ts}")),
            session_id: SessionId::new("s"),
            timestamp_ms: ts,
            path: "frames/x.png".to_owned(),
            perceptual_hash: 0,
            description: Some(description.to_owned()),
        }
    }

    #[test]
    fn offsets_render_as_clock_time() {
        assert_eq!(format_offset(0), "00:00:00");
        assert_eq!(format_offset(3_723_000), "01:02:03");
    }

    #[test]
    fn timeline_weaves_frames_into_the_transcript_by_time() {
        let segments = [
            segment(0, "me", "hello"),
            segment(10_000, "others", "slide"),
        ];
        let f = frame(5_000, "title slide");
        let timeline = build_timeline(&segments, &[&f]);
        let texts: Vec<&str> = timeline.iter().map(|e| e.text.as_str()).collect();
        assert_eq!(
            texts,
            [
                "[00:00:00] me: hello",
                "[00:00:05] (screen F1) title slide",
                "[00:00:10] others: slide"
            ]
        );
    }

    #[test]
    fn multi_line_text_is_flattened_to_one_entry() {
        let timeline = build_timeline(&[segment(0, "me", "a\n\n  b\tc")], &[]);
        assert_eq!(timeline[0].text, "[00:00:00] me: a b c");
    }

    #[test]
    fn chunks_never_split_an_item_and_cover_everything() {
        let items: Vec<String> = (0..40).map(|i| format!("segment number {i}")).collect();
        let chunks = split_into_chunks(&items, 100, String::len);
        assert!(chunks.len() > 1);
        let rejoined: Vec<&String> = chunks.iter().flat_map(|c| c.iter()).collect();
        assert_eq!(rejoined, items.iter().collect::<Vec<_>>());
        for chunk in &chunks {
            let size: usize = chunk.iter().map(|s| s.len() + 1).sum();
            assert!(size <= 100 || chunk.len() == 1, "chunk too large: {size}");
        }
    }

    #[test]
    fn an_oversized_item_gets_its_own_chunk() {
        let items = vec!["a".to_owned(), "b".repeat(500), "c".to_owned()];
        let chunks = split_into_chunks(&items, 50, String::len);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[1].len(), 1);
        assert_eq!(chunks[1][0].len(), 500);
    }

    #[test]
    fn short_input_stays_in_one_chunk_and_empty_input_in_none() {
        let items = vec!["a".to_owned(), "b".to_owned()];
        assert_eq!(split_into_chunks(&items, 1_000, String::len).len(), 1);
        assert_eq!(
            split_into_chunks::<String>(&[], 1_000, String::len).len(),
            0
        );
    }

    #[test]
    fn budgets_are_defined_for_every_provider_and_leave_room_for_the_prompt() {
        for &kind in ProviderKind::ALL {
            let budget = TokenBudget::for_provider(kind);
            assert!(budget.input_tokens > PROMPT_OVERHEAD_TOKENS);
            assert!(budget.reply_tokens > 0);
            assert!(budget.content_chars() > 0);
        }
        assert!(
            TokenBudget::for_provider(ProviderKind::Ollama).input_tokens
                < TokenBudget::for_provider(ProviderKind::Gemini).input_tokens
        );
    }

    #[test]
    fn dates_round_trip_through_epoch_time() {
        let parsed = parse_iso_date("2026-11-06").unwrap();
        assert_eq!(utc_date_string(parsed), "2026-11-06");
        assert_eq!(parse_iso_date("1970-01-01"), Some(EpochMs::new(0)));
        assert_eq!(
            parse_iso_date("2024-02-29T10:00:00Z")
                .map(utc_date_string)
                .as_deref(),
            Some("2024-02-29")
        );
        assert_eq!(
            utc_datetime_string(EpochMs::new(1_793_972_580_000)),
            "2026-11-06 13:43 UTC"
        );
    }

    #[test]
    fn impossible_dates_are_rejected() {
        for bad in [
            "2025-02-29",
            "2025-13-01",
            "2025-00-10",
            "next friday",
            "2025-1-5",
            "1969-12-31",
            "",
        ] {
            assert_eq!(parse_iso_date(bad), None, "{bad}");
        }
    }

    #[test]
    fn token_estimate_rounds_up() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("abc"), 1);
        assert_eq!(estimate_tokens("abcd"), 2);
    }
}
