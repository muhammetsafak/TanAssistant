//! Planning the pieces of a long recording for a cloud provider.

use tan_application::transcription::{plan_chunks, AudioSpan, SpeechMap, CLOUD_CHUNK_MS};

/// Window length of the synthetic energy track (what the voice-activity detector uses).
const WINDOW_MS: u64 = 30;
/// One hour.
const HOUR_MS: u64 = 60 * 60 * 1_000;
/// Raw 16 kHz mono 16-bit PCM per millisecond: the most an encoded piece can weigh (FLAC
/// never exceeds it by more than a few percent, and a piece of speech is far below it).
const PCM_BYTES_PER_MS: u64 = 32;
/// The smallest upload cap among the providers is `OpenAI`'s 25 MB.
const UPLOAD_CAP_BYTES: u64 = 25 * 1024 * 1024;
const QUIET: f32 = 0.0;
/// Level of the single silent window of a word-internal gap: as silent as the hush, but one
/// window wide.
const GAP: f32 = 0.0;
const LOUD: f32 = 0.3;
const HISS: f32 = 0.001;

/// Small deterministic generator, so the test needs no RNG crate.
struct Lcg(u64);

impl Lcg {
    fn between(&mut self, low: u64, high: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        low + (self.0 >> 33) % (high - low + 1)
    }
}

/// An hour of meeting: speech bursts of 3-28 s with pauses of 1.2-8 s between them (the
/// detector bridges anything shorter than a second), and now and then a monologue of 70 s with
/// no pause in it, whose only quiet moments are 300 ms hushes every 20 s (a single silent
/// window inside it, like the gap of a stop consonant, is no place to cut: see `GAP`).
fn one_hour_meeting() -> SpeechMap {
    let mut rng = Lcg(42);
    let windows = usize::try_from(HOUR_MS / WINDOW_MS).unwrap();
    let mut energies = vec![HISS; windows];
    let mut regions = Vec::new();
    let mut at = 500_u64;
    let mut burst = 0_u32;
    while at < HOUR_MS - 80_000 {
        burst += 1;
        let monologue = burst.is_multiple_of(25);
        let length = if monologue {
            70_000
        } else {
            rng.between(3_000, 28_000)
        };
        let region = AudioSpan {
            start_ms: at,
            end_ms: at + length,
        };
        for window in region.start_ms / WINDOW_MS..region.end_ms / WINDOW_MS {
            energies[usize::try_from(window).unwrap()] = LOUD;
        }
        if monologue {
            for quiet_at in [20_000, 40_000, 60_000] {
                let first = usize::try_from((region.start_ms + quiet_at) / WINDOW_MS).unwrap();
                energies[first..first + 10].fill(QUIET);
                // A decoy: one silent window, 2 s earlier than each hush, that must not win.
                energies[first - 66] = GAP;
            }
        }
        regions.push(region);
        at = region.end_ms + rng.between(1_200, 8_000);
    }
    SpeechMap {
        duration_ms: HOUR_MS,
        regions,
        energies,
        energy_window_ms: WINDOW_MS,
    }
}

fn inside(region: &AudioSpan, at: u64) -> bool {
    region.start_ms < at && at < region.end_ms
}

#[test]
fn a_sixty_minute_recording_is_cut_into_pieces_that_fit_every_limit() {
    let meeting = one_hour_meeting();
    let chunks = plan_chunks(&meeting, CLOUD_CHUNK_MS);

    assert_eq!(CLOUD_CHUNK_MS, 30_000);
    assert!(chunks.len() >= 120, "{} pieces", chunks.len());
    for chunk in &chunks {
        let length = chunk.end_ms - chunk.start_ms;
        assert!(length > 0 && length <= CLOUD_CHUNK_MS, "{chunk:?}");
        assert!(
            length * PCM_BYTES_PER_MS < UPLOAD_CAP_BYTES,
            "{chunk:?} could exceed the 25 MB cap"
        );
        assert!(chunk.end_ms <= meeting.duration_ms);
    }
    for pair in chunks.windows(2) {
        assert!(
            pair[0].end_ms <= pair[1].start_ms,
            "pieces overlap or are out of order: {pair:?}"
        );
    }
}

#[test]
fn no_speech_is_lost_and_no_cut_runs_through_it_except_in_a_monologue_at_its_quiet_spot() {
    let meeting = one_hour_meeting();
    let chunks = plan_chunks(&meeting, CLOUD_CHUNK_MS);

    // Every region is covered by the pieces, edge to edge.
    for region in &meeting.regions {
        let covered: u64 = chunks
            .iter()
            .map(|c| {
                c.end_ms
                    .min(region.end_ms)
                    .saturating_sub(c.start_ms.max(region.start_ms))
            })
            .sum();
        assert_eq!(covered, region.end_ms - region.start_ms, "{region:?}");
    }

    let mut cuts_in_speech = 0;
    for chunk in &chunks {
        for edge in [chunk.start_ms, chunk.end_ms] {
            let Some(region) = meeting.regions.iter().find(|r| inside(r, edge)) else {
                continue; // in a pause: a clean cut
            };
            // A cut inside speech is only allowed in a stretch longer than a piece, and then
            // exactly at the quiet window the detector energies show.
            assert!(
                region.end_ms - region.start_ms > CLOUD_CHUNK_MS,
                "a cut at {edge} ms runs through {region:?}"
            );
            let window = usize::try_from(edge / WINDOW_MS).unwrap();
            // Quiet all around, not just at one window: a decoy dip does not count.
            assert!(
                meeting.energies[window - 3..=window + 3]
                    .iter()
                    .all(|&energy| energy < 0.01),
                "the cut at {edge} ms is not in a sustained quiet stretch"
            );
            cuts_in_speech += 1;
        }
    }
    assert!(cuts_in_speech > 0, "the monologues must have been cut");
}

#[test]
fn silence_is_not_sent() {
    let meeting = one_hour_meeting();
    let chunks = plan_chunks(&meeting, CLOUD_CHUNK_MS);
    let sent: u64 = chunks.iter().map(|c| c.end_ms - c.start_ms).sum();
    let speech: u64 = meeting.regions.iter().map(|r| r.end_ms - r.start_ms).sum();
    assert!(sent >= speech);
    // Pauses inside a piece are sent (they keep the timing natural), those between pieces are not.
    assert!(
        sent < HOUR_MS,
        "{sent} ms sent of an hour; the pauses between pieces must be dropped"
    );
}

#[test]
fn a_longer_limit_makes_fewer_pieces_and_a_shorter_one_more() {
    let meeting = one_hour_meeting();
    let short = plan_chunks(&meeting, 15_000).len();
    let default = plan_chunks(&meeting, CLOUD_CHUNK_MS).len();
    let long = plan_chunks(&meeting, 120_000).len();
    assert!(
        short > default && default > long,
        "{short} {default} {long}"
    );
}
