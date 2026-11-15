# exergame

A desktop and Android app that shows your camera feed and detects your body
pose in real time: 17 skeleton points (head, shoulders, elbows, hips, knees…)
are drawn over the video, with live stats on how fast the detection runs.

Built with [Tauri 2](https://v2.tauri.app) (Rust) + React. The pose model is
MoveNet SinglePose Lightning: on the desktop it runs through ONNX Runtime with
CUDA on NVIDIA GPUs (CPU fallback), on Android through LiteRT with the GPU
delegate. Both platforms ship the same model.

## Development

Everything (Rust, Node, GPU libraries, the Android SDK) is pre-installed in
the container, so the only host requirements are [Docker](https://docs.docker.com/get-docker/)
with the Compose plugin and, for GPU acceleration, the
[NVIDIA Container Toolkit](https://docs.nvidia.com/datacenter/cloud-native/container-toolkit/latest/install-guide.html).

```bash
docker compose up
```

This starts the vite dev server and `cargo tauri dev`: the app window opens
and hot-reloads as you edit. The first run also builds the container image.
(`make dev` does the same.)

## Production builds

```bash
make appimage       # desktop: single-file AppImage with the GPU runtime bundle
make android        # signed release APK + AAB
make android-debug  # debug APK for quick device testing
```

The artifacts land at:

- `src-tauri/Exergame/src-tauri/target/release/bundle/appimage/movenet-demo_0.1.0_amd64.AppImage`
- `src-tauri/Exergame/src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release.apk` (and the `.aab` next to it)

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
