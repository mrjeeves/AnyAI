//! Moonshine ASR backend (UsefulSensors Moonshine v2).
//!
//! Moonshine ships as an encoder/decoder ONNX pair. The encoder is
//! the "ergodic streaming encoder" — it takes raw 16 kHz mono f32 PCM
//! (not a mel spectrogram, unlike whisper) and produces a feature
//! sequence `[batch, time, dim]`. The decoder is a merged
//! autoregressive seq2seq decoder; we drive it with greedy argmax
//! decode until EOS.
//!
//! **Decode loop strategy.** Moonshine's merged decoder ONNX
//! technically supports two branches gated on a `use_cache_branch`
//! input: a fast cached path (one token in, past-KV grows by one)
//! and a slow no-cache path (full input_ids sequence in, ignore past
//! tensors). The cached path needs careful shape management of the
//! past-KV inputs whose dimensions are model-specific (n_heads /
//! head_dim) and which I can't introspect at runtime without
//! actually loading the model. The no-cache path is simpler at the
//! cost of O(n²) decoder forwards per chunk; with `n ≤ 30` tokens
//! per 1 s chunk that's tractable (≤ ~50 ms even on Pi 5). Pick
//! correctness + simplicity here; the cached path is a follow-up
//! optimisation when there's a measured latency win to chase.
//!
//! Tokenizer is HuggingFace `tokenizer.json` (BPE). We decode token
//! IDs → text via the `tokenizers` crate with its pure-Rust
//! `fancy-regex` backend (see Cargo.toml).
//!
//! Reference: <https://github.com/usefulsensors/moonshine>.

use anyhow::{anyhow, Context, Result};
use ndarray::{Array2, ArrayD};
use ort::session::{builder::GraphOptimizationLevel, Session};
use ort::value::Tensor;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use tokenizers::Tokenizer;

use crate::asr::{AsrBackend, AsrCaps, AsrChunkOut, AsrSegment};
use crate::models::{model_dir, ModelKind};
use crate::ort_setup;

/// Moonshine's special-token IDs. Fixed by the model card; seeds the
/// decoder and detects end-of-sequence.
const START_TOKEN: i64 = 1;
const EOS_TOKEN: i64 = 2;

/// Cap on decoded tokens per chunk. Defends against pathological
/// decoder loops on noisy / out-of-distribution audio — a 1 s chunk
/// realistically produces ≤ 30 tokens.
const MAX_DECODE_STEPS: usize = 64;

pub struct MoonshineBackend {
    model_name: String,
    encoder: Option<Session>,
    decoder: Option<Session>,
    tokenizer: Option<Tokenizer>,
    /// Sniffed at warm-up. Defaults are the names UsefulSensors
    /// publishes; suffix-match handles renames.
    enc_input_name: String,
    enc_output_name: String,
    dec_input_ids_name: String,
    dec_enc_hidden_name: String,
    dec_logits_name: String,
    /// Names of all past-KV inputs the decoder graph declares.
    /// We pass zero-shape tensors for each one on every step (the
    /// no-cache decode branch the model exposes via
    /// `use_cache_branch=false`).
    past_kv_input_names: Vec<String>,
    /// Name of the `use_cache_branch` input if the export has one.
    /// Some Moonshine ONNX exports have it; some don't. When
    /// present we pass `false` every step.
    use_cache_branch_name: Option<String>,
}

impl MoonshineBackend {
    pub fn new(model_name: &str) -> Result<Self> {
        Ok(Self {
            model_name: model_name.to_string(),
            encoder: None,
            decoder: None,
            tokenizer: None,
            enc_input_name: "input_values".to_string(),
            enc_output_name: "last_hidden_state".to_string(),
            dec_input_ids_name: "input_ids".to_string(),
            dec_enc_hidden_name: "encoder_hidden_states".to_string(),
            dec_logits_name: "logits".to_string(),
            past_kv_input_names: Vec::new(),
            use_cache_branch_name: None,
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
            chunk_seconds: 1.0,
            min_tail_seconds: 0.3,
            multilingual: false,
            streaming: true,
            state_reset_chunks: 0,
        }
    }

    fn warm_up(&mut self, on_stage: &dyn Fn(&str), cancel: &AtomicBool) -> Result<()> {
        let enc_path = self.artifact_path("encoder.onnx")?;
        let dec_path = self.artifact_path("decoder.onnx")?;
        let tok_path = self.artifact_path("tokenizer.json")?;

        on_stage("Verifying Moonshine files…");
        // Pre-flight: not just "exists" but "has plausibly the right
        // number of bytes". A truncated download (network drop on
        // first pull, cancelled-mid-stream) leaves a 0-byte or
        // partial .onnx file behind that passes the `exists()` check
        // but causes `commit_from_file` to hang for minutes inside
        // ORT trying to make sense of the malformed protobuf. Bailing
        // here with a clear error lets the user re-pull instead.
        check_file_plausible(&enc_path, MIN_ENCODER_BYTES, "encoder")?;
        check_file_plausible(&dec_path, MIN_DECODER_BYTES, "decoder")?;
        check_file_plausible(&tok_path, MIN_TOKENIZER_BYTES, "tokenizer")?;
        if cancel.load(Ordering::Relaxed) {
            return Err(anyhow!("Moonshine warm-up cancelled"));
        }

        // ORT graph optimization is set to `Disable` (was Level1 in
        // #113, originally Level3 before that). Even Level1's
        // constant-folding pass can stall for minutes inside
        // `commit_from_file` on the quantized merged-decoder ONNX
        // when run on memory-constrained Apple Silicon (reported on
        // an M-series 8 GB laptop). The model is already INT8 from
        // the upstream export, so the runtime wins from graph
        // optimization are minimal anyway. `with_intra_threads(1)`
        // for the same reason — the ORT thread-pool init was
        // hypothesised as a second source of stalls under memory
        // pressure; single-thread is the safest baseline. We can
        // walk these back up tier-by-tier if perf becomes a
        // problem, but correctness ("Record actually works") wins
        // over a marginal latency improvement here.
        on_stage(&format!(
            "Loading Moonshine encoder… ({})",
            ort_setup::status().diagnostic()
        ));
        let enc_path_owned = enc_path.clone();
        let encoder = ort_setup::load_session("Moonshine encoder", 90, move || {
            Session::builder()
                .map_err(|e| anyhow!("ort builder: {e}"))?
                .with_optimization_level(GraphOptimizationLevel::Disable)
                .map_err(|e| anyhow!("ort opt level: {e}"))?
                .with_intra_threads(1)
                .map_err(|e| anyhow!("ort threads: {e}"))?
                .commit_from_file(&enc_path_owned)
                .map_err(|e| anyhow!("loading {}: {e}", enc_path_owned.display()))
                .with_context(|| "warm_up moonshine encoder".to_string())
        })?;

        if cancel.load(Ordering::Relaxed) {
            return Err(anyhow!("Moonshine warm-up cancelled"));
        }
        on_stage("Loading Moonshine decoder…");
        let dec_path_owned = dec_path.clone();
        let decoder = ort_setup::load_session("Moonshine decoder", 90, move || {
            Session::builder()
                .map_err(|e| anyhow!("ort builder: {e}"))?
                .with_optimization_level(GraphOptimizationLevel::Disable)
                .map_err(|e| anyhow!("ort opt level: {e}"))?
                .with_intra_threads(1)
                .map_err(|e| anyhow!("ort threads: {e}"))?
                .commit_from_file(&dec_path_owned)
                .map_err(|e| anyhow!("loading {}: {e}", dec_path_owned.display()))
                .with_context(|| "warm_up moonshine decoder".to_string())
        })?;

        // Sniff encoder I/O. First input + first output by
        // convention; tolerate any naming.
        if let Some(input) = encoder.inputs().first() {
            self.enc_input_name = input.name().to_string();
        }
        if let Some(output) = encoder.outputs().first() {
            self.enc_output_name = output.name().to_string();
        }

        // Sniff decoder I/O by suffix-match against the canonical
        // names. Past-KV inputs all start with `past_key_values.`;
        // the `use_cache_branch` input is bool / int8 and named
        // exactly that.
        for input in decoder.inputs() {
            let n = input.name();
            let lower = n.to_lowercase();
            if n.starts_with("past_key_values.") {
                self.past_kv_input_names.push(n.to_string());
            } else if lower == "use_cache_branch" {
                self.use_cache_branch_name = Some(n.to_string());
            } else if lower.ends_with("input_ids") {
                self.dec_input_ids_name = n.to_string();
            } else if lower.contains("encoder_hidden") || lower.contains("encoder_outputs") {
                self.dec_enc_hidden_name = n.to_string();
            }
        }
        for output in decoder.outputs() {
            if output.name().to_lowercase().ends_with("logits") {
                self.dec_logits_name = output.name().to_string();
                break;
            }
        }

        if cancel.load(Ordering::Relaxed) {
            return Err(anyhow!("Moonshine warm-up cancelled"));
        }
        on_stage("Loading Moonshine tokenizer…");
        let tokenizer = Tokenizer::from_file(&tok_path)
            .map_err(|e| anyhow!("loading tokenizer {}: {e}", tok_path.display()))?;

        self.encoder = Some(encoder);
        self.decoder = Some(decoder);
        self.tokenizer = Some(tokenizer);
        Ok(())
    }

    fn process_chunk(
        &mut self,
        pcm16k_mono: &[f32],
        _chunk_t0_ms: u64,
        cancel: &AtomicBool,
    ) -> Result<AsrChunkOut> {
        if pcm16k_mono.len() < 16_000 / 10 {
            return Ok(AsrChunkOut::default());
        }
        if cancel.load(Ordering::Relaxed) {
            return Ok(AsrChunkOut::default());
        }

        // 1. Encoder forward. `[1, N]` raw PCM → `[1, T, D]` hidden.
        let enc_hidden = self.run_encoder(pcm16k_mono)?;

        if cancel.load(Ordering::Relaxed) {
            return Ok(AsrChunkOut::default());
        }

        // 2. Greedy autoregressive decode (no-cache branch). Each
        // step we feed the entire accumulated `input_ids` sequence
        // and read out the next token's argmax logit.
        let mut tokens: Vec<i64> = vec![START_TOKEN];
        for _ in 0..MAX_DECODE_STEPS {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            let next = self.run_decoder_step(&tokens, &enc_hidden)?;
            if next == EOS_TOKEN {
                break;
            }
            tokens.push(next);
        }

        // 3. Detokenize. Skip the start token; map remaining IDs to
        // text. Negative IDs (the merged decoder shouldn't emit
        // these but defend anyway) collapse to 0.
        let ids: Vec<u32> = tokens.iter().skip(1).map(|&t| t.max(0) as u32).collect();
        let tokenizer = self
            .tokenizer
            .as_ref()
            .ok_or_else(|| anyhow!("Moonshine tokenizer not loaded"))?;
        let text = tokenizer
            .decode(&ids, true)
            .map_err(|e| anyhow!("tokenizer decode: {e}"))?;
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Ok(AsrChunkOut::default());
        }

        let segment = AsrSegment {
            start_ms: 0,
            end_ms: (pcm16k_mono.len() as u64 * 1000) / 16_000,
            text: trimmed.to_string(),
            confidence: None,
        };
        Ok(AsrChunkOut {
            segments: vec![segment],
            used_state: false,
        })
    }

    fn reset_state(&mut self) {
        // Each `process_chunk` is independent — no carried state.
    }
}

impl MoonshineBackend {
    /// Encoder forward. Returns the owned `[1, T, D]` hidden array
    /// (cloned out of ORT's session arena so it survives across
    /// decoder steps).
    fn run_encoder(&mut self, pcm16k_mono: &[f32]) -> Result<ArrayD<f32>> {
        let encoder = self
            .encoder
            .as_mut()
            .ok_or_else(|| anyhow!("Moonshine encoder not warmed up"))?;

        let input: Array2<f32> =
            Array2::from_shape_vec((1, pcm16k_mono.len()), pcm16k_mono.to_vec())
                .map_err(|e| anyhow!("shape encoder input: {e}"))?;
        let tensor = Tensor::from_array(input).map_err(|e| anyhow!("ort tensor: {e}"))?;
        let outputs = encoder
            .run(ort::inputs![self.enc_input_name.as_str() => tensor])
            .map_err(|e| anyhow!("ort encoder run: {e}"))?;
        let view = outputs
            .get(self.enc_output_name.as_str())
            .ok_or_else(|| anyhow!("encoder missing output {}", self.enc_output_name))?
            .try_extract_array::<f32>()
            .map_err(|e| anyhow!("ort extract encoder: {e}"))?;
        Ok(view.to_owned())
    }

    /// One decoder forward pass: feed the full `tokens` sequence and
    /// the encoder hidden states, read back logits, return the
    /// argmax over the last position's vocab axis.
    fn run_decoder_step(&mut self, tokens: &[i64], enc_hidden: &ArrayD<f32>) -> Result<i64> {
        let decoder = self
            .decoder
            .as_mut()
            .ok_or_else(|| anyhow!("Moonshine decoder not warmed up"))?;
        let dec_input_ids_name = self.dec_input_ids_name.clone();
        let dec_enc_hidden_name = self.dec_enc_hidden_name.clone();
        let dec_logits_name = self.dec_logits_name.clone();
        let past_names = self.past_kv_input_names.clone();
        let use_cache_name = self.use_cache_branch_name.clone();

        let input_ids: Array2<i64> = Array2::from_shape_vec((1, tokens.len()), tokens.to_vec())
            .map_err(|e| anyhow!("shape input_ids: {e}"))?;
        let input_ids_tensor =
            Tensor::from_array(input_ids).map_err(|e| anyhow!("ort tensor input_ids: {e}"))?;
        let enc_tensor =
            Tensor::from_array(enc_hidden.clone()).map_err(|e| anyhow!("ort tensor enc: {e}"))?;

        let mut inputs: Vec<(
            std::borrow::Cow<'static, str>,
            ort::session::SessionInputValue<'_>,
        )> = vec![
            (
                std::borrow::Cow::Owned(dec_input_ids_name),
                input_ids_tensor.into(),
            ),
            (
                std::borrow::Cow::Owned(dec_enc_hidden_name),
                enc_tensor.into(),
            ),
        ];

        // No-cache branch: dummy zero-shape past-KV tensors. The
        // model's `use_cache_branch=false` graph ignores their
        // contents but the ONNX runtime still needs every declared
        // input bound. `[1, 0, 0, 0]` is a generic zero-volume shape
        // that ORT accepts across the merged-decoder exports we
        // ship. If a future export rejects it, switch to inspecting
        // `input.dtype()` shape metadata to derive the right
        // n_heads / head_dim.
        for name in &past_names {
            let empty: ArrayD<f32> = ArrayD::zeros(ndarray::IxDyn(&[1, 0, 0, 0]));
            let t = Tensor::from_array(empty).map_err(|e| anyhow!("ort tensor past-kv: {e}"))?;
            inputs.push((std::borrow::Cow::Owned(name.clone()), t.into()));
        }

        // `use_cache_branch` flag: bool encoded as a single-element
        // tensor. ONNX bool tensors round-trip as i8 (0/1) on most
        // runtimes; if a future export uses a true bool dtype, ORT
        // will surface a type-mismatch error and we'll switch.
        if let Some(name) = use_cache_name {
            let flag: ndarray::Array1<bool> = ndarray::Array1::from_vec(vec![false]);
            let t = Tensor::from_array(flag).map_err(|e| anyhow!("ort tensor use_cache: {e}"))?;
            inputs.push((std::borrow::Cow::Owned(name), t.into()));
        }

        let outputs = decoder
            .run(inputs)
            .map_err(|e| anyhow!("ort decoder run: {e}"))?;
        let logits_view = outputs
            .get(dec_logits_name.as_str())
            .ok_or_else(|| anyhow!("decoder missing logits"))?
            .try_extract_array::<f32>()
            .map_err(|e| anyhow!("ort extract logits: {e}"))?;
        // Shape `[1, seq_len, vocab]`. We want the argmax at the
        // last position.
        let shape = logits_view.shape().to_vec();
        if shape.len() != 3 || shape[0] != 1 {
            return Err(anyhow!("unexpected decoder logits shape {:?}", shape));
        }
        let last = shape[1] - 1;
        let vocab = shape[2];
        let mut best_i = 0usize;
        let mut best_v = f32::NEG_INFINITY;
        for v in 0..vocab {
            let s = logits_view[[0, last, v]];
            if s > best_v {
                best_v = s;
                best_i = v;
            }
        }
        Ok(best_i as i64)
    }
}

/// Minimum plausible size for a fully-downloaded Moonshine encoder
/// (~30 MB INT8 ONNX export). Mirrors `min_bytes` in
/// `src-tauri/src/models.rs` for the same artifact so we catch a
/// truncated download here instead of letting it stall inside
/// `commit_from_file`.
const MIN_ENCODER_BYTES: u64 = 15_000_000;
const MIN_DECODER_BYTES: u64 = 20_000_000;
const MIN_TOKENIZER_BYTES: u64 = 500_000;

/// Verify an artifact file is present and at least `min_bytes` long.
/// `commit_from_file` on a truncated ONNX file enters a slow
/// protobuf-parsing path that looks identical to "ORT is loading the
/// model" but never returns. Catching the truncation here surfaces a
/// clear error the user can act on (delete + re-download).
fn check_file_plausible(path: &std::path::Path, min_bytes: u64, label: &str) -> Result<()> {
    let meta = std::fs::metadata(path).map_err(|e| {
        anyhow!(
            "Moonshine {label} file is missing or unreadable at {}: {e}",
            path.display()
        )
    })?;
    if meta.len() < min_bytes {
        return Err(anyhow!(
            "Moonshine {label} at {} looks truncated ({} bytes, expected ≥ {}). Delete it from Settings → Models and re-download.",
            path.display(),
            meta.len(),
            min_bytes,
        ));
    }
    Ok(())
}
