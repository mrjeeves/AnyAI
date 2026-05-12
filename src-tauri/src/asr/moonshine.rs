//! Moonshine ASR backend (UsefulSensors Moonshine v2).
//!
//! **STATUS: scaffolded, ONNX inference pending.** The trait surface,
//! model-file resolution, and tokenizer loading work today; the
//! per-chunk forward pass through the encoder + merged decoder still
//! needs to be wired against the ort 2.0.0-rc.12 API (see
//! `PROGRESS.md` at repo root for the exact list of TODOs).
//!
//! Moonshine ships as an encoder/decoder ONNX pair. The encoder is the
//! "ergodic streaming encoder" — it takes raw 16 kHz mono f32 PCM (not
//! a mel spectrogram, unlike whisper) and produces a feature sequence.
//! The decoder is a merged autoregressive seq2seq decoder with past
//! key/value caches baked into the ONNX graph; per step we feed the
//! previous token plus past-KV tensors and read back the next token +
//! updated KV. Greedy argmax decode, stop on EOS.
//!
//! Tokenizer is HuggingFace `tokenizer.json` (BPE). We decode token
//! IDs → text via the `tokenizers` crate.
//!
//! Reference: <https://github.com/usefulsensors/moonshine>.

use anyhow::{anyhow, Result};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use tokenizers::Tokenizer;

use crate::asr::{AsrBackend, AsrCaps, AsrChunkOut};
use crate::models::{model_dir, ModelKind};

/// Moonshine's special-token IDs. These are fixed by the model card
/// and used to seed the decoder + detect end-of-sequence.
#[allow(dead_code)]
const START_TOKEN: i64 = 1;
#[allow(dead_code)]
const EOS_TOKEN: i64 = 2;

/// Cap on decoded tokens per chunk. Defends against pathological
/// decoder loops on noisy / out-of-distribution audio.
#[allow(dead_code)]
const MAX_DECODE_STEPS: usize = 256;

/// Moonshine Small ONNX backend.
pub struct MoonshineBackend {
    /// Logical model name from the manifest (e.g.
    /// `moonshine-small-q8`). Used to locate on-disk artifacts.
    model_name: String,
    /// Loaded once during warm-up. Inference uses it to detokenize
    /// the decoder's argmax IDs.
    tokenizer: Option<Tokenizer>,
}

impl MoonshineBackend {
    pub fn new(model_name: &str) -> Result<Self> {
        Ok(Self {
            model_name: model_name.to_string(),
            tokenizer: None,
        })
    }

    fn artifact_path(&self, filename: &str) -> Result<PathBuf> {
        Ok(model_dir(ModelKind::Asr, &self.model_name)?.join(filename))
    }
}

impl AsrBackend for MoonshineBackend {
    fn caps(&self) -> AsrCaps {
        AsrCaps {
            label: "Moonshine Small",
            // Moonshine is happiest on 0.5–1.0 s feeds — short chunks
            // keep the streaming encoder responsive without overpaying
            // per-chunk fixed cost. 1.0 s is the sweet spot in
            // practice and matches the upstream reference latency.
            chunk_seconds: 1.0,
            min_tail_seconds: 0.3,
            multilingual: false,
            streaming: true,
            // Moonshine doesn't grow stateful caches across chunks
            // (each `process_chunk` is independent), so the worker
            // doesn't need to recycle anything.
            state_reset_chunks: 0,
        }
    }

    fn warm_up(&mut self) -> Result<()> {
        // Confirm the encoder + decoder artifacts exist on disk so we
        // fail fast at warm-up rather than mid-session. Actual ORT
        // session loading lands when the ort 2.x wire-up does.
        let enc_path = self.artifact_path("encoder.onnx")?;
        let dec_path = self.artifact_path("decoder.onnx")?;
        let tok_path = self.artifact_path("tokenizer.json")?;
        for p in [&enc_path, &dec_path, &tok_path] {
            if !p.exists() {
                return Err(anyhow!("Moonshine artifact missing: {}", p.display()));
            }
        }
        let tokenizer = Tokenizer::from_file(&tok_path)
            .map_err(|e| anyhow!("loading tokenizer {}: {e}", tok_path.display()))?;
        self.tokenizer = Some(tokenizer);
        Ok(())
    }

    fn process_chunk(
        &mut self,
        _pcm16k_mono: &[f32],
        _chunk_t0_ms: u64,
        _cancel: &AtomicBool,
    ) -> Result<AsrChunkOut> {
        // TODO(diarization-branch, session-2): wire ort 2.0.0-rc.12
        // encoder forward → autoregressive merged-decoder loop →
        // tokenizer.decode. The decoder uses past-KV outputs as
        // inputs on the next step; the merged export exposes
        // `past_key_values.{n}.{decoder|encoder}.{key|value}` inputs
        // paired with `present.{n}.{decoder|encoder}.{key|value}`
        // outputs. See `PROGRESS.md` § "Moonshine forward".
        Err(anyhow!(
            "Moonshine ONNX inference not yet implemented — see PROGRESS.md"
        ))
    }

    fn reset_state(&mut self) {
        // Moonshine doesn't carry state across chunks in this
        // configuration. Nothing to reset.
    }
}
