# exergame

A Tauri 2 + React + TypeScript desktop app that streams a webcam to a canvas and runs real-time
human pose estimation (MoveNet SinglePose Thunder via ONNX Runtime) with on-screen telemetry.

## Features

- Camera list with per-camera resolution picker (queried from the device itself).
- Live preview with pose keypoints drawn over the frame.
- Mirror toggle (on by default) that flips the preview together with the skeleton.
- Telemetry badges under the preview: frame resolution, smoothed recognition time
  (window of 10), smoothed maximum FPS (window of 20) and the active execution
  provider (`MoveNet: GPU` / `MoveNet: CPU`).
- GPU inference when CUDA is available, automatic fallback to CPU otherwise.

## How it works

- **Capture (`src-tauri/src/camera.rs`)** — `nokhwa` opens the camera in a dedicated thread,
  decodes frames to RGB, runs pose recognition in the same loop and sends a combined payload
  to the frontend over a Tauri `Channel`.
- **Frame payload** — 12-byte big-endian header (width, height, JSON tail length),
  followed by RGB pixels, followed by a JSON tail with
  `keypoints`, `inferenceMs`, `preprocessMs`, `maxFps` and `provider`.
- **Inference (`src-tauri/src/movenet.rs`)** — the model (`src-tauri/movenet/model.onnx`,
  embedded with `include_bytes!`) runs through `ort` 2.0.0-rc.15 (`load-dynamic` feature).
  Preprocessing samples the 256×256 letterboxed input directly from the RGB buffer
  (`image` 0.24 `imageops::resize` was measured at ~55 ms per frame, the custom path is ~4 ms).
- **Frontend (`src/App.tsx`)** — a React class component renders frames on a canvas via
  `requestAnimationFrame`, draws keypoints with `score >= 0.3` and shows the smoothed stats.
  Styling uses Bulma (`react-bulma-components`).

## Tauri commands

| Command | Purpose |
| --- | --- |
| `list_cameras` | Enumerate cameras (`nokhwa::query`) |
| `list_resolutions` | Supported `WIDTHxHEIGHT` strings for a camera |
| `start_rust_camera` | Start streaming + recognition; args: `cameraIndex`, `resolution` (optional), `onFrame` channel |
| `stop_rust_camera` | Stop the stream and release the device |

## Running locally

Requirements: Rust (stable), Node.js + pnpm.

```bash
pnpm install
pnpm tauri dev
```

## Running in Docker

The compose file starts two services: `frontend` (Vite dev server) and `exergame`
(the desktop shell built by `cargo tauri dev`). The `exergame` service mounts the
app at `/srv`, passes `/dev/video0`, X11/Wayland sockets and the NVIDIA GPU through.

First build (downloads ~1 GB: ONNX Runtime GPU build, cuDNN, CUDA 13.1 runtime libs):

```bash
docker compose build exergame
docker compose up
```

Verify the GPU is visible inside the container:

```bash
docker compose exec exergame nvidia-smi
```

### Production build

`build-prod.sh` (at the repository root) automates the two-step release build:
the frontend is compiled in the `frontend` container, the release binary in the
`exergame` container, which already contains the GPU ONNX Runtime and the CUDA stack.

```bash
./build-prod.sh            # release binary only
./build-prod.sh --bundle   # also build installable packages (deb/rpm/AppImage)
```

Artifacts land in `src-tauri/Exergame/src-tauri/target/release/` (`exergame`,
plus `bundle/` when `--bundle` is passed). Run the binary inside the GPU container:

```bash
docker compose exec exergame /srv/src-tauri/target/release/exergame
```

### GPU on the host (optional)

The host has the NVIDIA driver but no CUDA stack, so the release binary runs
MoveNet on CPU. To run it on GPU, a CUDA 13 + cuDNN 9 stack and the GPU build of
ONNX Runtime are expected in `~/.local/lib/exergame-ort-gpu` (assembled from the
official ONNX Runtime `gpu_cuda13` release and NVIDIA's `libcudnn9-cuda-13` deb —
the same versions the container uses). Then:

```bash
./run-gpu.sh
```

The wrapper sets `ORT_DYLIB_PATH` and `LD_LIBRARY_PATH` to that directory and
starts the release binary; the badge should show `MoveNet: GPU`.

A system-wide alternative is `sudo pacman -S cuda cudnn` (Arch), as long as the
packaged CUDA major matches the ONNX Runtime build (13 for `gpu_cuda13`).

### NVIDIA Container Toolkit

The host needs `nvidia-container-toolkit` (Arch: `sudo pacman -S nvidia-container-toolkit`,
then `sudo nvidia-ctk runtime configure --runtime=docker && sudo systemctl restart docker`).
Without it the GPU reservation in `docker-compose.yaml` fails to start.

## GPU / CPU execution

`ort` is built with `load-dynamic`, so the app looks for `libonnxruntime.so` in this order:

1. `ORT_DYLIB_PATH` environment variable,
2. next to the executable,
3. `/opt/onnxruntime/lib` and `/opt/onnxruntime` (GPU build installed by the Dockerfile),
4. `src-tauri/ort/` (CPU build kept in the repository as a fallback),
5. system library search path.

When the CUDA execution provider fails to register (no GPU, missing CUDA/cuDNN libs),
`ort` logs a warning and falls back to CPU; the badge shows which provider is active.
Set `EXERGAME_FORCE_CPU=1` to skip the CUDA attempt entirely.

The CUDA stack comes from Ubuntu 26.04 `multiverse` (`cuda-cudart-13-1`, `libcublas-13-1`,
`libcurand-13-1`) plus `libcudnn9-cuda-13` from NVIDIA's repository. CUDA 12.4 from Ubuntu
is not enough — ORT's CUDA EP requires `cudaLibraryGetKernel`, which only exists in newer
CUDA builds; CUDA 13 still supports Turing (sm_75, e.g. GTX 1650 SUPER).

## Diagnostics

- The capture thread logs a timing breakdown every ~60 frames to stdout:
  `pose: 10 ms total (preprocess 4 ms, inference 6 ms), provider GPU, max fps 101.3, frame 320x240`.
- `ort` warnings/errors (e.g. CUDA EP registration failures) are re-emitted to stdout
  via a tracing subscriber.
- The frontend logs `list_resolutions` results and camera command failures
  through `tauri-plugin-log`.

## Performance notes

- `[profile.dev.package."*"] opt-level = 3` optimizes dependencies in dev builds;
  without it frame decoding/resize dominate the frame budget.
- GPU inference of MoveNet Thunder takes ~6 ms on a GTX 1650 SUPER; the real stream
  FPS is capped by the camera frame rate, not by recognition.
- For an even faster model, MoveNet Lightning (~192×192 input) can be swapped in —
  adjust `MOVENET_INPUT_SIZE` accordingly.
