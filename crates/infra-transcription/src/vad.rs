//! Voice activity detection: find the speech in a recording so whisper never sees the silence
//! (ADR-011).
//!
//! Whisper's cost is per 30-second window, whatever is in it, so a meeting track where the
//! other side talks half of the time costs twice what it has to. This is a small energy-based
//! detector: it needs no model file, runs in milliseconds, and errs on the side of keeping
//! audio (padding around speech, short gaps are bridged, steady loud noise counts as speech).
//! Missing a word is worse than transcribing a little silence.
//!
//! The speech regions are cut out and joined with a short silence between them
//! ([`Compacted`]); [`Compacted::original_ms`] maps a time in the joined audio back onto the
//! original recording, so the transcript keeps the real timeline.

/// Length of one detection frame.
pub const FRAME_MS: u64 = 30;

/// Whisper's sample rate; sample counts below are at this rate.
const RATE: usize = 16_000;

/// Audio kept on both sides of speech.
const PAD_MS: u64 = 250;
/// Gaps between speech shorter than this are kept (pauses inside a sentence).
const MERGE_GAP_MS: u64 = 1_000;
/// Bursts shorter than this are clicks, not speech.
const MIN_SPEECH_MS: u64 = 90;
/// Silence inserted between two kept regions, so whisper still sees a pause.
const JOIN_SILENCE_MS: u64 = 300;

/// A frame is speech when it is this much louder than the quietest tenth of the recording...
const SPEECH_OVER_NOISE_DB: f32 = 9.0;
/// ...but never quieter than this (digital silence and hiss are not speech)...
const MIN_THRESHOLD_DBFS: f32 = -48.0;
/// ...and never louder than this (steady loud noise must not hide speech).
const MAX_THRESHOLD_DBFS: f32 = -38.0;

/// A run of frames `start..end` that holds speech.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameRegion {
    /// First frame.
    pub start: usize,
    /// One past the last frame.
    pub end: usize,
}

fn frames_for(ms: u64) -> usize {
    usize::try_from(ms.div_ceil(FRAME_MS)).unwrap_or(usize::MAX)
}

/// Speech regions from the RMS of consecutive 30 ms frames.
#[must_use]
pub fn regions_from_energies(rms: &[f32]) -> Vec<FrameRegion> {
    if rms.is_empty() {
        return Vec::new();
    }
    let db: Vec<f32> = rms.iter().map(|&r| 20.0 * (r + 1e-9).log10()).collect();
    let mut sorted = db.clone();
    sorted.sort_by(f32::total_cmp);
    let noise_floor = sorted[sorted.len() / 10];
    let threshold =
        (noise_floor + SPEECH_OVER_NOISE_DB).clamp(MIN_THRESHOLD_DBFS, MAX_THRESHOLD_DBFS);

    // Runs of loud frames, minus the clicks.
    let min_run = frames_for(MIN_SPEECH_MS);
    let mut runs: Vec<FrameRegion> = Vec::new();
    let mut run_start: Option<usize> = None;
    for (index, &level) in db
        .iter()
        .enumerate()
        .chain(std::iter::once((db.len(), &f32::MIN)))
    {
        let loud = level > threshold;
        match (loud, run_start) {
            (true, None) => run_start = Some(index),
            (false, Some(start)) => {
                run_start = None;
                if index - start >= min_run {
                    runs.push(FrameRegion { start, end: index });
                }
            }
            _ => {}
        }
    }

    // Pad, then bridge short gaps.
    let pad = frames_for(PAD_MS);
    let bridge = frames_for(MERGE_GAP_MS);
    let mut regions: Vec<FrameRegion> = Vec::new();
    for run in runs {
        let padded = FrameRegion {
            start: run.start.saturating_sub(pad),
            end: (run.end + pad).min(rms.len()),
        };
        match regions.last_mut() {
            Some(last) if padded.start <= last.end + bridge => last.end = last.end.max(padded.end),
            _ => regions.push(padded),
        }
    }
    regions
}

/// Total milliseconds of speech in the regions.
#[must_use]
pub fn speech_ms(regions: &[FrameRegion]) -> u64 {
    regions
        .iter()
        .map(|r| (r.end - r.start) as u64 * FRAME_MS)
        .sum()
}

/// RMS of consecutive 30 ms frames of 16 kHz audio.
#[must_use]
#[allow(clippy::cast_precision_loss)] // frame lengths are tiny
pub fn frame_energies(samples: &[f32]) -> Vec<f32> {
    let frame = RATE * usize::try_from(FRAME_MS).unwrap_or(30) / 1_000;
    samples
        .chunks(frame)
        .map(|chunk| (chunk.iter().map(|s| s * s).sum::<f32>() / chunk.len() as f32).sqrt())
        .collect()
}

#[derive(Debug, Clone, Copy)]
struct Entry {
    /// Where the region starts in the joined audio, in samples.
    joined_start: usize,
    /// Where it starts in the original, in samples.
    original_start: usize,
    len: usize,
}

/// The speech of a piece of audio, joined together, with the map back to the original times.
#[derive(Debug, Clone)]
pub struct Compacted {
    /// The joined 16 kHz mono samples.
    pub samples: Vec<f32>,
    map: Vec<Entry>,
}

impl Compacted {
    /// Cuts the regions out of `samples` and joins them. `None` when there is no speech.
    #[must_use]
    pub fn new(samples: &[f32], regions: &[FrameRegion]) -> Option<Self> {
        let frame = RATE * usize::try_from(FRAME_MS).unwrap_or(30) / 1_000;
        let join = RATE * usize::try_from(JOIN_SILENCE_MS).unwrap_or(300) / 1_000;
        let mut joined = Vec::new();
        let mut map = Vec::new();
        for region in regions {
            let start = (region.start * frame).min(samples.len());
            let end = (region.end * frame).min(samples.len());
            if end <= start {
                continue;
            }
            if !map.is_empty() {
                joined.resize(joined.len() + join, 0.0);
            }
            map.push(Entry {
                joined_start: joined.len(),
                original_start: start,
                len: end - start,
            });
            joined.extend_from_slice(&samples[start..end]);
        }
        (!map.is_empty()).then_some(Self {
            samples: joined,
            map,
        })
    }

    /// Duration of the joined audio in milliseconds.
    #[must_use]
    pub fn duration_ms(&self) -> u64 {
        (self.samples.len() * 1_000 / RATE) as u64
    }

    /// The original time of `joined_ms`. A time inside the silence that was inserted between
    /// two regions maps to the end of the region before it when `is_end` (a segment's end),
    /// otherwise to the start of the region after it (a segment's start).
    #[must_use]
    pub fn original_ms(&self, joined_ms: u64, is_end: bool) -> u64 {
        let at = usize::try_from(joined_ms)
            .unwrap_or(usize::MAX)
            .saturating_mul(RATE / 1_000);
        let index = self
            .map
            .partition_point(|entry| entry.joined_start <= at)
            .saturating_sub(1);
        let entry = self.map[index];
        let original = if at < entry.joined_start + entry.len {
            entry.original_start + (at - entry.joined_start)
        } else if is_end {
            entry.original_start + entry.len
        } else {
            self.map
                .get(index + 1)
                .map_or(entry.original_start + entry.len, |next| next.original_start)
        };
        (original / (RATE / 1_000)) as u64
    }
}

#[cfg(test)]
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::assert_is_empty
)]
mod tests {
    use super::*;

    /// 16 kHz audio: low hiss everywhere, a tone burst at each `(start_ms, end_ms)`.
    fn audio(total_ms: usize, bursts: &[(usize, usize)]) -> Vec<f32> {
        (0..total_ms * 16)
            .map(|i| {
                let ms = i / 16;
                let hiss = ((i * 7919 % 1000) as f32 / 1000.0 - 0.5) * 0.002;
                if bursts.iter().any(|&(a, b)| (a..b).contains(&ms)) {
                    hiss + (i as f32 * 0.35).sin() * 0.2
                } else {
                    hiss
                }
            })
            .collect()
    }

    fn regions_ms(samples: &[f32]) -> Vec<(u64, u64)> {
        regions_from_energies(&frame_energies(samples))
            .iter()
            .map(|r| (r.start as u64 * FRAME_MS, r.end as u64 * FRAME_MS))
            .collect()
    }

    #[test]
    fn digital_silence_and_hiss_are_not_speech() {
        assert!(regions_from_energies(&[]).is_empty());
        assert!(regions_ms(&vec![0.0; 16_000 * 20]).is_empty());
        assert!(regions_ms(&audio(20_000, &[])).is_empty());
    }

    #[test]
    fn speech_is_found_padded_and_the_silence_around_it_is_dropped() {
        let regions = regions_ms(&audio(30_000, &[(10_000, 12_000)]));
        assert_eq!(regions.len(), 1);
        let (start, end) = regions[0];
        assert!((9_600..=10_000).contains(&start), "{start}");
        assert!((12_000..=12_400).contains(&end), "{end}");
    }

    #[test]
    fn short_pauses_are_bridged_and_long_ones_split_the_speech() {
        // 0.5 s pause: one region. 6 s pause: two.
        let near = regions_ms(&audio(30_000, &[(5_000, 7_000), (7_500, 9_000)]));
        assert_eq!(near.len(), 1, "{near:?}");
        let far = regions_ms(&audio(30_000, &[(5_000, 7_000), (13_000, 15_000)]));
        assert_eq!(far.len(), 2, "{far:?}");
    }

    #[test]
    fn clicks_are_ignored() {
        assert!(regions_ms(&audio(10_000, &[(4_000, 4_040)])).is_empty());
    }

    #[test]
    fn steady_loud_noise_counts_as_speech_rather_than_hiding_it() {
        let loud: Vec<f32> = (0..16_000 * 10)
            .map(|i| (i as f32 * 0.1).sin() * 0.15)
            .collect();
        let regions = regions_ms(&loud);
        assert_eq!(regions.len(), 1);
        assert!(regions[0].1 - regions[0].0 >= 9_900);
    }

    #[test]
    fn the_joined_audio_maps_back_to_the_original_timeline() {
        let samples = audio(40_000, &[(5_000, 7_000), (25_000, 28_000)]);
        let regions = regions_from_energies(&frame_energies(&samples));
        assert_eq!(regions.len(), 2);
        let compact = Compacted::new(&samples, &regions).unwrap();
        // Far less audio than the original.
        assert!(compact.duration_ms() < 8_000, "{}", compact.duration_ms());

        // The first region starts at joined 0 and maps to its original start.
        let first_start = regions[0].start as u64 * FRAME_MS;
        assert_eq!(compact.original_ms(0, false), first_start);
        // A point 1 s into the first region's speech maps to the same point in the original.
        let into_first = 5_000 - first_start; // burst starts here, in joined time
        assert!(
            compact
                .original_ms(into_first + 1_000, false)
                .abs_diff(6_000)
                <= 30
        );
        // A point inside the second region, via the joined time of its burst.
        let second_joined_start = {
            let first_len = (regions[0].end - regions[0].start) as u64 * FRAME_MS;
            first_len + JOIN_SILENCE_MS
        };
        let second_start = regions[1].start as u64 * FRAME_MS;
        let burst_offset = 25_000 - second_start;
        assert!(
            compact
                .original_ms(second_joined_start + burst_offset + 500, false)
                .abs_diff(25_500)
                <= 30
        );
        // The inserted silence: an end stays in the first region, a start jumps to the next.
        let gap = (regions[0].end - regions[0].start) as u64 * FRAME_MS + JOIN_SILENCE_MS / 2;
        assert_eq!(
            compact.original_ms(gap, true),
            regions[0].end as u64 * FRAME_MS
        );
        assert_eq!(compact.original_ms(gap, false), second_start);
        // Past the end: clamps to the end of the last region.
        assert_eq!(
            compact.original_ms(compact.duration_ms() + 5_000, true),
            regions[1].end as u64 * FRAME_MS
        );
    }

    #[test]
    fn no_regions_means_nothing_to_compact() {
        assert!(Compacted::new(&[0.0; 1_000], &[]).is_none());
        assert_eq!(speech_ms(&[]), 0);
        assert_eq!(speech_ms(&[FrameRegion { start: 2, end: 12 }]), 300);
    }
}
