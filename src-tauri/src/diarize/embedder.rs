//! Speaker-embedding ONNX backend (wespeaker-r34 / 3D-Speaker CAM++).
//!
//! Both supported models take 16 kHz mono f32 audio (typically a
//! 0.5–4 s slice of one voiced region) and emit a single fixed-size
//! L2-normalized embedding. Output dim is **256** for
//! wespeaker-voxceleb-resnet34-LM and **192** for CAM++ small.
//!
//! Both ONNX exports we ship take raw waveform input (the
//! spectrogram front-end is baked into the graph). Input / output
//! tensor names are sniffed at warm-up by suffix-match so a future
//! re-export with renamed nodes still works.

use anyhow::{anyhow, Context, Result};
use ndarray::Array2;
use ort::session::{builder::GraphOptimizationLevel, Session};
use ort::value::Tensor;

use crate::models::{model_dir, ModelKind};

/// Minimum slice length we'll embed. Below this, embedders' output
/// is dominated by their input-normalization pad and clusters poorly.
const MIN_EMBED_SAMPLES: usize = 16_000 / 2; // 0.5 s

pub struct Embedder {
    model_name: String,
    /// Lazily-loaded ORT session. `warm_up` populates it; `embed`
    /// borrows it mutably (ORT's `run` requires `&mut self` on the
    /// session because output buffers are owned by the session arena).
    session: Option<Session>,
    /// Sniffed at warm-up: name of the audio / waveform input tensor.
    input_name: String,
    /// Sniffed at warm-up: name of the embedding output tensor.
    output_name: String,
    /// Cached output dimensionality once we've seen one forward pass.
    /// Currently informational; callers don't read it yet, but having
    /// it on the struct keeps the clusterer's pre-allocation path
    /// straightforward to wire up later.
    #[allow(dead_code)]
    dim: Option<usize>,
}

impl Embedder {
    pub fn new(name: &str) -> Result<Self> {
        Ok(Self {
            model_name: name.to_string(),
            session: None,
            input_name: "feats".to_string(),
            output_name: "embedding".to_string(),
            dim: None,
        })
    }

    pub fn warm_up(&mut self) -> Result<()> {
        let path = model_dir(ModelKind::Diarize, &self.model_name)?.join("embedder.onnx");
        if !path.exists() {
            return Err(anyhow!("embedder ONNX missing: {}", path.display()));
        }
        let session = Session::builder()
            .map_err(|e| anyhow!("ort builder: {e}"))?
            .with_optimization_level(GraphOptimizationLevel::Level1)
            .map_err(|e| anyhow!("ort opt level: {e}"))?
            .with_intra_threads(intra_threads())
            .map_err(|e| anyhow!("ort threads: {e}"))?
            .commit_from_file(&path)
            .map_err(|e| anyhow!("loading {}: {e}", path.display()))
            .with_context(|| format!("warm_up embedder {}", self.model_name))?;

        // Sniff I/O names. wespeaker / 3D-Speaker exports vary; we
        // accept any input whose name looks like audio / feats /
        // waveform and any output whose name looks like an
        // embedding.
        for input in session.inputs() {
            let n = input.name().to_lowercase();
            if n.contains("feat") || n.contains("audio") || n.contains("wave") || n == "input" {
                self.input_name = input.name().to_string();
                break;
            }
        }
        for output in session.outputs() {
            let n = output.name().to_lowercase();
            if n.contains("embed") || n.contains("output") {
                self.output_name = output.name().to_string();
                break;
            }
        }
        self.session = Some(session);
        Ok(())
    }

    #[allow(dead_code)]
    pub fn dim(&self) -> Option<usize> {
        self.dim
    }

    /// Embed one voiced slice. Returns an L2-normalized vector, or
    /// an empty vector when the slice is too short to embed
    /// usefully. The clusterer treats an empty embedding as
    /// "cosine sim = 0 vs every centroid", opening a new cluster —
    /// but callers should filter on length first (see
    /// `MIN_EMBED_SAMPLES`).
    pub fn embed(&mut self, slice_pcm: &[f32]) -> Result<Vec<f32>> {
        if slice_pcm.len() < MIN_EMBED_SAMPLES {
            return Ok(Vec::new());
        }
        let session = self
            .session
            .as_mut()
            .ok_or_else(|| anyhow!("embedder not warmed up"))?;

        // Build the `[1, N]` f32 input. `Array2::from_shape_vec`
        // takes ownership and matches ORT's canonical 2-D shape for
        // raw-waveform embedder exports. We pass it to ORT by value
        // via `Tensor::from_array`; the alternative
        // `TensorRef::from_array_view(&arr)` has a generics-inference
        // gotcha (`T` doesn't propagate through the borrow) that
        // requires turbofish to disambiguate — by-value is cleaner.
        let input: Array2<f32> = Array2::from_shape_vec((1, slice_pcm.len()), slice_pcm.to_vec())
            .map_err(|e| anyhow!("shape input: {e}"))?;
        let tensor = Tensor::from_array(input).map_err(|e| anyhow!("ort tensor: {e}"))?;

        let outputs = session
            .run(ort::inputs![self.input_name.as_str() => tensor])
            .map_err(|e| anyhow!("ort run: {e}"))?;

        let value = outputs
            .get(self.output_name.as_str())
            .ok_or_else(|| anyhow!("embedder missing output {}", self.output_name))?;
        let view = value
            .try_extract_array::<f32>()
            .map_err(|e| anyhow!("ort extract: {e}"))?;

        // Accept either `[1, D]` or `[1, 1, D]`; collapse into a flat
        // `Vec<f32>`. The `iter()` walks elements in C-order, which
        // is correct for both shapes since the leading axes are 1.
        let shape = view.shape().to_vec();
        if shape.last().copied().unwrap_or(0) == 0 {
            return Err(anyhow!(
                "embedder produced zero-length output ({:?})",
                shape
            ));
        }
        let mut out: Vec<f32> = view.iter().copied().collect();
        l2_normalize(&mut out);
        self.dim = Some(out.len());
        Ok(out)
    }
}

fn l2_normalize(v: &mut [f32]) {
    let n: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 1e-12 {
        for x in v {
            *x /= n;
        }
    }
}

/// Threads to give the ORT CPU EP. Keep the embedder lean — it runs
/// alongside the ASR backend, so monopolising every core would just
/// trade one stage's latency for the other's.
fn intra_threads() -> usize {
    let n = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(2);
    n.saturating_sub(1).clamp(1, 2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn l2_normalize_unit_vector_unchanged() {
        let mut v = vec![1.0, 0.0, 0.0];
        l2_normalize(&mut v);
        assert_eq!(v, vec![1.0, 0.0, 0.0]);
    }

    #[test]
    fn l2_normalize_scales_magnitude_to_one() {
        let mut v = vec![3.0, 4.0]; // magnitude 5
        l2_normalize(&mut v);
        let mag: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((mag - 1.0).abs() < 1e-6, "expected mag=1, got {mag}");
        assert!((v[0] - 0.6).abs() < 1e-6);
        assert!((v[1] - 0.8).abs() < 1e-6);
    }

    #[test]
    fn l2_normalize_zero_vector_stays_zero() {
        let mut v = vec![0.0, 0.0, 0.0];
        l2_normalize(&mut v);
        assert_eq!(v, vec![0.0, 0.0, 0.0]);
    }
}
