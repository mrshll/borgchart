//! The wake decision: what to do about one member, from plain-value
//! observations. It returns an [`Action`] and never performs one.
//!
//! - **No unread messages, no wake.**
//! - **Only the member's live start.** A start whose pane is no longer the
//!   one live in the member's place gets nothing.
//! - **Dedup by read progress, within a start.** An attempt covers the read
//!   progress it was made at. Another scan at the same progress sends
//!   nothing more; new messages or the member's reading make a new wake
//!   owed. A new start gets one fresh attempt for messages still unread,
//!   so a crash after a wake never strands them.
//! - **Intent, then outcome.** A wake comes with its intent, which the
//!   caller records before acting and resolves with the outcome after. An
//!   unresolved intent (a watcher that died mid-wake) isn't resent until
//!   its retry time, like an `uncertain` outcome.
//! - **Uncertain means no fallback.** An `uncertain` outcome may have
//!   delivered, so it's retried with the same method, a bounded number of
//!   times, then needs a person. Only `rejected` or `unavailable` moves on
//!   to the next method.
//! - **Repeats without progress stop.** A member woken repeatedly with
//!   nothing read needs a person, and gets no more wakes until it reads.
//! - **Missing evidence isn't evidence.** Unknown evidence defers.
//! - **Methods per vendor and version** come from [`super::methods`]. An
//!   unknown version needs a person.

use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use super::methods::{Method, WakeRules};
use crate::team::Vendor;

/// Evidence the caller may not have been able to read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Seen<T> {
    Known(T),
    Unknown,
}

/// One start of a member.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Start {
    pub id: String,
    /// The identity of the pane the start runs in.
    pub pane: String,
}

/// Positions in the member's inbox, in one order: the newest message's,
/// and the member's read cursor (the last one it has read).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadProgress {
    pub newest: u64,
    pub read: u64,
}

impl ReadProgress {
    fn unread(&self) -> bool {
        self.newest > self.read
    }
}

/// Evidence of a wait the member armed for itself.
///
/// The caller reports `Armed` only for a live wait of this member, start
/// and inbox. A wait under another member, or a dead one, is `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wait {
    Armed,
    None,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    /// Idle, with an empty input box.
    Idle,
    Working,
    /// A prompt, approval or dialog.
    Blocked,
    /// Idle, with text in the input box.
    Draft,
    CopyMode,
    Unknown,
}

/// How an attempt ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    /// Delivered.
    Dispatched,
    /// Refused, so nothing was delivered.
    Rejected,
    /// The method couldn't be used, so nothing was delivered.
    Unavailable,
    /// It may or may not have been delivered (a timeout, say).
    Uncertain,
}

/// A wake attempt: its intent, and its outcome once known.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attempt {
    pub start: Start,
    /// The read progress the attempt covers.
    pub progress: ReadProgress,
    pub method: Method,
    #[serde(with = "super::utc")]
    pub at: SystemTime,
    /// Tries of `method` at this progress, counting this one.
    pub tries: u32,
    /// Wakes in this start since the read cursor last moved, counting this one.
    pub wakes: u32,
    #[serde(default)]
    pub outcome: Option<Outcome>,
}

impl Attempt {
    /// The same attempt, resolved.
    pub fn resolved(&self, outcome: Outcome) -> Attempt {
        Attempt {
            outcome: Some(outcome),
            ..self.clone()
        }
    }

    /// An unresolved intent younger than `retry_after`: its side effect may
    /// still be under way.
    pub fn in_flight(&self, now: SystemTime, retry_after: Duration) -> bool {
        self.outcome.is_none() && now.duration_since(self.at).unwrap_or_default() < retry_after
    }
}

/// What the caller observed about one member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    /// The member's current start, as recorded.
    pub start: Start,
    /// The pane live in the member's place now; `Known(None)` if none is.
    pub live_pane: Seen<Option<String>>,
    pub progress: Seen<ReadProgress>,
    pub wait: Wait,
    pub vendor: Vendor,
    /// The CLI's version; `None` if it couldn't be read.
    pub version: Option<String>,
    pub screen: Screen,
    /// The member's last attempt, from any start.
    pub last: Option<Attempt>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// How long an `uncertain` outcome or an unresolved intent waits before
    /// a retry.
    pub retry_after: Duration,
    /// Tries of one method at one read progress before it needs a person.
    pub max_tries: u32,
    /// Wakes with nothing read before the member needs a person.
    pub max_wakes: u32,
}

impl Default for Policy {
    fn default() -> Self {
        Policy {
            retry_after: Duration::from_secs(120),
            max_tries: 3,
            max_wakes: 3,
        }
    }
}

/// Why nothing is owed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quiet {
    NoUnread,
    /// The recorded start's pane is not the one live.
    StartNotLive,
    WaitArmed,
    /// Already woken at this read progress.
    Woken,
    /// An `uncertain` or unresolved attempt hasn't reached its retry time.
    RetryNotDue,
}

/// Why the decision waits for better evidence or a better moment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Defer {
    LivePaneUnknown,
    ProgressUnknown,
    WaitUnknown,
    ScreenUnknown,
    /// Mid-turn: a typed reminder waits for idle.
    Working,
}

/// Why a person is needed. It feeds the alert book ([`super::alerts`]);
/// nothing else acts on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NeedsPerson {
    /// No wake rule for this vendor and version.
    UnknownVersion,
    /// Woken this many times in this start with nothing read.
    Repeats { wakes: u32 },
    /// `uncertain` (or unresolved) this many times at one read progress.
    RetriesExhausted { method: Method, tries: u32 },
    /// The last method was refused or unavailable, and there's no other.
    Unreachable { method: Method, outcome: Outcome },
    /// The rules no longer list the method of an unresolved attempt, so a
    /// fallback might follow an uncertain delivery.
    MethodWithdrawn(Method),
    /// A typed reminder is owed, but the screen shows this.
    Screen(Screen),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Nothing(Quiet),
    Defer(Defer),
    /// Record `intent`, re-check that `target` is still live, use `method`,
    /// then record `intent.resolved(outcome)`.
    Wake {
        method: Method,
        target: Start,
        intent: Attempt,
    },
    NeedsPerson(NeedsPerson),
}

pub fn decide(obs: &Observation, rules: &WakeRules, policy: &Policy, now: SystemTime) -> Action {
    match &obs.live_pane {
        Seen::Unknown => return Action::Defer(Defer::LivePaneUnknown),
        Seen::Known(pane) if pane.as_deref() != Some(obs.start.pane.as_str()) => {
            return Action::Nothing(Quiet::StartNotLive);
        }
        Seen::Known(_) => {}
    }
    let progress = match obs.progress {
        Seen::Known(p) => p,
        Seen::Unknown => return Action::Defer(Defer::ProgressUnknown),
    };
    if !progress.unread() {
        return Action::Nothing(Quiet::NoUnread);
    }
    let Some(entry) = rules.find(obs.vendor, obs.version.as_deref()) else {
        return Action::NeedsPerson(NeedsPerson::UnknownVersion);
    };
    if entry.self_wake {
        match obs.wait {
            Wait::Armed => return Action::Nothing(Quiet::WaitArmed),
            Wait::Unknown => return Action::Defer(Defer::WaitUnknown),
            Wait::None => {}
        }
    }
    let last = obs.last.as_ref().filter(|a| a.start == obs.start);
    let (index, tries, wakes) = match last {
        None => (0, 1, 1),
        Some(a) if a.progress == progress => match a.outcome {
            Some(Outcome::Dispatched) => return Action::Nothing(Quiet::Woken),
            None | Some(Outcome::Uncertain) => {
                if now.duration_since(a.at).unwrap_or_default() < policy.retry_after {
                    return Action::Nothing(Quiet::RetryNotDue);
                }
                if a.tries >= policy.max_tries {
                    return Action::NeedsPerson(NeedsPerson::RetriesExhausted {
                        method: a.method,
                        tries: a.tries,
                    });
                }
                let Some(i) = entry.methods.iter().position(|m| *m == a.method) else {
                    return Action::NeedsPerson(NeedsPerson::MethodWithdrawn(a.method));
                };
                (i, a.tries + 1, a.wakes)
            }
            Some(outcome @ (Outcome::Rejected | Outcome::Unavailable)) => {
                let next = entry
                    .methods
                    .iter()
                    .position(|m| *m == a.method)
                    .map(|i| i + 1)
                    .filter(|i| *i < entry.methods.len());
                let Some(i) = next else {
                    return Action::NeedsPerson(NeedsPerson::Unreachable {
                        method: a.method,
                        outcome,
                    });
                };
                (i, 1, a.wakes)
            }
        },
        Some(a) if progress.read == a.progress.read => {
            if a.wakes >= policy.max_wakes {
                return Action::NeedsPerson(NeedsPerson::Repeats { wakes: a.wakes });
            }
            (0, 1, a.wakes + 1)
        }
        Some(_) => (0, 1, 1),
    };
    let method = entry.methods[index];
    if method == Method::Typed {
        match obs.screen {
            Screen::Idle => {}
            Screen::Working => return Action::Defer(Defer::Working),
            Screen::Unknown => return Action::Defer(Defer::ScreenUnknown),
            s @ (Screen::Blocked | Screen::Draft | Screen::CopyMode) => {
                return Action::NeedsPerson(NeedsPerson::Screen(s));
            }
        }
    }
    Action::Wake {
        method,
        target: obs.start.clone(),
        intent: Attempt {
            start: obs.start.clone(),
            progress,
            method,
            at: now,
            tries,
            wakes,
            outcome: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::watcher::methods;

    fn t(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000 + secs)
    }

    fn start(id: &str) -> Start {
        Start {
            id: id.to_string(),
            pane: format!("pane-{id}"),
        }
    }

    fn progress(newest: u64, read: u64) -> Seen<ReadProgress> {
        Seen::Known(ReadProgress { newest, read })
    }

    /// A Codex 0.165.0 member in start `s1`, idle, with message 2 unread.
    fn codex() -> Observation {
        Observation {
            start: start("s1"),
            live_pane: Seen::Known(Some("pane-s1".to_string())),
            progress: progress(2, 1),
            wait: Wait::None,
            vendor: Vendor::Codex,
            version: Some("0.165.0".to_string()),
            screen: Screen::Idle,
            last: None,
        }
    }

    /// A Claude Code 2.1.x member in start `s1`, idle, no wait, message 2 unread.
    fn claude() -> Observation {
        Observation {
            vendor: Vendor::Claude,
            version: Some("2.1.318".to_string()),
            ..codex()
        }
    }

    fn decide_at(obs: &Observation, now: SystemTime) -> Action {
        decide(obs, &methods::bundled(), &Policy::default(), now)
    }

    /// The intent of a wake, or a panic naming what came instead.
    fn intent(action: Action) -> Attempt {
        match action {
            Action::Wake { intent, .. } => intent,
            other => panic!("expected a wake, got {other:?}"),
        }
    }

    fn method(action: &Action) -> Option<Method> {
        match action {
            Action::Wake { method, .. } => Some(*method),
            _ => None,
        }
    }

    /// One scan: decide, and if it wakes, resolve the intent with `outcome`.
    fn scan(obs: &mut Observation, now: SystemTime, outcome: Outcome) -> Action {
        let action = decide_at(obs, now);
        if let Action::Wake { intent, .. } = &action {
            obs.last = Some(intent.resolved(outcome));
        }
        action
    }

    // Catches: waking a member with nothing to read (a wasted turn).
    #[test]
    fn no_unread_messages_no_wake() {
        let mut obs = codex();
        obs.progress = progress(3, 3);
        assert_eq!(decide_at(&obs, t(0)), Action::Nothing(Quiet::NoUnread));
    }

    // Catches: dedup by time or by "woken once": the second scan of one
    // start resending, or a new message after the wake never waking.
    #[test]
    fn two_scans_in_one_start_send_one_wake() {
        let mut obs = codex();
        assert!(method(&scan(&mut obs, t(0), Outcome::Dispatched)).is_some());
        assert_eq!(
            scan(&mut obs, t(1000), Outcome::Dispatched),
            Action::Nothing(Quiet::Woken)
        );
        obs.progress = progress(3, 1);
        assert!(method(&scan(&mut obs, t(1001), Outcome::Dispatched)).is_some());
    }

    // Catches: a failed wake (refused everywhere, or retries used up) that
    // stops all wakes for good, and a dispatched one that suppresses the next
    // start's wake for still-unread messages.
    #[test]
    fn unread_messages_survive_a_failed_wake_and_a_new_start() {
        let mut obs = codex();
        assert_eq!(
            method(&scan(&mut obs, t(0), Outcome::Rejected)),
            Some(Method::Queue)
        );
        assert_eq!(
            method(&scan(&mut obs, t(1), Outcome::Rejected)),
            Some(Method::Typed)
        );
        assert!(matches!(
            decide_at(&obs, t(2)),
            Action::NeedsPerson(NeedsPerson::Unreachable { .. })
        ));
        // A new message is still owed a wake.
        obs.progress = progress(3, 1);
        assert_eq!(
            method(&scan(&mut obs, t(3), Outcome::Dispatched)),
            Some(Method::Queue)
        );
        // A new start, same unread messages: one fresh attempt, to the new pane.
        obs.start = start("s2");
        obs.live_pane = Seen::Known(Some("pane-s2".to_string()));
        match decide_at(&obs, t(4)) {
            Action::Wake { target, intent, .. } => {
                assert_eq!(target, start("s2"));
                assert_eq!((intent.tries, intent.wakes), (1, 1));
            }
            other => panic!("expected a wake, got {other:?}"),
        }
    }

    // Catches: a watcher that died between intent and outcome resending at
    // once on restart, and one that never resends.
    #[test]
    fn unresolved_intent_after_restart_does_not_resend() {
        let mut obs = codex();
        let first = intent(decide_at(&obs, t(0)));
        obs.last = Some(first.clone()); // recorded; the watcher died before the outcome
        assert_eq!(decide_at(&obs, t(10)), Action::Nothing(Quiet::RetryNotDue));
        let retry = intent(decide_at(&obs, t(200)));
        assert_eq!((retry.method, retry.tries), (first.method, 2));
    }

    // Catches: falling back to typing after a queue call that may have
    // delivered (a double wake, or text typed into a turn).
    #[test]
    fn uncertain_never_types() {
        let mut obs = codex();
        let mut now = 0;
        loop {
            assert!(now < 3600, "retries never ran out");
            match scan(&mut obs, t(now), Outcome::Uncertain) {
                Action::Wake { method, .. } => assert_eq!(method, Method::Queue),
                Action::Nothing(Quiet::RetryNotDue) => {}
                Action::NeedsPerson(NeedsPerson::RetriesExhausted { method, tries }) => {
                    assert_eq!((method, tries), (Method::Queue, 3));
                    break;
                }
                other => panic!("unexpected {other:?}"),
            }
            now += 60;
        }
    }

    // Catches: waking a start that's no longer the live one (an old or
    // replaced pane), or treating an unreadable pane as live.
    #[test]
    fn a_stale_or_replaced_start_gets_nothing() {
        let mut obs = codex();
        obs.live_pane = Seen::Known(Some("pane-s2".to_string()));
        assert_eq!(decide_at(&obs, t(0)), Action::Nothing(Quiet::StartNotLive));
        obs.live_pane = Seen::Known(None);
        assert_eq!(decide_at(&obs, t(0)), Action::Nothing(Quiet::StartNotLive));
        obs.live_pane = Seen::Unknown;
        assert_eq!(decide_at(&obs, t(0)), Action::Defer(Defer::LivePaneUnknown));
    }

    // Catches: a Claude member left asleep because some wait was seen, or
    // woken while its own wait is armed. The caller reports a wait under
    // another member, or a dead one, as `Wait::None` (see [`Wait`]); this
    // checks the decision acts on that report.
    #[test]
    fn only_an_armed_wait_of_the_member_counts() {
        let mut obs = claude();
        obs.wait = Wait::Armed;
        assert_eq!(decide_at(&obs, t(0)), Action::Nothing(Quiet::WaitArmed));
        obs.wait = Wait::None; // another member's, or a dead one
        assert_eq!(method(&decide_at(&obs, t(0))), Some(Method::Typed));
    }

    // Catches: guessing a wake for a CLI nobody tested, including a prefix
    // match too loose (2.10 for 2.1.*) or too strict (0.165.1 for 0.165.0).
    #[test]
    fn an_unknown_version_needs_a_person() {
        for (vendor, version) in [
            (Vendor::Codex, Some("0.165.1")),
            (Vendor::Codex, None),
            (Vendor::Claude, Some("2.10.0")),
            (Vendor::Claude, Some("2.1")),
        ] {
            let mut obs = codex();
            obs.vendor = vendor;
            obs.version = version.map(str::to_string);
            assert_eq!(
                decide_at(&obs, t(0)),
                Action::NeedsPerson(NeedsPerson::UnknownVersion),
                "{vendor:?} {version:?}"
            );
        }
    }

    // Catches: a retry loop on a member that is woken and never reads.
    #[test]
    fn repeats_without_progress_stop() {
        let mut obs = codex();
        for newest in 2..=4 {
            obs.progress = progress(newest, 1);
            assert!(method(&scan(&mut obs, t(newest), Outcome::Dispatched)).is_some());
        }
        obs.progress = progress(5, 1);
        assert_eq!(
            decide_at(&obs, t(10)),
            Action::NeedsPerson(NeedsPerson::Repeats { wakes: 3 })
        );
        // Reading resumes the wakes.
        obs.progress = progress(6, 5);
        assert_eq!(intent(decide_at(&obs, t(11))).wakes, 1);
    }

    // Catches: unknown evidence read as "no wait" or "an empty input box".
    #[test]
    fn unknown_evidence_defers() {
        let mut obs = claude();
        obs.wait = Wait::Unknown;
        assert_eq!(decide_at(&obs, t(0)), Action::Defer(Defer::WaitUnknown));
        obs.wait = Wait::None;
        obs.screen = Screen::Unknown;
        assert_eq!(decide_at(&obs, t(0)), Action::Defer(Defer::ScreenUnknown));
        obs.progress = Seen::Unknown;
        assert_eq!(decide_at(&obs, t(0)), Action::Defer(Defer::ProgressUnknown));
    }

    // Catches: a typed reminder into a draft, a prompt, copy mode or a turn.
    #[test]
    fn typed_needs_an_idle_empty_screen() {
        let mut obs = claude();
        obs.screen = Screen::Working;
        assert_eq!(decide_at(&obs, t(0)), Action::Defer(Defer::Working));
        for s in [Screen::Draft, Screen::Blocked, Screen::CopyMode] {
            obs.screen = s;
            assert_eq!(
                decide_at(&obs, t(0)),
                Action::NeedsPerson(NeedsPerson::Screen(s))
            );
        }
    }

    // Catches: a Codex wake that types first, or is held back by a busy
    // screen or a draft, which a queued message doesn't touch.
    #[test]
    fn codex_queues_first_whatever_the_screen() {
        let mut obs = codex();
        obs.screen = Screen::Draft;
        obs.wait = Wait::Unknown;
        assert_eq!(method(&decide_at(&obs, t(0))), Some(Method::Queue));
    }
}
