//! pyannote-segmentation-3.0 ONNX wrapper.
//!
//! **STATUS: scaffolded, ONNX inference pending.** The powerset →
//! voiced-slice decoder is fully implemented and unit-tested; the
//! ONNX forward pass is the next session's job (see `PROGRESS.md`).
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

use anyhow::{anyhow, Result};
use std::sync::atomic::AtomicBool;

use crate::models::{model_dir, ModelKind};

/// Approximate frame stride in milliseconds. sherpa-onnx's export of
/// pyannote-seg-3.0 emits 293 frames per 5 s window ≈ 17.07 ms per
/// frame.
const FRAME_STRIDE_MS: u64 = 17;

/// Minimum slice duration that survives the powerset filter. Frame-
/// level noise (a single misclassified frame) produces 17 ms blips.
const MIN_SLICE_MS: u64 = 100;

/// Powerset bitmask: which of {A, B, C} are active in each class.
#[allow(dead_code)]
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
pub struct VoicedSlice {
    pub start_ms: u64,
    pub end_ms: u64,
    /// 0/1/2 — index into the *local* (this-window) speaker space.
    pub local_speaker: u8,
    pub overlap: bool,
}

pub struct Segmenter {
    model_name: String,
}

impl Segmenter {
    pub fn new(name: &str) -> Result<Self> {
        Ok(Self {
            model_name: name.to_string(),
        })
    }

    pub fn warm_up(&mut self) -> Result<()> {
        let path = model_dir(ModelKind::Diarize, &self.model_name)?.join("segmentation.onnx");
        if !path.exists() {
            return Err(anyhow!("segmenter ONNX missing: {}", path.display()));
        }
        Ok(())
    }

    /// Run segmentation on a window of audio. `window_t0_ms` is the
    /// absolute (session-relative) start time of the window.
    pub fn segment(
        &mut self,
        _window: &[f32],
        _window_t0_ms: u64,
        _cancel: &AtomicBool,
    ) -> Result<Vec<VoicedSlice>> {
        // TODO(diarization-branch, session-2): wire ort 2.0.0-rc.12
        // forward → `[1, T, 7]` logits → argmax per frame →
        // `rle_to_slices`. See `PROGRESS.md` § "pyannote-seg forward".
        Err(anyhow!(
            "pyannote-seg ONNX inference not yet implemented — see PROGRESS.md"
        ))
    }
}

/// Run-length-encode a per-frame bitmask sequence into voiced slices.
/// Each contiguous run of frames with the same active-speaker set
/// becomes one `VoicedSlice` per active local speaker. Overlap is
/// `true` when more than one bit is set in the run's mask.
///
/// Pure function — kept around for the ort wire-up to call once it
/// has logits.
#[allow(dead_code)]
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
