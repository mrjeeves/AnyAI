//! NVIDIA Parakeet TDT 0.6B v3 ASR backend.
//!
//! **STATUS: scaffolded, ONNX inference pending.** The trait surface,
//! model-file resolution, vocab loading, and SentencePiece-style
//! detokenization work today; the ONNX forward pass is the next
//! session's job (see `PROGRESS.md`).
//!
//! Parakeet is a CTC/RNN-T family hybrid trained by NVIDIA's NeMo
//! team. The v3 0.6B variant adds 25-language support. We pull the
//! community ONNX export (istupakov/parakeet-tdt-0.6b-v3-onnx) which
//! bakes the encoder + RNNT predictor + joint network + the TDT
//! decode loop into a single graph: input is raw f32 PCM (with a
//! lengths tensor), output is a `[batch, time]` int sequence of token
//! IDs plus per-token frame indices.
//!
//! Vocab is a flat `tokens.txt` (one BPE piece per line, indexed by
//! line number). We detokenize by joining pieces and replacing the
//! SentencePiece ▁ with spaces.

use anyhow::{anyhow, Context, Result};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use crate::asr::{AsrBackend, AsrCaps, AsrChunkOut, AsrSegment};
use crate::models::{model_dir, ModelKind};

/// Frame stride used by the encoder, in samples. NeMo's standard
/// fastconformer stride is 80 ms at 16 kHz = 1280 samples/frame.
const FRAME_STRIDE_MS: u64 = 80;

/// SentencePiece word-boundary marker (Unicode lower-eighth block).
const WORD_BOUNDARY: &str = "\u{2581}";

pub struct ParakeetBackend {
    model_name: String,
    tokens: Vec<String>,
}

impl ParakeetBackend {
    pub fn new(model_name: &str) -> Result<Self> {
        Ok(Self {
            model_name: model_name.to_string(),
            tokens: Vec::new(),
        })
    }

    fn artifact_path(&self, filename: &str) -> Result<PathBuf> {
        Ok(model_dir(ModelKind::Asr, &self.model_name)?.join(filename))
    }

    /// Resolve a token ID to its string form. Out-of-vocab IDs
    /// resolve to empty so the RNNT blank effectively vanishes.
    #[allow(dead_code)]
    fn id_to_piece(&self, id: usize) -> &str {
        self.tokens.get(id).map(String::as_str).unwrap_or("")
    }
}

impl AsrBackend for ParakeetBackend {
    fn caps(&self) -> AsrCaps {
        AsrCaps {
            label: "Parakeet TDT 0.6B v3",
            chunk_seconds: 1.0,
            min_tail_seconds: 0.3,
            multilingual: true,
            streaming: true,
            state_reset_chunks: 0,
        }
    }

    fn warm_up(&mut self) -> Result<()> {
        let model_path = self.artifact_path("model.onnx")?;
        let tokens_path = self.artifact_path("tokens.txt")?;
        if !model_path.exists() {
            return Err(anyhow!("Parakeet model missing: {}", model_path.display()));
        }

        // Read vocabulary. Format is one token per line; line index =
        // token ID. NeMo exports include `<blk>` as line 0; we keep
        // it so the indexing matches the graph but `id_to_piece` for
        // a blank returns the empty string.
        let raw = std::fs::read_to_string(&tokens_path)
            .with_context(|| format!("reading {}", tokens_path.display()))?;
        let tokens: Vec<String> = raw
            .lines()
            .map(|l| {
                let piece = l.split('\t').next().unwrap_or("").to_string();
                if piece == "<blk>" || piece == "<pad>" || piece == "<unk>" {
                    String::new()
                } else {
                    piece
                }
            })
            .collect();
        if tokens.is_empty() {
            return Err(anyhow!("empty tokens.txt for {}", model_path.display()));
        }
        self.tokens = tokens;
        Ok(())
    }

    fn process_chunk(
        &mut self,
        _pcm16k_mono: &[f32],
        _chunk_t0_ms: u64,
        _cancel: &AtomicBool,
    ) -> Result<AsrChunkOut> {
        // TODO(diarization-branch, session-2): wire ort 2.0.0-rc.12
        // single-pass forward. Build the input tensor as `[1, T]` f32
        // (audio_signal) + optional `[1]` i64 lengths, run the
        // session, decode token IDs and (when the export emits them)
        // per-token frame indices to segments via
        // `decode_to_segments`. See `PROGRESS.md` § "Parakeet
        // forward".
        Err(anyhow!(
            "Parakeet ONNX inference not yet implemented — see PROGRESS.md"
        ))
    }

    fn reset_state(&mut self) {
        // Single-pass merged graph; no per-chunk state.
    }
}

impl ParakeetBackend {
    /// Stitch token IDs into one or more text segments. If the graph
    /// emits per-token frame indices, we split the chunk into
    /// multiple segments wherever there's a > 300 ms silent gap
    /// between tokens. Without timestamps, the whole chunk becomes
    /// one segment.
    ///
    /// Pure function — kept around for the ort wire-up to call once
    /// it has token IDs and frames.
    #[allow(dead_code)]
    pub(crate) fn decode_to_segments(
        &self,
        token_ids: &[i64],
        timestamps: Option<&[i64]>,
        chunk_samples: usize,
    ) -> Vec<AsrSegment> {
        let chunk_end_ms = (chunk_samples as u64 * 1000) / 16_000;
        if token_ids.is_empty() {
            return Vec::new();
        }

        let pieces: Vec<&str> = token_ids
            .iter()
            .map(|&id| self.id_to_piece(id.max(0) as usize))
            .collect();

        let Some(times) = timestamps else {
            let text = stitch_pieces(&pieces);
            if text.trim().is_empty() {
                return Vec::new();
            }
            return vec![AsrSegment {
                start_ms: 0,
                end_ms: chunk_end_ms,
                text,
                confidence: None,
            }];
        };

        const GAP_MS: u64 = 300;
        let gap_frames = (GAP_MS / FRAME_STRIDE_MS).max(1) as i64;

        let mut segments = Vec::new();
        let mut run_start: usize = 0;
        let mut prev_frame: Option<i64> = None;
        for (i, &frame) in times.iter().enumerate() {
            if let Some(prev) = prev_frame {
                if frame - prev > gap_frames && i > run_start {
                    segments.push(self.emit_run(&pieces[run_start..i], &times[run_start..i]));
                    run_start = i;
                }
            }
            prev_frame = Some(frame);
        }
        if run_start < pieces.len() {
            segments.push(self.emit_run(&pieces[run_start..], &times[run_start..]));
        }
        segments
            .into_iter()
            .filter(|s| !s.text.trim().is_empty())
            .collect()
    }

    fn emit_run(&self, pieces: &[&str], frames: &[i64]) -> AsrSegment {
        let text = stitch_pieces(pieces);
        let start_ms = frames.first().map(|f| *f as u64 * FRAME_STRIDE_MS).unwrap_or(0);
        let end_ms = frames
            .last()
            .map(|f| (*f as u64 + 1) * FRAME_STRIDE_MS)
            .unwrap_or(start_ms);
        AsrSegment {
            start_ms,
            end_ms,
            text,
            confidence: None,
        }
    }
}

/// Combine SentencePiece-style pieces into a readable string: replace
/// the U+2581 word-boundary marker with a space, drop empty pieces.
fn stitch_pieces(pieces: &[&str]) -> String {
    let mut out = String::new();
    for p in pieces {
        if p.is_empty() {
            continue;
        }
        if let Some(rest) = p.strip_prefix(WORD_BOUNDARY) {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(rest);
        } else {
            out.push_str(p);
        }
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stitch_simple() {
        let pieces = vec!["\u{2581}hello", "\u{2581}world"];
        assert_eq!(stitch_pieces(&pieces), "hello world");
    }

    #[test]
    fn stitch_subwords() {
        let pieces = vec!["\u{2581}un", "believ", "able"];
        assert_eq!(stitch_pieces(&pieces), "unbelievable");
    }

    #[test]
    fn stitch_skips_empty_pieces() {
        let pieces = vec!["", "\u{2581}cat", ""];
        assert_eq!(stitch_pieces(&pieces), "cat");
    }
}
