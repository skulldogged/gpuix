//! Fades in text a streaming document gains, the way Waku and Zeron do.
//!
//! Only colour changes: the new characters are laid out at once and their runs
//! go from transparent to full over [`FADE`], so nothing moves while they
//! appear. Each painted piece (a paragraph, heading, cell or code line) is
//! tracked by its document-ordered selection sub-key.

use std::collections::HashMap;
use std::time::Duration;

use gpui::TextRun;
use web_time::Instant;

const FADE: Duration = Duration::from_millis(320);

#[derive(Default)]
pub struct Veil {
    /// The text each piece showed last frame.
    seen: HashMap<usize, String>,
    /// Where each piece's fading tails start, and when they started.
    fades: HashMap<usize, Vec<(usize, Instant)>>,
    /// False until the first frame, which shows whatever is already there.
    seeded: bool,
    /// Whether a fade is still running, so the element asks for another frame.
    pub animating: bool,
}

impl Veil {
    /// Call once a frame has painted every piece.
    pub fn finish_frame(&mut self) {
        self.seeded = true;
    }

    /// `runs` for `text` in piece `sub`, with text new since the last frame
    /// partly transparent.
    pub fn runs(&mut self, sub: usize, text: &str, runs: Vec<TextRun>) -> Vec<TextRun> {
        let now = Instant::now();
        let start = match self.seen.get(&sub) {
            // An appended tail fades in. Any other change, such as pieces
            // shifting as a block turns into a table, shows at once.
            Some(old) if text.len() > old.len() && text.starts_with(old.as_str()) => {
                Some(old.len())
            }
            Some(old) if old == text => None,
            Some(_) => {
                self.fades.remove(&sub);
                None
            }
            None if self.seeded => Some(0),
            None => None,
        };
        self.seen.insert(sub, text.to_string());
        let fades = self.fades.entry(sub).or_default();
        if let Some(start) = start {
            fades.push((start, now));
        }
        fades.retain(|(_, at)| now.duration_since(*at) < FADE);
        if fades.is_empty() {
            return runs;
        }
        self.animating = true;
        veil_runs(runs, fades, now)
    }
}

/// Split `runs` where fades start and scale each part's alpha by the progress
/// of the latest fade covering it.
fn veil_runs(runs: Vec<TextRun>, fades: &[(usize, Instant)], now: Instant) -> Vec<TextRun> {
    let alpha_at = |byte: usize| {
        fades
            .iter()
            .filter(|(start, _)| byte >= *start)
            .map(|(_, at)| progress(now.duration_since(*at)))
            .fold(1.0_f32, f32::min)
    };
    let mut cuts: Vec<usize> = fades.iter().map(|(start, _)| *start).collect();
    cuts.sort_unstable();
    let mut out = Vec::with_capacity(runs.len() + cuts.len());
    let mut offset = 0;
    for run in runs {
        let end = offset + run.len;
        let mut at = offset;
        for cut in cuts.iter().copied().filter(|c| *c > offset && *c < end) {
            out.push(faded(&run, cut - at, alpha_at(at)));
            at = cut;
        }
        out.push(faded(&run, end - at, alpha_at(at)));
        offset = end;
    }
    out
}

fn faded(run: &TextRun, len: usize, alpha: f32) -> TextRun {
    let mut run = run.clone();
    run.len = len;
    run.color.a *= alpha;
    run
}

/// Eases out, so new text is readable almost at once.
fn progress(elapsed: Duration) -> f32 {
    let t = (elapsed.as_secs_f32() / FADE.as_secs_f32()).clamp(0.0, 1.0);
    1.0 - (1.0 - t).powf(1.6)
}
