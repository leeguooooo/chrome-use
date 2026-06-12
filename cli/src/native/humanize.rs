//! Human-like input behaviour for stealth.
//!
//! When chrome-use drives a real Chrome over CDP, the input events it
//! dispatches are already `isTrusted` — but a click that teleports the cursor
//! straight to an element's exact centre, with no approach path and zero delay
//! between move/press/release, is a behavioural tell that advanced anti-bot
//! vendors (Akamai, PerimeterX, DataDome) look for.
//!
//! This module produces **human-like motion plans** — curved, eased cursor
//! trajectories and variable keystroke timing — as *pure data*. It performs no
//! I/O and knows nothing about CDP: callers turn the returned steps into
//! `Input.dispatchMouseEvent` / `dispatchKeyEvent` calls. Keeping the maths pure
//! makes the easing/jitter/detection logic unit-testable and deterministic
//! (every randomised value comes from a caller-supplied seed).
//!
//! Design (see brainstorm 2026-06-11):
//! - Three levels: [`HumanizeLevel::Off`] (instant, today's behaviour),
//!   `Fast` (a few cheap eased steps), `Human` (full curved trajectory + jitter).
//! - Baseline is `Off`; the daemon escalates a session to `Human` when
//!   [`detect_level`] spots a known anti-bot vendor on the page. `--humanize` /
//!   `AGENT_BROWSER_HUMANIZE` force a fixed level.
//! - Humanization only changes *how* the cursor reaches a target, never *which*
//!   element is hit: the landing jitter stays inside the caller-provided bounds.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

// ---- daemon-wide runtime state -------------------------------------------
//
// The pure motion maths above are stateless. The daemon drives one active page
// at a time, so we keep the *current* humanize level and last cursor position
// in process-global slots rather than threading them through every call site.
// (The adaptive detector flips the level per navigation; `dispatch_click` reads
// the level + cursor here, so no signature in the click/type call graph has to
// change.)

/// `AGENT_BROWSER_HUMANIZE` forces a fixed level, overriding the adaptive
/// detector. Parsed once.
fn env_override() -> Option<HumanizeLevel> {
    static OVERRIDE: OnceLock<Option<HumanizeLevel>> = OnceLock::new();
    *OVERRIDE.get_or_init(|| {
        std::env::var("AGENT_BROWSER_HUMANIZE")
            .ok()
            .and_then(|s| HumanizeLevel::parse(&s))
    })
}

fn session_level() -> &'static Mutex<HumanizeLevel> {
    static LEVEL: OnceLock<Mutex<HumanizeLevel>> = OnceLock::new();
    LEVEL.get_or_init(|| Mutex::new(HumanizeLevel::Off))
}

fn last_cursor_slot() -> &'static Mutex<(f64, f64)> {
    static CURSOR: OnceLock<Mutex<(f64, f64)>> = OnceLock::new();
    CURSOR.get_or_init(|| Mutex::new((0.0, 0.0)))
}

/// The level that should apply right now: the env override if set, else the
/// level the detector last chose for the active page.
pub fn active_level() -> HumanizeLevel {
    env_override().unwrap_or_else(|| *session_level().lock().unwrap())
}

/// Set by the adaptive detector after navigation. Ignored while an env override
/// is in force (so `--humanize` always wins).
pub fn set_detected_level(level: HumanizeLevel) {
    *session_level().lock().unwrap() = level;
}

/// Where the virtual cursor currently sits, so the next move starts from there
/// instead of teleporting.
pub fn last_cursor() -> (f64, f64) {
    *last_cursor_slot().lock().unwrap()
}

/// Record the cursor landing point after a move/click.
pub fn set_last_cursor(p: (f64, f64)) {
    *last_cursor_slot().lock().unwrap() = p;
}

/// A fresh seed per action so repeated clicks on the same point still vary,
/// without touching the wall clock or a global RNG (both would break replay).
pub fn next_seed() -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new(0x1234_5678);
    COUNTER
        .fetch_add(0x9E37_79B9_7F4A_7C15, Ordering::Relaxed)
        .rotate_left(17)
}

/// How human-like input motion should be.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum HumanizeLevel {
    /// Instant: a single move to the exact point, no delays. Original behaviour.
    #[default]
    Off,
    /// A few eased steps with small delays — cheap cover for ordinary sites.
    Fast,
    /// Full curved, decelerating trajectory with landing jitter and press
    /// dwell — for pages guarded by behavioural anti-bot systems.
    Human,
}

impl HumanizeLevel {
    /// Parse a user-supplied level (`--humanize` / `AGENT_BROWSER_HUMANIZE`).
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "off" | "none" | "instant" | "0" => Some(Self::Off),
            "fast" | "light" | "low" => Some(Self::Fast),
            "human" | "full" | "high" | "max" => Some(Self::Human),
            _ => None,
        }
    }

    fn is_off(self) -> bool {
        matches!(self, Self::Off)
    }
}

/// One step of a humanized cursor move: dispatch `mouseMoved` to (`x`, `y`),
/// then sleep for `delay` before the next step. The final step's point is where
/// the press/release should land.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MoveStep {
    pub x: f64,
    pub y: f64,
    pub delay: Duration,
}

/// Tiny deterministic PRNG (xorshift64*). Seeded by the caller so trajectories
/// are reproducible in tests; we avoid pulling in the `rand` crate and never
/// call a wall-clock/global RNG (which would also break workflow replay).
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        // Avoid the zero state, which xorshift cannot escape.
        Rng(seed ^ 0x9E37_79B9_7F4A_7C15)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in [0, 1).
    fn unit(&mut self) -> f64 {
        // Top 53 bits → f64 mantissa.
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Uniform in [-1, 1).
    fn signed(&mut self) -> f64 {
        self.unit() * 2.0 - 1.0
    }
}

/// Smootherstep ease (zero velocity at both ends) — used to bias the per-step
/// timing so the cursor accelerates away from the start and decelerates into
/// the target, the way a hand does.
fn ease(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

/// Cubic Bézier point at parameter `t`.
fn bezier(p0: (f64, f64), p1: (f64, f64), p2: (f64, f64), p3: (f64, f64), t: f64) -> (f64, f64) {
    let u = 1.0 - t;
    let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
    (
        a * p0.0 + b * p1.0 + c * p2.0 + d * p3.0,
        a * p0.1 + b * p1.1 + c * p2.1 + d * p3.1,
    )
}

/// Pick a landing point inside `bbox` (`x`, `y`, `width`, `height`). `Off`
/// returns the exact centre; `Fast`/`Human` jitter around the centre but stay
/// well inside the element so the click still lands on it.
pub fn landing_point(bbox: (f64, f64, f64, f64), level: HumanizeLevel, seed: u64) -> (f64, f64) {
    let (bx, by, bw, bh) = bbox;
    let cx = bx + bw / 2.0;
    let cy = by + bh / 2.0;
    if level.is_off() || bw <= 1.0 || bh <= 1.0 {
        return (cx, cy);
    }
    // Keep within the inner 60% so jitter never lands on a neighbouring element
    // or the element's padding/edge.
    let spread = match level {
        HumanizeLevel::Human => 0.30,
        _ => 0.15,
    };
    let mut rng = Rng::new(seed);
    (
        cx + rng.signed() * bw * spread,
        cy + rng.signed() * bh * spread,
    )
}

/// Build the cursor path from `from` to `to`. The last [`MoveStep`] is the
/// landing point. `Off` yields a single zero-delay step at `to` (today's
/// teleport), so callers can use one code path for every level.
pub fn move_path(
    from: (f64, f64),
    to: (f64, f64),
    level: HumanizeLevel,
    seed: u64,
) -> Vec<MoveStep> {
    if level.is_off() {
        return vec![MoveStep {
            x: to.0,
            y: to.1,
            delay: Duration::ZERO,
        }];
    }

    let dist = (to.0 - from.0).hypot(to.1 - from.1);
    if dist < 1.0 {
        return vec![MoveStep {
            x: to.0,
            y: to.1,
            delay: Duration::ZERO,
        }];
    }

    let (steps, total_ms, arc) = match level {
        HumanizeLevel::Fast => {
            let s = ((dist / 120.0).round() as usize).clamp(3, 6);
            (s, (dist * 0.35).clamp(40.0, 130.0), 0.06)
        }
        // Off handled above.
        _ => {
            let s = ((dist / 45.0).round() as usize).clamp(8, 24);
            (s, (dist * 0.9).clamp(140.0, 650.0), 0.16)
        }
    };

    let mut rng = Rng::new(seed);

    // Two control points along the line, pushed perpendicular to it to bow the
    // path into a gentle, slightly asymmetric arc.
    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
    let (nx, ny) = (-dy / dist, dx / dist); // unit normal
    let bow = dist * arc * rng.signed();
    let ctrl = |frac: f64, jitter: f64, rng: &mut Rng| {
        let base = (from.0 + dx * frac, from.1 + dy * frac);
        let off = bow * (1.0 + jitter * rng.signed());
        (base.0 + nx * off, base.1 + ny * off)
    };
    let p1 = ctrl(0.33, 0.4, &mut rng);
    let p2 = ctrl(0.66, 0.4, &mut rng);

    let mut out = Vec::with_capacity(steps);
    let mut prev_ease = 0.0;
    for i in 1..=steps {
        let t = i as f64 / steps as f64;
        // Ease maps wall-time progress so most points cluster near the ends
        // (slow start, slow finish, fast middle).
        let te = ease(t);
        let (x, y) = bezier(from, p1, p2, to, te);
        let frac = te - prev_ease;
        prev_ease = te;
        out.push(MoveStep {
            x,
            y,
            delay: Duration::from_micros((total_ms * frac * 1000.0).max(0.0) as u64),
        });
    }
    // Guarantee the final point is exactly the target.
    if let Some(last) = out.last_mut() {
        last.x = to.0;
        last.y = to.1;
    }
    out
}

/// Split a wheel scroll of (`total_dx`, `total_dy`) into eased segments. `Off`
/// returns a single instant segment (today's one-shot scroll); `Fast`/`Human`
/// break it into several accelerate-then-decelerate chunks with small,
/// jittered inter-segment delays, the way a trackpad/wheel flick actually
/// lands. The segment deltas always sum to the requested total.
pub fn scroll_segments(
    total_dx: f64,
    total_dy: f64,
    level: HumanizeLevel,
    seed: u64,
) -> Vec<(f64, f64, Duration)> {
    if level.is_off() {
        return vec![(total_dx, total_dy, Duration::ZERO)];
    }
    let (segs, base_ms) = match level {
        HumanizeLevel::Fast => (4usize, 18.0),
        _ => (9usize, 28.0),
    };
    let mut rng = Rng::new(seed);
    let mut out = Vec::with_capacity(segs);
    let mut prev = 0.0;
    for i in 1..=segs {
        let f = ease(i as f64 / segs as f64);
        let frac = f - prev;
        prev = f;
        let jitter = 1.0 + 0.3 * rng.signed();
        out.push((
            total_dx * frac,
            total_dy * frac,
            Duration::from_millis((base_ms * jitter).max(4.0) as u64),
        ));
    }
    out
}

/// Dwell between `mousePressed` and `mouseReleased` (a real click isn't
/// instantaneous). Zero for `Off`.
pub fn press_dwell(level: HumanizeLevel, seed: u64) -> Duration {
    match level {
        HumanizeLevel::Off => Duration::ZERO,
        HumanizeLevel::Fast => Duration::from_millis(20 + (seed % 30)),
        HumanizeLevel::Human => Duration::from_millis(50 + (seed % 90)),
    }
}

/// Per-character delays for typing `len` characters. `Off` is all-zero (use a
/// single `Input.insertText`); `Fast`/`Human` produce variable inter-keystroke
/// gaps with the occasional longer "think" pause, like a real typist.
pub fn keystroke_delays(len: usize, level: HumanizeLevel, seed: u64) -> Vec<Duration> {
    if level.is_off() || len == 0 {
        return vec![Duration::ZERO; len];
    }
    let (mean, jitter, pause_chance, pause_extra) = match level {
        HumanizeLevel::Fast => (25.0, 15.0, 0.0, 0.0),
        _ => (95.0, 55.0, 0.06, 220.0),
    };
    let mut rng = Rng::new(seed);
    (0..len)
        .map(|_| {
            let mut ms = (mean + rng.signed() * jitter).max(8.0);
            if pause_chance > 0.0 && rng.unit() < pause_chance {
                ms += rng.unit() * pause_extra;
            }
            Duration::from_millis(ms as u64)
        })
        .collect()
}

/// Page signals sampled after navigation, used to decide whether to escalate a
/// session to [`HumanizeLevel::Human`]. All strings are matched case-insensitively.
#[derive(Debug, Default, Clone)]
pub struct DetectSignals {
    /// Cookie names present on the document (e.g. `_abck`, `datadome`).
    pub cookie_names: Vec<String>,
    /// `src` of loaded scripts.
    pub script_urls: Vec<String>,
    /// Names of suspicious globals on `window` (e.g. `_px`, `bmak`).
    pub window_globals: Vec<String>,
}

/// Known behavioural anti-bot fingerprints: (substring, vendor). Matched against
/// cookie names, script URLs, and window globals.
const VENDOR_MARKERS: &[(&str, &str)] = &[
    ("_abck", "akamai"),
    ("bm_sz", "akamai"),
    ("ak_bmsc", "akamai"),
    ("bmak", "akamai"),
    ("_px", "perimeterx"),
    ("perimeterx", "perimeterx"),
    ("px-cloud", "perimeterx"),
    ("datadome", "datadome"),
    ("kpsdk", "kasada"),
    ("incap_ses", "imperva"),
    ("visid_incap", "imperva"),
    ("reese84", "imperva"),
    ("__cf_bm", "cloudflare-bot-mgmt"),
];

/// Decide the level for a page. Returns `Human` if any known anti-bot vendor is
/// present, otherwise `baseline`. Misses just stay at baseline and false hits
/// only cost a little latency, so matching is deliberately liberal.
pub fn detect_level(signals: &DetectSignals, baseline: HumanizeLevel) -> HumanizeLevel {
    let hay: Vec<String> = signals
        .cookie_names
        .iter()
        .chain(signals.script_urls.iter())
        .chain(signals.window_globals.iter())
        .map(|s| s.to_ascii_lowercase())
        .collect();
    let matched = VENDOR_MARKERS
        .iter()
        .any(|(marker, _)| hay.iter().any(|h| h.contains(marker)));
    if matched {
        HumanizeLevel::Human
    } else {
        baseline
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_accepts_known_levels_and_rejects_junk() {
        assert_eq!(HumanizeLevel::parse("off"), Some(HumanizeLevel::Off));
        assert_eq!(HumanizeLevel::parse(" FAST "), Some(HumanizeLevel::Fast));
        assert_eq!(HumanizeLevel::parse("Human"), Some(HumanizeLevel::Human));
        assert_eq!(HumanizeLevel::parse("max"), Some(HumanizeLevel::Human));
        assert_eq!(HumanizeLevel::parse("wat"), None);
    }

    #[test]
    fn off_level_teleports_in_one_step() {
        let path = move_path((0.0, 0.0), (100.0, 50.0), HumanizeLevel::Off, 1);
        assert_eq!(path.len(), 1);
        assert_eq!((path[0].x, path[0].y), (100.0, 50.0));
        assert_eq!(path[0].delay, Duration::ZERO);
    }

    #[test]
    fn humanized_path_is_multi_step_and_lands_exactly_on_target() {
        let to = (640.0, 480.0);
        let path = move_path((10.0, 10.0), to, HumanizeLevel::Human, 42);
        assert!(path.len() >= 8, "human path should have many steps");
        let last = path.last().unwrap();
        assert_eq!((last.x, last.y), to, "final point must equal the target");
        // Path must actually leave the straight line at some point (it's a curve).
        let straight = path.iter().all(|s| {
            let t = (s.x - 10.0) / (to.0 - 10.0);
            (s.y - (10.0 + t * (to.1 - 10.0))).abs() < 0.5
        });
        assert!(!straight, "human path should bow off the straight line");
    }

    #[test]
    fn fast_path_is_shorter_than_human() {
        let fast = move_path((0.0, 0.0), (500.0, 500.0), HumanizeLevel::Fast, 7);
        let human = move_path((0.0, 0.0), (500.0, 500.0), HumanizeLevel::Human, 7);
        assert!(fast.len() < human.len());
        assert!((3..=6).contains(&fast.len()));
    }

    #[test]
    fn move_path_is_deterministic_for_a_seed() {
        let a = move_path((1.0, 2.0), (300.0, 400.0), HumanizeLevel::Human, 99);
        let b = move_path((1.0, 2.0), (300.0, 400.0), HumanizeLevel::Human, 99);
        assert_eq!(a, b);
        let c = move_path((1.0, 2.0), (300.0, 400.0), HumanizeLevel::Human, 100);
        assert_ne!(a, c, "different seeds should differ");
    }

    #[test]
    fn landing_point_stays_inside_bounds_and_centres_when_off() {
        let bbox = (100.0, 100.0, 40.0, 20.0);
        assert_eq!(landing_point(bbox, HumanizeLevel::Off, 1), (120.0, 110.0));
        for seed in 0..200 {
            let (x, y) = landing_point(bbox, HumanizeLevel::Human, seed);
            assert!(x > 100.0 && x < 140.0, "x {x} escaped bbox");
            assert!(y > 100.0 && y < 120.0, "y {y} escaped bbox");
        }
    }

    #[test]
    fn keystroke_delays_zero_when_off_and_positive_otherwise() {
        assert!(keystroke_delays(5, HumanizeLevel::Off, 1)
            .iter()
            .all(|d| *d == Duration::ZERO));
        let human = keystroke_delays(20, HumanizeLevel::Human, 3);
        assert_eq!(human.len(), 20);
        assert!(human.iter().all(|d| *d >= Duration::from_millis(8)));
    }

    #[test]
    fn scroll_segments_sum_to_total_and_single_when_off() {
        let off = scroll_segments(0.0, 600.0, HumanizeLevel::Off, 1);
        assert_eq!(off.len(), 1);
        assert_eq!((off[0].0, off[0].1), (0.0, 600.0));
        assert_eq!(off[0].2, Duration::ZERO);

        let human = scroll_segments(0.0, 600.0, HumanizeLevel::Human, 5);
        assert!(human.len() >= 5);
        let total_dy: f64 = human.iter().map(|s| s.1).sum();
        assert!(
            (total_dy - 600.0).abs() < 1e-6,
            "segments must sum to total"
        );
        assert!(human.iter().all(|s| s.2 >= Duration::from_millis(4)));
    }

    #[test]
    fn detect_escalates_on_known_vendor_else_baseline() {
        let mut s = DetectSignals::default();
        assert_eq!(detect_level(&s, HumanizeLevel::Off), HumanizeLevel::Off);

        s.cookie_names = vec!["sessionid".into(), "_abck".into()];
        assert_eq!(detect_level(&s, HumanizeLevel::Off), HumanizeLevel::Human);

        let s2 = DetectSignals {
            script_urls: vec!["https://cdn.example.com/DataDome-tags.js".into()],
            ..Default::default()
        };
        assert_eq!(detect_level(&s2, HumanizeLevel::Off), HumanizeLevel::Human);

        let s3 = DetectSignals {
            window_globals: vec!["_pxAppId".into()],
            ..Default::default()
        };
        assert_eq!(detect_level(&s3, HumanizeLevel::Fast), HumanizeLevel::Human);

        // Unknown signals keep the baseline.
        let s4 = DetectSignals {
            cookie_names: vec!["cart".into(), "theme".into()],
            ..Default::default()
        };
        assert_eq!(detect_level(&s4, HumanizeLevel::Fast), HumanizeLevel::Fast);
    }
}
