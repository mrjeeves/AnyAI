//! Speaker-embedding ONNX backend (wespeaker-r34 / 3D-Speaker CAM++).
//!
//! **STATUS: scaffolded, ONNX inference pending.** Model-file
//! resolution works; the forward pass is the next session's job
//! (see `PROGRESS.md`).
//!
//! Both supported models take 16 kHz mono f32 audio (typically a
//! 0.5–4 s slice of one voiced region) and emit a single fixed-size
//! L2-normalized embedding. Output dim is **256** for
//! wespeaker-voxceleb-resnet34-LM and **192** for CAM++ small.
//!
//! Both ONNX exports we ship take raw waveform input (the
//! spectrogram front-end is baked into the graph).

use anyhow::{anyhow, Result};

use crate::models::{model_dir, ModelKind};

/// Minimum slice length we'll embed. Below this, embedders' output
/// is dominated by their input-normalization pad and clusters poorly.
const MIN_EMBED_SAMPLES: usize = 16_000 / 2; // 0.5 s

pub struct Embedder {
    model_name: String,
    dim: Option<usize>,
}

impl Embedder {
    pub fn new(name: &str) -> Result<Self> {
        Ok(Self {
            model_name: name.to_string(),
            dim: None,
        })
    }

    pub fn warm_up(&mut self) -> Result<()> {
        let path = model_dir(ModelKind::Diarize, &self.model_name)?.join("embedder.onnx");
        if !path.exists() {
            return Err(anyhow!("embedder ONNX missing: {}", path.display()));
        }
        Ok(())
    }

    pub fn dim(&self) -> Option<usize> {
        self.dim
    }

    /// Embed one voiced slice. Returns an L2-normalized vector.
    pub fn embed(&mut self, slice_pcm: &[f32]) -> Result<Vec<f32>> {
        if slice_pcm.len() < MIN_EMBED_SAMPLES {
            return Ok(Vec::new());
        }
        // TODO(diarization-branch, session-2): wire ort 2.0.0-rc.12
        // forward → `[1, D]` (or `[1, 1, D]`) embedding → L2-
        // normalize → cache `dim`. See `PROGRESS.md` § "Embedder
        // forward".
        Err(anyhow!(
            "speaker-embedding ONNX inference not yet implemented — see PROGRESS.md"
        ))
    }
}

#[allow(dead_code)]
fn l2_normalize(v: &mut [f32]) {
    let n: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 1e-12 {
        for x in v {
            *x /= n;
        }
    }
}
