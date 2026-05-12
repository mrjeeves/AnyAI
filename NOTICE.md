# NOTICE

MyOwnLLM bundles open-source models and libraries for local
transcription and speaker diarization. The list below covers
**model weights** (downloaded on demand from public mirrors) and
**Rust / JS dependencies** with permissive licenses that require
attribution.

This file is informational. The project itself is MIT-licensed; see
`LICENSE` for the project license.

## Model weights

### Moonshine Small (`moonshine-small-q8`)

- **Source:** [`UsefulSensors/moonshine`](https://huggingface.co/UsefulSensors/moonshine) (HuggingFace)
- **License:** MIT
- **Used for:** English speech-to-text on Pi-class / low-end hardware.
- **Citation:** Useful Sensors. _Moonshine: Speech recognition for live transcription and voice commands._ 2024.
  <https://arxiv.org/abs/2410.15608>

### Parakeet TDT 0.6B v3 (`parakeet-tdt-0.6b-v3-int8`)

- **Source:** Community ONNX export at [`istupakov/parakeet-tdt-0.6b-v3-onnx`](https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx) of the original NVIDIA
  [`nvidia/parakeet-tdt-0.6b-v3`](https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3) weights.
- **License:** Apache-2.0 (weights); the ONNX converter inherits the same.
- **Used for:** 25-language speech-to-text on Apple Silicon / x86 (with or without a discrete GPU).
- **Citation:** NVIDIA NeMo Team. _Parakeet TDT: Token-and-Duration Transducer for Streaming ASR._ 2024–2025.

### pyannote-segmentation-3.0 (`pyannote-seg-3.0`)

- **Source:** Ungated ONNX mirror at
  [`csukuangfj/sherpa-onnx-pyannote-segmentation-3-0`](https://huggingface.co/csukuangfj/sherpa-onnx-pyannote-segmentation-3-0) of
  [`pyannote/segmentation-3.0`](https://huggingface.co/pyannote/segmentation-3.0).
- **License:** CC-BY-SA-4.0 on the model weights.
- **Used for:** Voice-activity + speaker-change detection in the diarize pipeline.
- **Citation:** Bredin, H. _pyannote.audio 2.1 speaker diarization pipeline: principle, benchmark, and recipe._ Interspeech 2023.
  <https://www.isca-archive.org/interspeech_2023/bredin23_interspeech.pdf>

The CC-BY-SA-4.0 weights are used at runtime; per common practice, the
license is widely interpreted as **not** applying to model *outputs*
(text transcripts, speaker turn timings). If you redistribute the
weights themselves, the share-alike clause applies.

### wespeaker-voxceleb-resnet34-LM (`wespeaker-r34`)

- **Source:** Ungated ONNX at
  [`csukuangfj/sherpa-onnx-3d-speaker`](https://huggingface.co/csukuangfj/sherpa-onnx-3d-speaker), originally from the
  [WeSpeaker](https://github.com/wenet-e2e/wespeaker) project.
- **License:** Apache-2.0
- **Used for:** Speaker embeddings (256-d) on capable hardware.
- **Citation:** Wang, H. et al. _Wespeaker: A research and production oriented speaker embedding learning toolkit._ ICASSP 2023.

### 3D-Speaker CAM++ small (`campp-small`)

- **Source:** Ungated ONNX at
  [`csukuangfj/sherpa-onnx-3d-speaker`](https://huggingface.co/csukuangfj/sherpa-onnx-3d-speaker), originally from the
  [3D-Speaker](https://github.com/alibaba-damo-academy/3D-Speaker) toolkit.
- **License:** Apache-2.0
- **Used for:** Speaker embeddings (192-d) on Pi-class hardware — smaller and ~4× faster than wespeaker-r34 with modestly lower cluster purity.

## Inference runtime

### onnxruntime (loaded dynamically via `ort` 2.x)

- **Source:** [`microsoft/onnxruntime`](https://github.com/microsoft/onnxruntime)
- **License:** MIT
- **Distribution:** the platform-appropriate dynamic library is
  bundled next to the MyOwnLLM binary and loaded at session start.

### `ort` Rust crate

- **Source:** [`pykeio/ort`](https://github.com/pykeio/ort)
- **License:** MIT OR Apache-2.0

### `tokenizers` Rust crate

- **Source:** [`huggingface/tokenizers`](https://github.com/huggingface/tokenizers)
- **License:** Apache-2.0
- **Used for:** Moonshine's BPE tokenizer (`tokenizer.json`).

### `ndarray` Rust crate

- **Source:** [`rust-ndarray/ndarray`](https://github.com/rust-ndarray/ndarray)
- **License:** MIT OR Apache-2.0
