//! pyannote-segmentation-3.0 ONNX wrapper.
//!
//! Inputs 16 kHz mono f32 audio (typically 10 s windows), outputs
//! per-frame logits of shape `[batch=1, T_frames, 7]`. Frame stride
//! is the model's native ~17 ms. The 7-class axis is the **powerset**
//! of "up to 3 simultaneous speakers in this window":
//!
//! | class | local speakers active |
//! |------:|-----------------------|
//! | 0 | ∅ (silence)            |
//! | 1 | {A}                    |
//! | 2 | {B}                    |
//! | 3 | {C}                    |
//! | 4 | {A, B}                 |
//! | 5 | {A, C}                 |
//! | 6 | {B, C}                 |
//!
//! Argmax per frame → bitmask of which local speakers (A/B/C) are
//! active. Run-length-encode along the time axis to get voiced
//! slices, dropping any < 100 ms (frame-level noise).
//!
//! Reference: <https://huggingface.co/pyannote/segmentation-3.0>.

use anyhow::{anyhow, Context, Result};
use ndarray::Array2;
use ort::session::{builder::GraphOptimizationLevel, Session};
use ort::value::Tensor;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::models::{model_dir, ModelKind};
use crate::ort_setup;

/// Approximate frame stride in milliseconds. sherpa-onnx's export of
/// pyannote-seg-3.0 emits 293 frames per 5 s window ≈ 17.07 ms per
/// frame.
const FRAME_STRIDE_MS: u64 = 17;

/// Minimum slice duration that survives the powerset filter. Frame-
/// level noise (a single misclassified frame) produces 17 ms blips.
const MIN_SLICE_MS: u64 = 100;

/// Powerset bitmask: which of {A, B, C} are active in each class.
const CLASS_TO_BITS: [u8; 7] = [
    0b000, // ∅
    0b001, // {A}
    0b010, // {B}
    0b100, // {C}
    0b011, // {A, B}
    0b101, // {A, C}
    0b110, // {B, C}
];

/// A run of frames with a stable speaker bitmask. Times are absolute
/// (session-relative milliseconds).
#[derive(Debug, Clone)]
#[allow(dead_code)] // `local_speaker` is read by the embedder slice
                    // extractor when the ort wire-up lands — kept here
                    // so the surface stays stable across that change.
pub struct VoicedSlice {
    pub start_ms: u64,
    pub end_ms: u64,
    /// 0/1/2 — index into the *local* (this-window) speaker space.
    pub local_speaker: u8,
    pub overlap: bool,
}

pub struct Segmenter {
    model_name: String,
    session: Option<Session>,
    /// Sniffed at warm-up.
    input_name: String,
    output_name: String,
}

impl Segmenter {
    pub fn new(name: &str) -> Result<Self> {
        Ok(Self {
            model_name: name.to_string(),
            session: None,
            input_name: "waveform".to_string(),
            output_name: "logits".to_string(),
        })
    }

    pub fn warm_up(&mut self) -> Result<()> {
        let path = model_dir(ModelKind::Diarize, &self.model_name)?.join("segmentation.onnx");
        if !path.exists() {
            return Err(anyhow!("segmenter ONNX missing: {}", path.display()));
        }
        // Optimisation level restored to `Level3` (the crate default).
        // PR #115 walked this down to `Level1` while debugging the
        // Moonshine "Loading…" hang under the assumption that the
        // diarize warm-up was also stuck in the same way; with the
        // dylib-init fix in `ort_setup` the real cause is addressed
        // upstream, and there's no reason to leave whole-graph
        // optimisation off.
        let path_owned = path.clone();
        let model_name_owned = self.model_name.clone();
        let threads = intra_threads();
        let session = ort_setup::load_session("speaker segmenter", 90, move || {
            Session::builder()
                .map_err(|e| anyhow!("ort builder: {e}"))?
                .with_optimization_level(GraphOptimizationLevel::Level3)
                .map_err(|e| anyhow!("ort opt level: {e}"))?
                .with_intra_threads(threads)
                .map_err(|e| anyhow!("ort threads: {e}"))?
                .commit_from_file(&path_owned)
                .map_err(|e| anyhow!("loading {}: {e}", path_owned.display()))
                .with_context(|| format!("warm_up segmenter {model_name_owned}"))
        })?;

        // Sherpa-onnx's export uses `waveform` as the input and
        // `logits` as the output, but we suffix-match so a re-export
        // (which is what we'd switch to if upstream patches the
        // dynamic-shape issues) doesn't break us.
        for input in session.inputs() {
            let n = input.name().to_lowercase();
            if n.contains("wave") || n.contains("audio") || n == "input" {
                self.input_name = input.name().to_string();
                break;
            }
        }
        for output in session.outputs() {
            let n = output.name().to_lowercase();
            if n.contains("logit") || n.contains("output") || n.contains("score") {
                self.output_name = output.name().to_string();
                break;
            }
        }
        self.session = Some(session);
        Ok(())
    }

    /// Run segmentation on a window of audio. `window_t0_ms` is the
    /// absolute (session-relative) start time of the window.
    pub fn segment(
        &mut self,
        window: &[f32],
        window_t0_ms: u64,
        cancel: &AtomicBool,
    ) -> Result<Vec<VoicedSlice>> {
        // < 100 ms of audio: nothing meaningful to segment. The
        // pyannote-seg-3.0 model's receptive field is well above
        // this, so feeding it tiny windows produces garbage.
        if window.len() < 16_000 / 10 {
            return Ok(Vec::new());
        }
        if cancel.load(Ordering::Relaxed) {
            return Ok(Vec::new());
        }
        let session = self
            .session
            .as_mut()
            .ok_or_else(|| anyhow!("segmenter not warmed up"))?;

        let input: Array2<f32> = Array2::from_shape_vec((1, window.len()), window.to_vec())
            .map_err(|e| anyhow!("shape input: {e}"))?;
        let tensor = Tensor::from_array(input).map_err(|e| anyhow!("ort tensor: {e}"))?;
        let outputs = session
            .run(ort::inputs![self.input_name.as_str() => tensor])
            .map_err(|e| anyhow!("ort run: {e}"))?;

        let value = outputs
            .get(self.output_name.as_str())
            .ok_or_else(|| anyhow!("segmenter missing output {}", self.output_name))?;
        let logits = value
            .try_extract_array::<f32>()
            .map_err(|e| anyhow!("ort extract: {e}"))?;
        // Expected shape: [1, T, 7].
        let shape = logits.shape().to_vec();
        if shape.len() != 3 || shape[0] != 1 || shape[2] != 7 {
            return Err(anyhow!(
                "unexpected segmenter output shape {:?} (want [1, T, 7])",
                shape
            ));
        }
        let t_frames = shape[1];

        // Argmax per frame → bitmask. Walking the ArrayView via
        // `.iter()` would give C-order (frame-major over class), so
        // we index explicitly to keep the per-frame scan obvious.
        let mut bitmasks = Vec::with_capacity(t_frames);
        for t in 0..t_frames {
            let mut best_class = 0usize;
            let mut best_score = f32::NEG_INFINITY;
            for k in 0..7 {
                let s = logits[[0, t, k]];
                if s > best_score {
                    best_score = s;
                    best_class = k;
                }
            }
            bitmasks.push(CLASS_TO_BITS[best_class]);
        }

        Ok(rle_to_slices(&bitmasks, window_t0_ms))
    }
}

fn intra_threads() -> usize {
    let n = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(2);
    n.saturating_sub(1).clamp(1, 2)
}

/// Run-length-encode a per-frame bitmask sequence into voiced slices.
/// Each contiguous run of frames with the same active-speaker set
/// becomes one `VoicedSlice` per active local speaker. Overlap is
/// `true` when more than one bit is set in the run's mask.
pub(crate) fn rle_to_slices(bitmasks: &[u8], window_t0_ms: u64) -> Vec<VoicedSlice> {
    let mut out = Vec::new();
    if bitmasks.is_empty() {
        return out;
    }
    let mut run_start = 0usize;
    let mut cur = bitmasks[0];
    for i in 1..=bitmasks.len() {
        let next = bitmasks.get(i).copied().unwrap_or(255);
        if next != cur {
            if cur != 0 {
                let start_ms = window_t0_ms + (run_start as u64 * FRAME_STRIDE_MS);
                let end_ms = window_t0_ms + (i as u64 * FRAME_STRIDE_MS);
                if end_ms.saturating_sub(start_ms) >= MIN_SLICE_MS {
                    let overlap = cur.count_ones() > 1;
                    for spk in 0..3 {
                        if (cur >> spk) & 1 == 1 {
                            out.push(VoicedSlice {
                                start_ms,
                                end_ms,
                                local_speaker: spk,
                                overlap,
                            });
                        }
                    }
                }
            }
            run_start = i;
            cur = next;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rle_empty_input_returns_empty() {
        let out = rle_to_slices(&[], 0);
        assert!(out.is_empty());
    }

    #[test]
    fn rle_silence_produces_no_slices() {
        let frames = vec![0u8; 100];
        let out = rle_to_slices(&frames, 0);
        assert!(out.is_empty());
    }

    #[test]
    fn rle_continuous_voice_emits_one_slice() {
        let frames = vec![0b001u8; 100]; // 100 frames × 17 ms = 1700 ms
        let out = rle_to_slices(&frames, 1000);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].local_speaker, 0);
        assert_eq!(out[0].start_ms, 1000);
        assert_eq!(out[0].end_ms, 1000 + 100 * 17);
        assert!(!out[0].overlap);
    }

    #[test]
    fn rle_overlap_emits_one_slice_per_speaker_with_overlap_flag() {
        let frames = vec![0b011u8; 100]; // A + B simultaneously
        let out = rle_to_slices(&frames, 0);
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|s| s.overlap));
        assert!(out.iter().any(|s| s.local_speaker == 0));
        assert!(out.iter().any(|s| s.local_speaker == 1));
    }

    #[test]
    fn rle_drops_slices_below_min_duration() {
        let frames = vec![0b001u8; 5];
        let out = rle_to_slices(&frames, 0);
        assert!(out.is_empty());
    }
}
