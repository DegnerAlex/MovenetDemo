//! Build script.
//!
//! 1. Runs the standard Tauri build steps (config, capabilities, resources).
//! 2. Assembles the GPU runtime bundle (`ort-gpu-bundle/`) that the AppImage
//!    build packs into its resources. The bundle lives in the cargo target
//!    directory (`target/<profile>/ort-gpu-bundle`), so it never clutters the
//!    project tree and is removed by `cargo clean`.
//!
//! Inside the exergame container the GPU ONNX Runtime build (/opt/onnxruntime),
//! the CUDA 13 runtime (/usr/local/cuda-13.1) and cuDNN 9 (/usr/lib/x86_64-linux-gnu)
//! are available, so the bundle is assembled from there. When building elsewhere
//! (e.g. on the host without CUDA), the bundle is marked as CPU-only: the
//! resources glob still matches, and the app runs on CPU.

use std::{fs, path::Path};

fn main() {
    // Required by Tauri: processes the config, capabilities and resources.
    tauri_build::build();

    println!("cargo:rerun-if-changed=build.rs");

    // Android builds cross-compile in this same container: the GPU bundle
    // below is an x86_64 build and must never be assembled (or left over from
    // a desktop build) for the Android package. The static resources glob
    // (target/release/ort-gpu-bundle/*) is emptied instead, so nothing CUDA
    // ends up in the APK.
    if std::env::var("TARGET")
        .map(|target| target.contains("android"))
        .unwrap_or(false)
    {
        let glob_dir = Path::new("target/release/ort-gpu-bundle");
        let _ = fs::remove_dir_all(glob_dir);
        let _ = fs::create_dir_all(glob_dir);
        let _ = fs::write(
            glob_dir.join("CPU_ONLY_BUILD.txt"),
            "Android build: ONNX Runtime ships as a JNI library, not from here.\n",
        );
        return;
    }

    // OUT_DIR = <target>/<profile>/build/exergame-<hash>/out → walk up to the
    // profile directory (target/release or target/debug).
    let Some(profile_dir) = std::env::var_os("OUT_DIR")
        .as_deref()
        .map(std::path::PathBuf::from)
        .and_then(|out| out.ancestors().nth(3).map(Path::to_path_buf))
    else {
        return;
    };

    let bundle_dir = profile_dir.join("ort-gpu-bundle");
    fs::create_dir_all(&bundle_dir).expect("failed to create ort-gpu-bundle");

    // The GPU ONNX Runtime build lives either in /opt/onnxruntime/lib (older
    // image layout) or directly in /opt/onnxruntime (current layout).
    let ort_lib = ["/opt/onnxruntime/lib", "/opt/onnxruntime"]
        .iter()
        .map(|dir| Path::new(dir).join("libonnxruntime.so.1.31.1"))
        .find(|path| path.exists());

    let Some(ort_lib) = ort_lib else {
        // No GPU ONNX Runtime available at build time — CPU-only bundle.
        fs::write(
            bundle_dir.join("CPU_ONLY_BUILD.txt"),
            "GPU ONNX Runtime was not available at build time; this build runs on CPU.\n",
        )
        .expect("failed to write bundle marker");
        return;
    };

    println!("cargo:rerun-if-changed={}", ort_lib.display());

    let ort_dir = ort_lib.parent().expect("ort lib has no parent dir");

    copy_if_changed(&ort_lib, &bundle_dir.join("libonnxruntime-gpu.so"));
    copy_if_changed(
        &ort_dir.join("libonnxruntime_providers_cuda.so"),
        &bundle_dir.join("libonnxruntime_providers_cuda.so"),
    );
    copy_if_changed(
        &ort_dir.join("libonnxruntime_providers_shared.so"),
        &bundle_dir.join("libonnxruntime_providers_shared.so"),
    );

    // CUDA 13.1 runtime libraries required by the CUDA provider.
    let cuda_dir = Path::new("/usr/local/cuda-13.1/targets/x86_64-linux/lib");
    for name in [
        "libcudart.so.13",
        "libcublas.so.13",
        "libcublasLt.so.13",
        "libcurand.so.10",
    ] {
        println!("cargo:rerun-if-changed={}", cuda_dir.join(name).display());
        copy_if_changed(&cuda_dir.join(name), &bundle_dir.join(name));
    }

    // cuDNN 9: ORT dlopens the unversioned "libcudnn.so", and the main library
    // pulls in the sub-libraries (graph/engines/...) from its own directory.
    let cudnn_dir = Path::new("/usr/lib/x86_64-linux-gnu");
    if let Ok(entries) = fs::read_dir(cudnn_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !name.starts_with("libcudnn") || !name.contains(".so.9") {
                continue;
            }

            println!("cargo:rerun-if-changed={}", entry.path().display());
            if name == "libcudnn.so.9" {
                // Unversioned name that ORT dlopens.
                copy_if_changed(&entry.path(), &bundle_dir.join("libcudnn.so"));
            } else {
                copy_if_changed(&entry.path(), &bundle_dir.join(name.as_ref()));
            }
        }
    }
}

/// Copies `from` to `to` following symlinks, skipping the copy when the
/// destination already exists with the same size.
fn copy_if_changed(from: &Path, to: &Path) {
    if let (Ok(dest_meta), Ok(src_meta)) = (fs::metadata(to), fs::metadata(from)) {
        if dest_meta.len() == src_meta.len() {
            return;
        }
    }

    if let Err(err) = fs::copy(from, to) {
        panic!("failed to copy {} to {}: {err}", from.display(), to.display());
    }
}
