# Movenet FPS Tester

<img height="300" alt="image" src="https://github.com/user-attachments/assets/eae8596f-2e2b-40dd-9bac-47be875c54b9" />
<img height="300" alt="image" src="https://github.com/user-attachments/assets/27d008dd-fcac-4321-aff9-86e03a0aef9b" />



A desktop and Android app that shows your camera feed and detects your body
pose in real time: 17 skeleton points (head, shoulders, elbows, hips, knees…)
are drawn over the video, with live stats on how fast the detection runs.

Built with [Tauri 2](https://v2.tauri.app) (Rust) + React. The pose model is
MoveNet SinglePose Lightning: on the desktop it runs through ONNX Runtime with
CUDA on NVIDIA GPUs (CPU fallback), on Android through LiteRT with the GPU
delegate. Both platforms ship the same model.

## Features

- Real-time body pose detection from a live camera feed
- 17 skeleton keypoints (head, shoulders, elbows, hips, knees…) drawn over the video
- Live performance stats: detection speed and which provider runs the model (`MoveNet: GPU` / `MoveNet: CPU`)
- MoveNet SinglePose Lightning model, the same one on every platform
- GPU acceleration: CUDA on desktop NVIDIA GPUs, ROCm on AMD, LiteRT GPU delegate (OpenCL) on Android, with automatic CPU fallback
- Cross-platform: one codebase for desktop (Linux AppImage) and Android, built with Tauri 2 (Rust) + React

## Models

The app uses a single pose model: [MoveNet SinglePose Lightning](https://www.tensorflow.org/hub/tutorials/movenet)
(Google, 17 COCO keypoints, 192×192 RGB input). The same model ships on every
platform in the format native to that runtime, embedded into the app so
nothing is downloaded at runtime:

| Platform | File | Format | Runtime |
| --- | --- | --- | --- |
| Desktop | `application/src-tauri/movenet/model.onnx` | ONNX | ONNX Runtime (CUDA / ROCm / CPU) |
| Android | `application/src-tauri/plugins/camera/android/src/main/assets/movenet-lightning.tflite` | TensorFlow Lite | LiteRT with the GPU delegate |

The desktop model is compiled into the binary (`include_bytes!`), the Android
one is packed as an uncompressed asset so LiteRT can memory-map it.

## Development

```bash
docker compose up 
```

- `frontend` — the Vite dev server on `http://localhost:9999`;
- `movenet` — the Rust application, the window opens on the host desktop.


## Production builds

```bash
make appimage       # desktop: single-file AppImage with the GPU runtime bundle
make android        # signed release APK + AAB
make android-debug  # debug APK for quick device testing
```

The artifacts land at:

- `application/src-tauri/target/release/bundle/appimage/movenet-demo_0.1.0_amd64.AppImage`
- `application/src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release.apk` (and the `.aab` next to it)

## GPU support

| Platform | Hardware | Provider | Notes |
| --- | --- | --- | --- |
| Desktop | NVIDIA | CUDA | automatic with a recent driver; the runtime is bundled into the AppImage |
| Desktop | AMD | ROCm | needs an ONNX Runtime ROCm build at `/opt/onnxruntime-rocm` (container image) |
| Desktop | Intel iGPU | — | not yet; possible via the OpenVINO execution provider (not wired up) |
| Desktop | no GPU | CPU | automatic fallback, works everywhere |
| Android | Adreno / Mali / Immortalis / PowerVR | LiteRT GPU delegate (OpenCL) | automatic CPU fallback when the delegate cannot start |

`EXERGAME_FORCE_CPU=1` forces the CPU provider even when a GPU is present.
The badge under the preview always shows which provider actually runs the
model (`MoveNet: GPU` / `MoveNet: CPU`).

## Download

Grab the ready-to-run builds from the
[latest release](https://github.com/DegnerAlex/MovenetDemo/releases/latest) —
no installation needed, just download, `chmod +x` and run:

```bash
chmod +x movenet-demo_*_amd64.AppImage
./movenet-demo_*_amd64.AppImage
```

Latest release:

- `movenet-demo_<version>_amd64.AppImage` — desktop (Linux, x86_64), bundles the ONNX Runtime GPU libraries, so only the NVIDIA driver is needed on the target machine (CUDA path); on CPU-only systems it falls back automatically, on AMD the ROCm build is used when available;
- `app-universal-release.apk` — signed Android release APK, works on `arm64-v8a` / `armeabi-v7a` devices with any OpenCL-capable GPU (Adreno / Mali / Immortalis / PowerVR), automatic CPU fallback when the GPU delegate cannot start.

Every release page lists the checksums and the changelog (`--generate-notes`
from the commit history). See the [GPU support](#gpu-support) table above for
the current platform support details.

## Libraries

Frontend (React / TypeScript):

- [React](https://react.dev) 19 + ReactDOM — UI
- [react-bulma-components](https://github.com/cztomsik/react-bulma-components) / [Bulma](https://bulma.io) — UI components and styling
- [@tauri-apps/api](https://v2.tauri.app/reference/javascript/api/) — IPC bridge to the Rust core
- [@tauri-apps/plugin-log](https://v2.tauri.app/plugin/logging/) — logging from the webview

Rust (desktop core and camera plugin):

- [Tauri](https://v2.tauri.app) 2 — app framework, windowing, IPC
- [nokhwa](https://github.com/l1npengpeng/nokhwa) — camera capture on desktop (native backend)
- [ort](https://github.com/pykeio/ort) — ONNX Runtime bindings for MoveNet inference (CUDA / ROCm / CPU)
- [libloading](https://github.com/nagisa/rust_libloading) — preloads CUDA/cuDNN shared libraries before ONNX Runtime init
- [serde](https://serde.rs) / serde_json — serialization for IPC and config
- [tracing](https://tracing.rs) / tracing-subscriber — structured logging, tracks which execution provider actually runs
- [libc](https://github.com/rust-lang/libc) — raw V4L2 ioctls for the virtual-camera frame-rate workaround
- [jpeg-decoder](https://github.com/image-rs/jpeg-decoder) — JPEG frame decoding on Android
- [base64](https://github.com/marshallpierce/rust-base64) — frame transport in the camera plugin

Android (camera plugin):

- [Jetpack CameraX](https://developer.android.com/camerax) — camera capture and lifecycle
- [LiteRT (TensorFlow Lite)](https://ai.google.dev/edge/litert) + GPU delegate — MoveNet inference on mobile GPUs via OpenCL
- AndroidX core / appcompat

Build tooling: [Vite](https://vite.dev), [TypeScript](https://www.typescriptlang.org), Cargo, Gradle with Kotlin.
