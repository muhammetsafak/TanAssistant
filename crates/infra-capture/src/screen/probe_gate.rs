//! Single-flight cache for the portal availability probe.
//!
//! Asking the portal takes a D-Bus round trip (and may have to start the portal service, which
//! takes a second or two after login), and a portal that hangs would hang every asker. The gate
//! makes sure that
//!
//! * a positive answer is remembered for good and a negative one for a while,
//! * at most one probe is in flight: a caller that arrives while one runs **waits for its
//!   result** (bounded by the probe timeout) instead of starting another or guessing,
//! * a probe that hangs past the timeout is not retried while it is still stuck (callers get
//!   `false`, "not known to work", at once), and when it finally answers, its answer is stored
//!   like any other.
//!
//! The gate does not run the probe itself: [`ProbeGate::claim`] tells the caller whether to run
//! it ([`Claim::Run`], exactly one caller at a time), the caller starts it wherever it likes and
//! reports with [`ProbeGate::finish`], and waits with [`ProbeGate::wait`].
//!
//! No dependency on the portal crates, so it is unit tested on any OS; only compiled on Linux
//! (and in tests).
// Only the Linux portal adapter calls these; on other OSes just the tests reach them.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// What [`ProbeGate::claim`] decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Claim {
    /// The answer is known (or a probe in flight did not answer in time: `false`).
    Answer(bool),
    /// The caller must run the probe now, report with [`ProbeGate::finish`] and may then
    /// [`ProbeGate::wait`] for it.
    Run,
}

#[derive(Debug)]
struct State {
    /// The last answer and when it was recorded.
    result: Option<(Instant, bool)>,
    /// When the probe in flight started; `None` when no probe is running.
    started: Option<Instant>,
}

impl State {
    /// The remembered answer if it is still good: positive forever, negative for `ttl`.
    fn answer(&self, negative_ttl: Duration) -> Option<bool> {
        match self.result {
            Some((_, true)) => Some(true),
            Some((recorded_at, false)) if recorded_at.elapsed() < negative_ttl => Some(false),
            _ => None,
        }
    }
}

/// See the module docs.
#[derive(Debug)]
pub(super) struct ProbeGate {
    state: Mutex<State>,
    changed: Condvar,
}

impl ProbeGate {
    /// A gate that knows nothing yet.
    pub(super) const fn new() -> Self {
        Self {
            state: Mutex::new(State {
                result: None,
                started: None,
            }),
            changed: Condvar::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Returns the known answer, or [`Claim::Run`] to the one caller that has to probe. While a
    /// probe is in flight the caller blocks until it finishes, for at most `timeout` counted
    /// from the probe's start; if it is still running then, the answer is `false` and no second
    /// probe is started.
    pub(super) fn claim(&self, negative_ttl: Duration, timeout: Duration) -> Claim {
        let mut state = self.lock();
        loop {
            if let Some(answer) = state.answer(negative_ttl) {
                return Claim::Answer(answer);
            }
            let Some(started) = state.started else {
                state.started = Some(Instant::now());
                return Claim::Run;
            };
            let Some(left) = remaining(started, timeout) else {
                return Claim::Answer(false);
            };
            state = self.wait_changed(state, left);
        }
    }

    /// Blocks until the probe the caller started has finished (at most `timeout` from its
    /// start) and returns its answer; `false` when it is still running by then.
    pub(super) fn wait(&self, negative_ttl: Duration, timeout: Duration) -> bool {
        let mut state = self.lock();
        loop {
            if let Some(answer) = state.answer(negative_ttl) {
                return answer;
            }
            let Some(left) = state
                .started
                .and_then(|started| remaining(started, timeout))
            else {
                return false;
            };
            state = self.wait_changed(state, left);
        }
    }

    /// Records the answer of the probe and wakes everyone waiting for it. Also valid long
    /// after the waiters gave up: the answer is kept for the next asker.
    pub(super) fn finish(&self, usable: bool) {
        let mut state = self.lock();
        state.result = Some((Instant::now(), usable));
        state.started = None;
        drop(state);
        self.changed.notify_all();
    }

    fn wait_changed<'a>(
        &self,
        state: MutexGuard<'a, State>,
        limit: Duration,
    ) -> MutexGuard<'a, State> {
        self.changed
            .wait_timeout(state, limit)
            .unwrap_or_else(PoisonError::into_inner)
            .0
    }
}

/// Time left of `timeout` for a probe that started at `started`; `None` when it is used up.
fn remaining(started: Instant, timeout: Duration) -> Option<Duration> {
    timeout
        .checked_sub(started.elapsed())
        .filter(|left| !left.is_zero())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::thread;

    use super::*;

    const TTL: Duration = Duration::from_secs(30);
    const LONG: Duration = Duration::from_secs(10);

    #[test]
    fn the_first_caller_runs_the_probe_and_a_positive_answer_is_kept() {
        let gate = ProbeGate::new();
        assert_eq!(gate.claim(TTL, LONG), Claim::Run);
        gate.finish(true);
        assert!(gate.wait(TTL, LONG));
        // Positive answers do not expire, whatever the negative ttl.
        assert_eq!(gate.claim(Duration::ZERO, LONG), Claim::Answer(true));
    }

    #[test]
    fn a_negative_answer_expires_and_is_then_probed_again() {
        let gate = ProbeGate::new();
        assert_eq!(gate.claim(TTL, LONG), Claim::Run);
        gate.finish(false);
        assert_eq!(gate.claim(TTL, LONG), Claim::Answer(false));
        assert_eq!(gate.claim(Duration::ZERO, LONG), Claim::Run);
    }

    #[test]
    fn a_caller_that_arrives_during_a_probe_gets_its_real_answer() {
        let gate = Arc::new(ProbeGate::new());
        assert_eq!(gate.claim(TTL, LONG), Claim::Run);

        let waiter = {
            let gate = gate.clone();
            thread::spawn(move || gate.claim(TTL, LONG))
        };
        thread::sleep(Duration::from_millis(100));
        assert!(
            !waiter.is_finished(),
            "the second caller must wait, not guess"
        );
        gate.finish(true);
        assert_eq!(waiter.join().unwrap(), Claim::Answer(true));
    }

    #[test]
    fn only_one_probe_runs_at_a_time() {
        let gate = Arc::new(ProbeGate::new());
        let callers: Vec<_> = (0..8)
            .map(|_| {
                let gate = gate.clone();
                thread::spawn(move || match gate.claim(TTL, LONG) {
                    Claim::Run => {
                        thread::sleep(Duration::from_millis(50));
                        gate.finish(true);
                        (true, true)
                    }
                    Claim::Answer(answer) => (false, answer),
                })
            })
            .collect();
        let outcomes: Vec<_> = callers.into_iter().map(|c| c.join().unwrap()).collect();
        assert_eq!(outcomes.iter().filter(|(ran, _)| *ran).count(), 1);
        assert!(outcomes.iter().all(|(_, answer)| *answer), "{outcomes:?}");
    }

    #[test]
    fn a_probe_that_hangs_is_not_retried_and_its_late_answer_is_kept() {
        let gate = ProbeGate::new();
        let timeout = Duration::from_millis(60);
        assert_eq!(gate.claim(TTL, timeout), Claim::Run);

        // Nobody finishes it: asking again waits out the timeout, then says false, and does
        // not hand out a second `Run` while the first probe is still stuck.
        let asked_at = Instant::now();
        assert_eq!(gate.claim(TTL, timeout), Claim::Answer(false));
        assert!(asked_at.elapsed() >= Duration::from_millis(30));
        assert_eq!(gate.claim(TTL, timeout), Claim::Answer(false));
        assert!(
            !gate.wait(TTL, timeout),
            "the starter gives up the same way"
        );

        // The stuck probe finally answers: that answer is not overwritten by the timeouts.
        gate.finish(true);
        assert_eq!(gate.claim(TTL, timeout), Claim::Answer(true));
    }

    #[test]
    fn a_failed_start_can_be_reported_as_a_negative_answer() {
        let gate = ProbeGate::new();
        assert_eq!(gate.claim(TTL, LONG), Claim::Run);
        gate.finish(false); // the probe thread could not be spawned
        assert!(!gate.wait(TTL, LONG));
        assert_eq!(gate.claim(TTL, LONG), Claim::Answer(false));
    }
}
