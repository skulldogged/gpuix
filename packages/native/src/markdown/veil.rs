//! Fades in what a streaming document gains. Nothing moves while it appears.
//!
//! A block that arrives (a paragraph, heading, list, list item, quote, code
//! block, table, rule or image) fades as a whole, chrome included, from
//! transparent to full over [`BLOCK_FADE`], as T3 Code's replies do. Blocks
//! are tracked by their path of sibling indices; one arriving inside another
//! arriving block shares that block's fade.
//!
//! Text that later extends a block already shown fades by colour alone over
//! [`FADE`], the way Waku and Zeron do: the new characters are laid out at
//! once and only their runs change. Each painted piece (a paragraph, heading,
//! cell or code line) is tracked by its document-ordered selection sub-key.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use gpui::TextRun;
use web_time::Instant;

const FADE: Duration = Duration::from_millis(320);
/// T3 Code's `transition: opacity 600ms ease-out`.
const BLOCK_FADE: Duration = Duration::from_millis(600);

#[derive(Default)]
pub struct Veil {
    /// The text each piece showed last frame.
    seen: HashMap<usize, String>,
    /// Where each piece's fading tails start, and when they started.
    fades: HashMap<usize, Vec<(usize, Instant)>>,
    /// Every block shown so far, by path. Never pruned, so a block that a
    /// passing parse drops and brings back doesn't fade again.
    blocks: HashSet<Vec<usize>>,
    /// When each block still fading in arrived.
    block_fades: HashMap<Vec<usize>, Instant>,
    /// False until the first frame, which shows whatever is already there.
    seeded: bool,
    /// Whether new blocks and text start fading. Off once the document stops
    /// streaming, while the fades already running finish.
    pub live: bool,
    /// Whether a fade is still running, so the element asks for another frame.
    pub animating: bool,
}

impl Veil {
    /// Call once a frame has painted every piece.
    pub fn finish_frame(&mut self) {
        self.seeded = true;
    }

    /// The opacity of the block at `path` while it fades in, and whether it
    /// arrived this frame. One arriving inside another (`covered`) shows
    /// through that block's fade instead of adding its own.
    pub fn block(&mut self, path: &[usize], covered: bool) -> (Option<f32>, bool) {
        let now = Instant::now();
        let arrived = !self.blocks.contains(path);
        if arrived {
            self.blocks.insert(path.to_vec());
            if self.seeded && self.live && !covered {
                self.block_fades.insert(path.to_vec(), now);
            }
        }
        let Some(at) = self.block_fades.get(path).copied() else {
            return (None, arrived);
        };
        let t = now.duration_since(at).as_secs_f32() / BLOCK_FADE.as_secs_f32();
        if t >= 1.0 {
            self.block_fades.remove(path);
            return (None, arrived);
        }
        self.animating = true;
        (Some(ease_out(t)), arrived)
    }

    /// `runs` for `text` in piece `sub`, with text new since the last frame
    /// partly transparent. A piece whose block arrived this frame (`covered`)
    /// fades with the block instead.
    pub fn runs(
        &mut self,
        sub: usize,
        text: &str,
        runs: Vec<TextRun>,
        covered: bool,
    ) -> Vec<TextRun> {
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
        let fade = self.live && !covered;
        let fades = self.fades.entry(sub).or_default();
        if let Some(start) = start.filter(|_| fade) {
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

/// CSS `ease-out`, `cubic-bezier(0, 0, 0.58, 1)`, at `x` of the way through.
fn ease_out(x: f32) -> f32 {
    // The curve's x(t) = 1.74t² - 0.74t³ only rises, so bisect for t.
    let (mut low, mut high) = (0.0_f32, 1.0_f32);
    for _ in 0..16 {
        let t = (low + high) / 2.0;
        if t * t * (1.74 - 0.74 * t) < x {
            low = t;
        } else {
            high = t;
        }
    }
    let t = (low + high) / 2.0;
    t * t * (3.0 - 2.0 * t)
}
