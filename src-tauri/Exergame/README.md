# exergame

A desktop app that shows your webcam feed and detects your body pose in real time:
17 skeleton points (head, shoulders, elbows, hips, knees…) are drawn over the video,
with live stats on how fast the detection runs.

Built with [Tauri 2](https://v2.tauri.app) (Rust) + React. The pose model is
MoveNet, executed by ONNX Runtime. If the computer has an NVIDIA GPU, the model
runs on it; otherwise it falls back to the CPU automatically.

> Linux only for now (the camera stack and windowing are Linux-specific).

## Quick start (Docker)

The easiest way — everything (Rust, Node, GPU libraries) is pre-installed in the
container. You need [Docker](https://docs.docker.com/get-docker/) with the
[Docker Compose](https://docs.docker.com/compose/) plugin.

```bash
docker compose build exergame   # once, downloads ~1 GB of components
docker compose up               # builds and opens the app window
```

If the computer has an NVIDIA GPU, also install the
[NVIDIA Container Toolkit](https://docs.nvidia.com/datacenter/cloud-native/container-toolkit/latest/install-guide.html)
so the container can use it (on Arch Linux:
`sudo pacman -S nvidia-container-toolkit && sudo nvidia-ctk runtime configure --runtime=docker && sudo systemctl restart docker`).

## What you'll see

- **Camera buttons** — pick a webcam; broken system devices are filtered out automatically.
- **Resolution buttons** — one list per camera, from the largest downwards.
- **Live preview** — green dots mark the detected body points. The **Mirror** button
  (on by default) flips the picture like a mirror.
- **Badges under the preview:**
  - frame resolution, e.g. `640×480`;
  - `Recognition: 10 ms · max 100 FPS` — how long one detection takes and the top
    speed it allows (averaged over the last frames);
  - `MoveNet: GPU` or `MoveNet: CPU` — where the model actually runs.

## Making an installable app (AppImage)

```bash
docker compose run --rm exergame cargo tauri build --bundles appimage
```

The result is a single self-contained file:

```
src-tauri/Exergame/src-tauri/target/release/bundle/appimage/exergame_0.1.0_amd64.AppImage
```

Copy it anywhere and run it. Without an NVIDIA driver it still works — just on CPU.

## GPU or CPU?

The badge tells you where the model runs. `GPU` is roughly 10× faster here.

- **Inside the container** — GPU is used automatically when the NVIDIA driver and
  the Container Toolkit are set up on the host.
- **On the host (without Docker)** — the app runs on CPU. There is an optional
  helper for GPU: put the CUDA libraries into `~/.local/lib/exergame-ort-gpu`
  and run `../run-gpu.sh` from the repository root (the script wires up the
  paths for you). Details on request — this is an advanced setup.
- `EXERGAME_FORCE_CPU=1` forces CPU even when a GPU is present.

## If something doesn't work

| Symptom | What to check |
| --- | --- |
| Container doesn't start, mentions "nvidia" | NVIDIA Container Toolkit is missing on the host (see Quick start) |
| Badge shows `MoveNet: CPU` but you expect GPU | Run `docker compose exec exergame nvidia-smi` — if it fails, the GPU isn't passed through; if it works, look for `ort` warnings in the app log |
| `No cameras found` | Is `/dev/video0` present on the host? Unplug/replug the webcam, check `ls /dev/video*` |
| App window doesn't appear on a Wayland desktop | The app disables the problematic WebKit renderer itself; try `WAYLAND_DISPLAY`/`DISPLAY` env as in `docker-compose.yaml` |

## Under the hood (for developers)

- **Rust side** — `src-tauri/src/camera.rs` grabs frames with `nokhwa`, runs the
  model in the capture loop and ships each frame plus the pose JSON to the UI;
  `src-tauri/src/movenet.rs` owns the ONNX Runtime session and picks the fastest
  available execution provider (CUDA → ROCm → CPU).
- **Frontend** — `src/App.tsx` draws frames and keypoints on a canvas and renders
  the badges (Bulma styling).
- **Build** — `pnpm tauri dev` / `pnpm tauri build` work on a machine with Rust
  and Node installed; inside the container use the `cargo tauri build` commands
  above. `build.rs` collects the GPU libraries for the AppImage into the cargo
  `target/` directory, so the repository stays clean.
- **Model** — MoveNet SinglePose Thunder, 256×256 input, stored at
  `src-tauri/movenet/model.onnx` and embedded into the binary.
