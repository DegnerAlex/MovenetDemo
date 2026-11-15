use ort::session::Session;
use ort::value::Tensor;
use serde::Serialize;
use std::sync::{Mutex, OnceLock};
use tracing_subscriber::{Layer as _, layer::SubscriberExt};

// MoveNet SinglePose Thunder, embedded into the binary so it does not depend on the working directory.
const MOVENET_MODEL: &[u8] = include_bytes!("../movenet/model.onnx");
const MOVENET_INPUT_SIZE: u32 = 256;

/// CPU build of ONNX Runtime, embedded into the binary so that raw (non-bundled)
/// desktop builds stay self-sufficient even when no library exists next to the
/// executable. On Android the library ships as a JNI lib instead (see
/// plugins/camera/build.rs), so nothing is embedded there.
#[cfg(not(target_os = "android"))]
const ORT_CPU_LIB: &[u8] = include_bytes!("../ort/libonnxruntime.so");

const KEYPOINT_NAMES: [&str; 17] = [
    "nose",
    "leftEye",
    "rightEye",
    "leftEar",
    "rightEar",
    "leftShoulder",
    "rightShoulder",
    "leftElbow",
    "rightElbow",
    "leftWrist",
    "rightWrist",
    "leftHip",
    "rightHip",
    "leftKnee",
    "rightKnee",
    "leftAnkle",
    "rightAnkle",
];

static POSE_SESSION: OnceLock<Result<Mutex<Session>, String>> = OnceLock::new();

/// Whether a GPU-family execution provider (CUDA/ROCm/MIGraphX) actually
/// registered with the loaded ONNX Runtime; "CPU" otherwise.
static GPU_EP_REGISTERED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static PROVIDER_PROBE_INSTALLED: OnceLock<()> = OnceLock::new();

/// Tauri resource directory (set from `lib.rs` setup). In AppImage builds it
/// contains the bundled CPU and GPU ONNX Runtime libraries.
static RESOURCE_DIR: OnceLock<std::path::PathBuf> = OnceLock::new();

pub fn set_resource_dir(dir: std::path::PathBuf) {
    let _ = RESOURCE_DIR.set(dir);
}

/// Tracing layer that captures ort events about execution provider
/// registration: any successfully registered GPU-family EP (CUDA, ROCm,
/// MIGraphX) flips GPU_EP_REGISTERED; failures only produce warnings.
struct ProviderProbe;

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for ProviderProbe {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut visitor = MessageVisitor(String::new());
        event.record(&mut visitor);
        let message = visitor.0;

        if (message.contains("Successfully registered `CUDAExecutionProvider`")
            || message.contains("Successfully registered `ROCmExecutionProvider`")
            || message.contains("Successfully registered `MIGraphXExecutionProvider`"))
            && !GPU_EP_REGISTERED.load(std::sync::atomic::Ordering::Relaxed)
        {
            GPU_EP_REGISTERED.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

struct MessageVisitor(String);

impl tracing::field::Visit for MessageVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            use std::fmt::Write as _;
            let _ = write!(self.0, "{value:?}");
        }
    }
}

/// Preloads shared libraries from `dir` with RTLD_GLOBAL and leaks the
/// handles: the libraries stay loaded for the whole process lifetime, which
/// lets the CUDA provider's dependencies (and ORT's libcudnn.so dlopen)
/// resolve by soname afterwards.
fn preload_libraries(dir: &std::path::Path, names: &[&str]) {
    for name in names {
        let path = dir.join(name);
        if !path.exists() {
            continue;
        }

        // Safety: dlopen of a regular library file, single-threaded context
        // (session initialization), the handle is intentionally leaked.
        unsafe {
            if let Ok(lib) = libloading::os::unix::Library::open(
                Some(&path),
                libloading::os::unix::RTLD_NOW | libloading::os::unix::RTLD_GLOBAL,
            ) {
                std::mem::forget(lib);
            }
        }
    }
}

/// Extracts the embedded CPU build into a per-machine temp cache and returns
/// its path. The file is written atomically (temp + rename) and reused when
/// its size matches the embedded copy.
#[cfg(not(target_os = "android"))]
fn ensure_embedded_ort() -> Option<std::path::PathBuf> {
    let dir = std::env::temp_dir().join("exergame-ort");
    let path = dir.join("libonnxruntime.so");

    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() == ORT_CPU_LIB.len() as u64 {
            return Some(path);
        }
    }

    std::fs::create_dir_all(&dir).ok()?;
    let tmp = dir.join("libonnxruntime.so.tmp");
    std::fs::write(&tmp, ORT_CPU_LIB).ok()?;
    // A stale file from another run may block the rename; retry once.
    std::fs::rename(&tmp, &path)
        .or_else(|_| {
            std::fs::remove_file(&path).ok();
            std::fs::rename(&tmp, &path)
        })
        .ok()?;

    Some(path)
}

/// Installs the global tracing subscriber once: ort errors and warnings go to
/// stdout, while the CUDA usage fact lands in POSE_PROVIDER.
fn install_provider_probe() {
    PROVIDER_PROBE_INSTALLED.get_or_init(|| {
        let ort_errors = tracing_subscriber::fmt::layer()
            .with_target(false)
            .with_filter(
                tracing_subscriber::filter::Targets::new()
                    .with_target("ort", tracing::Level::WARN),
            );

        let _ = tracing::subscriber::set_global_default(
            tracing_subscriber::registry()
                .with(ort_errors)
                .with(ProviderProbe),
        );
    });
}

/// Loads ONNX Runtime (`load-dynamic` build) and picks execution providers
/// based on what the chosen build actually ships:
///
/// 1. `ORT_DYLIB_PATH` environment variable (explicit override),
/// 2. `/opt/onnxruntime` (CUDA build from the container image) when an NVIDIA
///    driver is present,
/// 3. `/opt/onnxruntime-rocm` (ROCm build) when a ROCm SDK is present,
/// 4. the GPU build bundled into AppImage resources (when NVIDIA/ROCm present),
/// 5. the CPU build bundled into AppImage resources,
/// 6. the CPU build embedded into the binary (extracted to a temp cache),
/// 7. the system library search path.
///
/// GPU execution providers (CUDA / ROCm / MIGraphX) are only registered when
/// the matching provider library sits next to the chosen ONNX Runtime, so CPU
/// builds never produce registration warnings. EXERGAME_FORCE_CPU skips all
/// GPU providers.
fn ort_environment() -> Result<ort::environment::Environment, String> {
    let dylib_name = format!(
        "{}onnxruntime{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    );

    let nvidia_gpu = std::path::Path::new("/proc/driver/nvidia/version").exists();
    let rocm_sdk = std::path::Path::new("/opt/rocm").exists();

    let mut candidates = Vec::new();

    if let Ok(path) = std::env::var("ORT_DYLIB_PATH") {
        if !path.is_empty() {
            candidates.push(std::path::PathBuf::from(path));
        }
    }

    if nvidia_gpu {
        // CUDA build installed into the container image. Checked before the
        // executable directory so the GPU build is not shadowed by a CPU copy
        // placed next to the binary for portable runs.
        candidates.push(std::path::Path::new("/opt/onnxruntime/lib").join(&dylib_name));
        candidates.push(std::path::Path::new("/opt/onnxruntime").join(&dylib_name));
    }

    if rocm_sdk {
        // ROCm build (built from source) for AMD GPUs.
        candidates.push(std::path::Path::new("/opt/onnxruntime-rocm/lib").join(&dylib_name));
        candidates.push(std::path::Path::new("/opt/onnxruntime-rocm").join(&dylib_name));
    }

    // Libraries bundled into AppImage builds (Tauri resource directory).
    // Desktop only: those libraries are x86_64 builds. On Android the ONNX
    // Runtime ships as a JNI library (arm64) instead.
    #[cfg(not(target_os = "android"))]
    if let Some(resource_dir) = RESOURCE_DIR.get() {
        if nvidia_gpu || rocm_sdk {
            // GPU build with the CUDA execution provider.
            candidates.push(resource_dir.join("ort-gpu-bundle/libonnxruntime-gpu.so"));
        }

        // CPU build as the last resource fallback.
        candidates.push(resource_dir.join("ort/libonnxruntime.so"));
    }

    let mut chosen = candidates.iter().find(|path| path.exists()).cloned();

    #[cfg(target_os = "android")]
    if chosen.is_none() {
        // Android: the ONNX Runtime build ships as a JNI library inside the
        // APK; dlopen by bare name resolves it from the app's native lib dir.
        chosen = Some(std::path::PathBuf::from("libonnxruntime.so"));
    }

    // Last resort (desktop): the CPU build embedded into the binary (extracted
    // to a temp cache once per boot). Keeps raw binaries self-sufficient
    // without any files next to the executable.
    #[cfg(not(target_os = "android"))]
    if chosen.is_none() {
        if let Some(path) = ensure_embedded_ort() {
            candidates.push(path.clone());
            chosen = Some(path);
        }
    }

    // In bundled desktop builds the CUDA/cuDNN runtime libraries sit next to
    // the ONNX Runtime library, but they are not on the loader search path
    // (LD_LIBRARY_PATH cannot be changed after the process started). Preload
    // them with RTLD_GLOBAL so that the CUDA provider's dependencies resolve
    // by soname afterwards. Android never preloads: its ORT build is CPU-only.
    #[cfg(not(target_os = "android"))]
    if let Some(dir) = chosen.as_ref().and_then(|path| path.parent()) {
        if dir.join("libcudart.so.13").exists() {
            preload_libraries(
                dir,
                &[
                    "libcudart.so.13",
                    "libcublas.so.13",
                    "libcublasLt.so.13",
                    "libcurand.so.10",
                    "libcudnn.so",
                ],
            );
        }
    }

    let (mut environment, ort_dir) = if let Some(path) = chosen {
        let environment = ort::init_from(&path)
            .map_err(|err| format!("Failed to load {}: {err}", path.display()))?
            .with_name("exergame");
        let dir = path
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_default();
        (environment, dir)
    } else {
        (
            ort::init().with_name("exergame"),
            std::path::PathBuf::new(),
        )
    };

    // Register only the providers the loaded build actually ships: the CUDA
    // provider lives in libonnxruntime_providers_cuda.so, the ROCm provider in
    // libonnxruntime_providers_rocm.so, next to the main library. Desktop
    // only: Android builds are CPU-only (the shipped ORT has no GPU providers
    // and none of the provider libraries are packaged).
    #[cfg(not(target_os = "android"))]
    {
        let mut providers: Vec<ort::ep::ExecutionProviderDispatch> = Vec::new();
        let force_cpu =
            std::env::var("EXERGAME_FORCE_CPU").is_ok_and(|value| !value.is_empty());

        if !force_cpu {
            if ort_dir.join("libonnxruntime_providers_cuda.so").exists() {
                providers.push(ort::ep::CUDA::default().with_device_id(0).build());
            }

            if ort_dir.join("libonnxruntime_providers_rocm.so").exists() {
                providers.push(ort::ep::ROCm::default().with_device_id(0).build());
            }

            if ort_dir
                .join("libonnxruntime_providers_migraphx.so")
                .exists()
            {
                providers.push(ort::ep::MIGraphX::default().build());
            }
        }

        environment = if providers.is_empty() {
            environment
        } else {
            environment.with_execution_providers(providers)
        };
    }

    #[cfg(target_os = "android")]
    {
        // Android ORT is CPU-only; EP registration is intentionally skipped so
        // no CUDA/ROCm provider attempts (or warnings) happen on the device.
        let _ = &ort_dir;
    }

    environment
        .build()
        .map_err(|err| err.to_string())
}

fn pose_session() -> Result<&'static Mutex<Session>, String> {
    POSE_SESSION
        .get_or_init(|| -> Result<Mutex<Session>, String> {
            install_provider_probe();
            let environment = ort_environment()?;

            let mut builder =
                Session::builder(&environment).map_err(|err| err.to_string())?;
            let session = builder
                .commit_from_memory(MOVENET_MODEL)
                .map_err(|err| err.to_string())?;

            Ok(Mutex::new(session))
        })
        .as_ref()
        .map_err(|err| err.clone())
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Keypoint {
    pub name: &'static str,
    pub x: f32,
    pub y: f32,
    pub score: f32,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PoseEstimate {
    pub width: u32,
    pub height: u32,
    pub keypoints: Vec<Keypoint>,
    /// Total recognition time in milliseconds (preprocessing + MoveNet inference).
    pub inference_ms: f64,
    /// Milliseconds of that spent on preprocessing (frame resize/letterbox).
    pub preprocess_ms: f64,
    /// Maximum possible recognition FPS: 1000 / inference_ms.
    pub max_fps: f64,
    /// What ran the model: "GPU" (CUDA/ROCm/MIGraphX EP) or "CPU"; empty if
    /// recognition failed.
    pub provider: &'static str,
}

/// Fits a frame into the MoveNet input: 256×256×3 (int32), keeping the aspect
/// ratio and padding the borders with zeros.
/// Nearest sampling straight from the buffer: `imageops::resize` in image 0.24
/// costs tens of milliseconds per frame and killed the FPS.
fn preprocess_frame(
    width: u32,
    height: u32,
    rgb: &[u8],
) -> Result<(Vec<i32>, f32, f32, u32, u32), String> {
    if width == 0 || height == 0 || rgb.len() < width as usize * height as usize * 3 {
        return Err("Failed to read RGB frame".to_string());
    }

    let side = MOVENET_INPUT_SIZE as usize;
    let scale = (MOVENET_INPUT_SIZE as f32 / width as f32)
        .min(MOVENET_INPUT_SIZE as f32 / height as f32);
    let scaled_width = ((width as f32 * scale).round() as u32).clamp(1, MOVENET_INPUT_SIZE);
    let scaled_height = ((height as f32 * scale).round() as u32).clamp(1, MOVENET_INPUT_SIZE);
    let pad_x = (MOVENET_INPUT_SIZE - scaled_width) / 2;
    let pad_y = (MOVENET_INPUT_SIZE - scaled_height) / 2;

    let mut input = vec![0_i32; side * side * 3];

    let src_width = width as usize;
    for out_y in 0..scaled_height as usize {
        // Nearest source row/column for the output pixel.
        let src_y = (((out_y as f32 + 0.5) / scale).floor() as usize).min(height as usize - 1);

        for out_x in 0..scaled_width as usize {
            let src_x = (((out_x as f32 + 0.5) / scale).floor() as usize).min(src_width - 1);

            let src = (src_y * src_width + src_x) * 3;
            let offset = ((out_y + pad_y as usize) * side + (out_x + pad_x as usize)) * 3;

            input[offset] = rgb[src] as i32;
            input[offset + 1] = rgb[src + 1] as i32;
            input[offset + 2] = rgb[src + 2] as i32;
        }
    }

    Ok((
        input,
        width as f32 / scaled_width as f32,
        height as f32 / scaled_height as f32,
        pad_x,
        pad_y,
    ))
}

fn run_movenet(input: Vec<i32>) -> Result<Vec<f32>, String> {
    let session = pose_session()?;
    let mut session = session
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    let input = Tensor::from_array((
        [1usize, MOVENET_INPUT_SIZE as usize, MOVENET_INPUT_SIZE as usize, 3],
        input,
    ))
    .map_err(|err| err.to_string())?;

    let outputs = session
        .run(ort::inputs![input])
        .map_err(|err| err.to_string())?;

    let (_, raw) = outputs[0]
        .try_extract_tensor::<f32>()
        .map_err(|err| err.to_string())?;

    Ok(raw.to_vec())
}

/// Recognizes a pose on an RGB frame and measures the inference time.
pub fn recognize_frame(width: u32, height: u32, rgb: &[u8]) -> Result<PoseEstimate, String> {
    let started = std::time::Instant::now();
    let (input, scale_x, scale_y, pad_x, pad_y) = preprocess_frame(width, height, rgb)?;
    let preprocess_ms = started.elapsed().as_secs_f64() * 1000.0;

    let started = std::time::Instant::now();
    let raw = run_movenet(input)?;
    let inference_ms = started.elapsed().as_secs_f64() * 1000.0;

    let total_ms = preprocess_ms + inference_ms;

    if raw.len() < KEYPOINT_NAMES.len() * 3 {
        return Err("Unexpected MoveNet output format".to_string());
    }

    let keypoints = KEYPOINT_NAMES
        .iter()
        .copied()
        .enumerate()
        .map(|(index, name)| {
            let base = index * 3;
            // MoveNet outputs (y, x, score), normalized to 0..1.
            let y = raw[base];
            let x = raw[base + 1];
            let score = raw[base + 2];

            Keypoint {
                name,
                x: (x * MOVENET_INPUT_SIZE as f32 - pad_x as f32) * scale_x,
                y: (y * MOVENET_INPUT_SIZE as f32 - pad_y as f32) * scale_y,
                score,
            }
        })
        .collect();

    Ok(PoseEstimate {
        width,
        height,
        keypoints,
        inference_ms: total_ms,
        preprocess_ms,
        max_fps: 1000.0 / total_ms.max(0.001),
        provider: if GPU_EP_REGISTERED.load(std::sync::atomic::Ordering::Relaxed) {
            "GPU"
        } else {
            "CPU"
        },
    })
}
