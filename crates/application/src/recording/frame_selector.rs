//! Which captured frames are worth keeping (ADR-009: only visibly changed frames are stored).
//!
//! The decision is a pure function of the previous *kept* frame and the new frame's
//! timestamp and perceptual hash; computing the hash is the adapter's job
//! ([`crate::ports::FrameHasher`]).
//!
//! A frame is kept when
//! 1. it is the first one of the session, or
//! 2. its hash is at least `change_threshold` bits (Hamming distance) away from the last
//!    **kept** frame, or
//! 3. nothing has been kept for `max_gap_ms` (the keep-alive guarantee: even a static slide
//!    shows up in the frame timeline now and then).
//!
//! Comparing against the last kept frame, not the previous sample, makes a slow change (a
//! page filling with text) accumulate until it crosses the threshold instead of being
//! dropped one small step at a time.

/// Default keep-alive: keep a frame at least this often even when nothing changed.
pub const DEFAULT_KEEP_ALIVE_MS: u64 = 60_000;

/// Tunables of the frame selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameSelectorConfig {
    /// Minimum Hamming distance to the last kept frame for a frame to count as changed.
    /// `0` keeps every frame.
    pub change_threshold: u32,
    /// Keep a frame when nothing was kept for this long; `None` disables the guarantee.
    pub max_gap_ms: Option<u64>,
}

impl FrameSelectorConfig {
    /// `change_threshold` with the default keep-alive.
    #[must_use]
    pub const fn with_threshold(change_threshold: u32) -> Self {
        Self {
            change_threshold,
            max_gap_ms: Some(DEFAULT_KEEP_ALIVE_MS),
        }
    }
}

/// The last frame that was kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeptFrame {
    /// Its offset from the session start, in milliseconds.
    pub timestamp_ms: u64,
    /// Its perceptual hash.
    pub hash: u64,
}

/// Why a frame was kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeepReason {
    /// First frame of the session.
    First,
    /// The picture changed enough.
    Changed,
    /// The keep-alive interval elapsed without a kept frame.
    KeepAlive,
}

/// The outcome for one frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameDecision {
    /// Store the frame.
    Keep(KeepReason),
    /// Drop it.
    Skip,
}

/// Number of differing bits between two perceptual hashes.
#[must_use]
pub const fn hamming_distance(a: u64, b: u64) -> u32 {
    (a ^ b).count_ones()
}

/// Decides about one frame given the last kept one.
#[must_use]
pub fn decide_frame(
    config: &FrameSelectorConfig,
    last_kept: Option<KeptFrame>,
    timestamp_ms: u64,
    hash: u64,
) -> FrameDecision {
    let Some(last) = last_kept else {
        return FrameDecision::Keep(KeepReason::First);
    };
    if hamming_distance(last.hash, hash) >= config.change_threshold {
        return FrameDecision::Keep(KeepReason::Changed);
    }
    match config.max_gap_ms {
        Some(gap) if timestamp_ms.saturating_sub(last.timestamp_ms) >= gap => {
            FrameDecision::Keep(KeepReason::KeepAlive)
        }
        _ => FrameDecision::Skip,
    }
}

/// [`decide_frame`] plus the bookkeeping of the last kept frame.
#[derive(Debug, Clone)]
pub struct FrameSelector {
    config: FrameSelectorConfig,
    last_kept: Option<KeptFrame>,
}

impl FrameSelector {
    /// A selector that has not kept anything yet.
    #[must_use]
    pub fn new(config: FrameSelectorConfig) -> Self {
        Self {
            config,
            last_kept: None,
        }
    }

    /// Decides about the next frame and remembers it when it is kept.
    pub fn decide(&mut self, timestamp_ms: u64, hash: u64) -> FrameDecision {
        let decision = decide_frame(&self.config, self.last_kept, timestamp_ms, hash);
        if matches!(decision, FrameDecision::Keep(_)) {
            self.last_kept = Some(KeptFrame { timestamp_ms, hash });
        }
        decision
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: u64 = 0;
    /// 16 bits away from `A`.
    const FAR: u64 = 0xFFFF;
    /// 3 bits away from `A`.
    const NEAR: u64 = 0b111;

    fn selector(threshold: u32, max_gap_ms: Option<u64>) -> FrameSelector {
        FrameSelector::new(FrameSelectorConfig {
            change_threshold: threshold,
            max_gap_ms,
        })
    }

    #[test]
    fn hamming_counts_differing_bits() {
        assert_eq!(hamming_distance(0, 0), 0);
        assert_eq!(hamming_distance(0b1010, 0b0110), 2);
        assert_eq!(hamming_distance(u64::MAX, 0), 64);
    }

    #[test]
    fn first_frame_is_always_kept() {
        let mut s = selector(8, None);
        assert_eq!(s.decide(0, A), FrameDecision::Keep(KeepReason::First));
    }

    #[test]
    fn identical_frames_are_dropped() {
        let mut s = selector(8, None);
        s.decide(0, A);
        for i in 1..20 {
            assert_eq!(s.decide(i * 3_000, A), FrameDecision::Skip, "frame {i}");
        }
    }

    #[test]
    fn change_at_or_above_the_threshold_is_kept() {
        let mut s = selector(16, None);
        s.decide(0, A);
        assert_eq!(
            s.decide(3_000, FAR),
            FrameDecision::Keep(KeepReason::Changed)
        );
    }

    #[test]
    fn change_below_the_threshold_is_dropped() {
        let mut s = selector(8, None);
        s.decide(0, A);
        assert_eq!(s.decide(3_000, NEAR), FrameDecision::Skip);
    }

    #[test]
    fn slow_drift_accumulates_against_the_last_kept_frame() {
        let mut s = selector(8, None);
        s.decide(0, A);
        // Each step moves 3 more bits away from the kept frame (compared to the last *kept*
        // one, not to the previous sample): 3, 6, then 9 >= 8.
        assert_eq!(s.decide(3_000, 0b111), FrameDecision::Skip);
        assert_eq!(s.decide(6_000, 0b111_111), FrameDecision::Skip);
        assert_eq!(
            s.decide(9_000, 0b111_111_111),
            FrameDecision::Keep(KeepReason::Changed)
        );
    }

    #[test]
    fn kept_frame_becomes_the_new_reference() {
        let mut s = selector(8, None);
        s.decide(0, A);
        s.decide(3_000, FAR); // kept, reference is now FAR
        assert_eq!(s.decide(6_000, FAR), FrameDecision::Skip);
        assert_eq!(
            s.decide(9_000, A),
            FrameDecision::Keep(KeepReason::Changed),
            "going back to the first picture is a change again"
        );
    }

    #[test]
    fn keep_alive_forces_a_frame_after_the_gap() {
        let mut s = selector(8, Some(60_000));
        s.decide(0, A);
        assert_eq!(s.decide(59_999, A), FrameDecision::Skip);
        assert_eq!(
            s.decide(60_000, A),
            FrameDecision::Keep(KeepReason::KeepAlive)
        );
        // The gap restarts from the kept frame.
        assert_eq!(s.decide(100_000, A), FrameDecision::Skip);
        assert_eq!(
            s.decide(120_000, A),
            FrameDecision::Keep(KeepReason::KeepAlive)
        );
    }

    #[test]
    fn keep_alive_can_be_disabled() {
        let mut s = selector(8, None);
        s.decide(0, A);
        assert_eq!(s.decide(10 * 3_600_000, A), FrameDecision::Skip);
    }

    #[test]
    fn zero_threshold_keeps_every_frame() {
        let mut s = selector(0, None);
        s.decide(0, A);
        assert_eq!(s.decide(3_000, A), FrameDecision::Keep(KeepReason::Changed));
    }

    #[test]
    fn a_timestamp_that_goes_backwards_does_not_trigger_keep_alive() {
        let mut s = selector(8, Some(1_000));
        s.decide(10_000, A);
        assert_eq!(s.decide(5_000, A), FrameDecision::Skip);
    }

    #[test]
    fn pure_function_matches_the_selector() {
        let config = FrameSelectorConfig::with_threshold(8);
        assert_eq!(
            decide_frame(&config, None, 0, A),
            FrameDecision::Keep(KeepReason::First)
        );
        let last = Some(KeptFrame {
            timestamp_ms: 0,
            hash: A,
        });
        assert_eq!(
            decide_frame(&config, last, 3_000, NEAR),
            FrameDecision::Skip
        );
        assert_eq!(
            decide_frame(&config, last, 3_000, FAR),
            FrameDecision::Keep(KeepReason::Changed)
        );
    }
}
