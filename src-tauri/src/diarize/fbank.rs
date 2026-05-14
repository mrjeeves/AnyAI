//! Kaldi-compatible 80-dim log-mel filterbank for the speaker embedder.
//!
//! Both wespeaker-r34 and 3D-Speaker CAM++ consume `[B, T, 80]` log-mel
//! filterbank features rather than raw waveform — neither ONNX export
//! bakes the spectrogram front-end into the graph. This module is the
//! front-end: 16 kHz mono f32 audio in, `Array2<f32>` of shape `[T, 80]`
//! out, ready to be reshaped to `[1, T, 80]` for ORT.
//!
//! The numerics target Kaldi's `compute-fbank-feats` defaults, which is
//! what both wespeaker (via `torchaudio.compliance.kaldi.fbank`) and
//! 3D-Speaker (via kaldifeat) feed their models during training:
//!
//! | step              | setting                                          |
//! |-------------------|--------------------------------------------------|
//! | sample rate       | 16 000 Hz                                        |
//! | frame length      | 25 ms (400 samples)                              |
//! | frame shift       | 10 ms (160 samples)                              |
//! | FFT size          | next power of 2 ≥ frame length → 512             |
//! | dc removal        | subtract per-frame mean                          |
//! | pre-emphasis      | y[t] = x[t] − 0.97·x[t−1]                         |
//! | window            | Povey: `(0.5 − 0.5·cos(2π·n/(N−1)))^0.85`         |
//! | spectrum          | power (|FFT|²)                                   |
//! | mel scale         | Slaney: `mel(f) = 1127 · ln(1 + f/700)`           |
//! | mel bins          | 80, between 20 Hz and Nyquist (8000 Hz)           |
//! | log floor         | `max(filter_out, 1.19e-7).ln()` (kaldi epsilon)   |
//! | utterance CMN     | subtract per-bin mean across all frames          |
//!
//! The model accepts features as float32 in NTC order (`[batch, frames,
//! mel_bins]`); we leave the batch axis to the caller.

use ndarray::Array2;
use realfft::{RealFftPlanner, RealToComplex};
use std::sync::Arc;

pub const SAMPLE_RATE: u32 = 16_000;
pub const FRAME_LENGTH_SAMPLES: usize = 400; // 25 ms @ 16 kHz
pub const FRAME_SHIFT_SAMPLES: usize = 160; //  10 ms @ 16 kHz
pub const FFT_SIZE: usize = 512; // next pow-2 ≥ frame length
pub const NUM_MEL_BINS: usize = 80;
const PREEMPH: f32 = 0.97;
const LOW_FREQ_HZ: f32 = 20.0;
const HIGH_FREQ_HZ: f32 = (SAMPLE_RATE as f32) / 2.0;
/// Kaldi's energy floor before `ln`. Matches `compute-fbank-feats`'s
/// behaviour of clamping to `std::numeric_limits<float>::epsilon()`
/// rather than letting `ln(0)` slip through as `-inf`.
const LOG_EPS: f32 = f32::EPSILON;

/// Reusable fbank front-end. Constructing one allocates the mel
/// filterbank, the Povey window, and the realfft plan; `compute` is
/// pure forward work + a per-utterance pass for CMN.
pub struct Fbank {
    window: Vec<f32>,
    /// `[NUM_MEL_BINS][FFT_SIZE / 2 + 1]`, sparse-ish triangular filters.
    mel_filters: Vec<Vec<f32>>,
    fft: Arc<dyn RealToComplex<f32> + Send + Sync>,
}

impl Fbank {
    pub fn new() -> Self {
        let window = povey_window(FRAME_LENGTH_SAMPLES);
        let mel_filters = build_mel_filterbank(NUM_MEL_BINS, FFT_SIZE, SAMPLE_RATE as f32);
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(FFT_SIZE);
        Self {
            window,
            mel_filters,
            fft,
        }
    }

    /// Compute log-mel filterbank features for one utterance. Returns
    /// `[T, 80]` where `T = ⌊(len − frame_length) / frame_shift⌋ + 1`.
    /// Utterances shorter than one frame produce an empty array, which
    /// the embedder treats as "skip".
    pub fn compute(&self, waveform: &[f32]) -> Array2<f32> {
        if waveform.len() < FRAME_LENGTH_SAMPLES {
            return Array2::zeros((0, NUM_MEL_BINS));
        }
        let num_frames = (waveform.len() - FRAME_LENGTH_SAMPLES) / FRAME_SHIFT_SAMPLES + 1;
        let mut feats = Array2::<f32>::zeros((num_frames, NUM_MEL_BINS));

        // Reusable scratch buffers — one realfft plan, one frame at a time.
        let mut frame = self.fft.make_input_vec(); // length = FFT_SIZE
        let mut spec = self.fft.make_output_vec(); // length = FFT_SIZE/2 + 1

        for f in 0..num_frames {
            let start = f * FRAME_SHIFT_SAMPLES;
            // 1. Copy raw frame samples into the FFT-sized buffer
            //    (the trailing FFT_SIZE − FRAME_LENGTH_SAMPLES slots
            //    are zero-padding, set by the fill below).
            frame[..FRAME_LENGTH_SAMPLES]
                .copy_from_slice(&waveform[start..start + FRAME_LENGTH_SAMPLES]);
            frame[FRAME_LENGTH_SAMPLES..].fill(0.0);

            // 2. Remove DC (kaldi default `remove_dc_offset=true`).
            let mean: f32 =
                frame[..FRAME_LENGTH_SAMPLES].iter().sum::<f32>() / FRAME_LENGTH_SAMPLES as f32;
            for v in &mut frame[..FRAME_LENGTH_SAMPLES] {
                *v -= mean;
            }

            // 3. Pre-emphasis. Apply right-to-left so each tap reads the
            //    pre-emphasis-untouched previous sample. The leading
            //    sample's reference is itself (kaldi convention).
            for i in (1..FRAME_LENGTH_SAMPLES).rev() {
                frame[i] -= PREEMPH * frame[i - 1];
            }
            frame[0] -= PREEMPH * frame[0];

            // 4. Povey window.
            for (v, w) in frame
                .iter_mut()
                .take(FRAME_LENGTH_SAMPLES)
                .zip(self.window.iter())
            {
                *v *= w;
            }

            // 5. Real FFT → complex spectrum.
            //
            //    `process` may return an error if the buffer lengths
            //    drift from what the plan was made for, but they're
            //    fixed at compile time here, so the only failure path
            //    would be an internal realfft contract violation —
            //    fall back to skipping the frame rather than panicking.
            if self.fft.process(&mut frame, &mut spec).is_err() {
                continue;
            }

            // 6. Power spectrum then mel filterbank then log.
            for (m, filter) in self.mel_filters.iter().enumerate() {
                let mut energy: f32 = 0.0;
                for (k, &w) in filter.iter().enumerate() {
                    if w == 0.0 {
                        continue;
                    }
                    let re = spec[k].re;
                    let im = spec[k].im;
                    energy += w * (re * re + im * im);
                }
                feats[[f, m]] = energy.max(LOG_EPS).ln();
            }
        }

        // 7. Per-utterance CMN: subtract each mel bin's mean across all
        //    frames. wespeaker's recipe and 3D-Speaker's CAM++ both
        //    apply this at inference, so feeding the embedder un-CMN'd
        //    features lands it well outside its training distribution.
        if num_frames > 1 {
            for m in 0..NUM_MEL_BINS {
                let mut sum = 0.0;
                for t in 0..num_frames {
                    sum += feats[[t, m]];
                }
                let mean = sum / num_frames as f32;
                for t in 0..num_frames {
                    feats[[t, m]] -= mean;
                }
            }
        }

        feats
    }
}

impl Default for Fbank {
    fn default() -> Self {
        Self::new()
    }
}

/// `kaldi`'s Povey window: `(0.5 − 0.5·cos(2π·n/(N−1)))^0.85`. Slightly
/// flatter than a Hamming window — same family, raised to 0.85 instead
/// of left alone. The exponent is kaldi's default and the one wespeaker
/// /3D-Speaker train on, so it matters for the embedder to recognise its
/// input distribution.
fn povey_window(n: usize) -> Vec<f32> {
    let mut w = vec![0.0f32; n];
    let denom = (n - 1) as f32;
    for (i, slot) in w.iter_mut().enumerate() {
        let raw = 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / denom).cos();
        *slot = raw.powf(0.85);
    }
    w
}

fn hz_to_mel(hz: f32) -> f32 {
    1127.0 * (1.0 + hz / 700.0).ln()
}

fn mel_to_hz(mel: f32) -> f32 {
    700.0 * (mel / 1127.0).exp_m1()
}

/// Build a triangular mel filterbank matching kaldi's `compute-fbank-feats`.
///
/// Each filter `m` has its left edge at mel-point `m`, peak at `m+1`,
/// right edge at `m+2`. Weights are normalised so each filter peaks at
/// 1.0 (kaldi's `htk_compat=false` convention — no area normalisation).
fn build_mel_filterbank(num_bins: usize, fft_size: usize, sample_rate: f32) -> Vec<Vec<f32>> {
    let num_fft_bins = fft_size / 2 + 1;
    let mel_low = hz_to_mel(LOW_FREQ_HZ);
    let mel_high = hz_to_mel(HIGH_FREQ_HZ.min(sample_rate / 2.0));
    // num_bins + 2 evenly-spaced points in mel space.
    let mel_step = (mel_high - mel_low) / (num_bins + 1) as f32;
    let mel_points: Vec<f32> = (0..(num_bins + 2))
        .map(|i| mel_low + mel_step * i as f32)
        .collect();
    let hz_points: Vec<f32> = mel_points.iter().map(|&m| mel_to_hz(m)).collect();
    // Map to fractional FFT bins so triangles can be evaluated at each
    // integer bin index without quantising the edges.
    let bin_points: Vec<f32> = hz_points
        .iter()
        .map(|&hz| hz * fft_size as f32 / sample_rate)
        .collect();

    let mut filters = vec![vec![0.0f32; num_fft_bins]; num_bins];
    for (m, filter) in filters.iter_mut().enumerate() {
        let left = bin_points[m];
        let center = bin_points[m + 1];
        let right = bin_points[m + 2];
        for (k, slot) in filter.iter_mut().enumerate() {
            let kf = k as f32;
            let w = if kf < left || kf > right {
                0.0
            } else if kf <= center {
                (kf - left) / (center - left).max(1e-6)
            } else {
                (right - kf) / (right - center).max(1e-6)
            };
            if w > 0.0 {
                *slot = w;
            }
        }
    }
    filters
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fbank_shape_matches_frame_count() {
        // 1.0 s of audio → 1 + (16000 − 400) / 160 = 98 frames.
        let audio = vec![0.0f32; SAMPLE_RATE as usize];
        let feats = Fbank::new().compute(&audio);
        assert_eq!(feats.shape(), &[98, NUM_MEL_BINS]);
    }

    #[test]
    fn fbank_too_short_returns_empty() {
        let audio = vec![0.0f32; 200]; // < 400 sample frame length
        let feats = Fbank::new().compute(&audio);
        assert_eq!(feats.shape(), &[0, NUM_MEL_BINS]);
    }

    #[test]
    fn fbank_cmn_zeroes_dc() {
        // A pure DC offset becomes ~zero after pre-emphasis but the
        // log-energy floor leaves a constant per-bin value. CMN must
        // then subtract it, leaving features with per-bin mean ≈ 0.
        let audio = vec![0.5f32; SAMPLE_RATE as usize / 2]; // 0.5 s
        let feats = Fbank::new().compute(&audio);
        assert!(feats.shape()[0] > 0);
        for m in 0..NUM_MEL_BINS {
            let mean: f32 = feats.column(m).iter().sum::<f32>() / feats.shape()[0] as f32;
            assert!(mean.abs() < 1e-4, "mel bin {m}: mean={mean} (want ~0)");
        }
    }

    #[test]
    fn fbank_pulse_concentrates_energy_near_tone() {
        // 1 kHz tone in the *middle* of an otherwise silent utterance.
        // After CMN, each mel bin's mean across frames is dominated by
        // the silent surroundings, so the tone frame's deviation from
        // that mean spikes hardest in the bins covering ~1 kHz. A
        // constant tone over the whole utterance would zero out under
        // CMN (mean = value), making peak-bin selection arbitrary; the
        // pulse pattern is what isolates the tone's spectral signature.
        let f = 1000.0f32;
        let total = SAMPLE_RATE as usize; // 1.0 s
        let pulse_start = total / 2 - SAMPLE_RATE as usize / 20; // 0.45 s
        let pulse_end = total / 2 + SAMPLE_RATE as usize / 20; //   0.55 s
        let audio: Vec<f32> = (0..total)
            .map(|i| {
                if i >= pulse_start && i < pulse_end {
                    (2.0 * std::f32::consts::PI * f * i as f32 / SAMPLE_RATE as f32).sin()
                } else {
                    0.0
                }
            })
            .collect();
        let feats = Fbank::new().compute(&audio);
        // Frame near the middle of the pulse: roughly 0.5 s / 10 ms = 50.
        let mid_frame = feats.shape()[0] / 2;
        let row = feats.row(mid_frame);
        let (peak_bin, _) = row
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .unwrap();
        // mel(1 kHz) sits ~34% up the 20 Hz–8 kHz mel range, so for 80
        // bins the peak should land in the low 20s through low 30s.
        // Allow a generous window — pre-emphasis tilts the spectrum
        // upward, so we accept a bit of drift toward higher bins.
        assert!(
            (15..=45).contains(&peak_bin),
            "1 kHz pulse peaked at mel bin {peak_bin} (want ~20–35)"
        );
    }

    #[test]
    fn povey_window_endpoints_are_zero() {
        let w = povey_window(400);
        assert!(w[0].abs() < 1e-6);
        assert!(w[399].abs() < 1e-6);
        // Middle should be ~1.0 (0.5 − 0.5·cos(π))^0.85 = 1.0^0.85 = 1.0.
        assert!((w[200] - 1.0).abs() < 1e-3, "w[200]={}", w[200]);
    }

    #[test]
    fn mel_filterbank_triangles_sum_above_zero_in_band() {
        let fb = build_mel_filterbank(NUM_MEL_BINS, FFT_SIZE, SAMPLE_RATE as f32);
        assert_eq!(fb.len(), NUM_MEL_BINS);
        assert_eq!(fb[0].len(), FFT_SIZE / 2 + 1);
        // Every filter has at least one bin with weight > 0.
        for (m, filter) in fb.iter().enumerate() {
            let any_nonzero = filter.iter().any(|&w| w > 0.0);
            assert!(any_nonzero, "mel filter {m} is all zero");
        }
    }
}
