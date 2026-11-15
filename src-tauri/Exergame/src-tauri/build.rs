use std::{fs, path::Path};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    // The runtime bundle directory is shared between desktop and Android
    // builds: each build makes it valid for its own target BEFORE
    // tauri_build::build() validates the resources glob.
    //
    // - desktop: the GPU ONNX Runtime stack (CUDA/cuDNN included),
    // - android: nothing is bundled (the ARM runtime ships as a JNI
    //   library), but the resources glob must still match, hence the
    //   CPU-only marker.
    //
    // Desktop and Android builds can run in parallel (they do during
    // development), so the Android branch must never delete the desktop
    // files: it only creates the directory when it does not exist yet.
    let bundle_dir = Path::new("target/release/ort-gpu-bundle");
    let target = std::env::var("TARGET").unwrap_or_default();
    let is_android = target.contains("android");

    if is_android {
        if !bundle_dir.exists() {
            fs::create_dir_all(bundle_dir).expect("failed to create ort-gpu-bundle");
            fs::write(
                bundle_dir.join("CPU_ONLY_BUILD.txt"),
                "Android build: ONNX Runtime ships as a JNI library, not from here.\n",
            )
            .expect("failed to write bundle marker");
        }
    } else {
        // Desktop: rebuild from scratch on every run, so a marker left by an
        // Android build never survives into a packaged bundle.
        let _ = fs::remove_dir_all(bundle_dir);
        fs::create_dir_all(bundle_dir).expect("failed to create ort-gpu-bundle");

        if let Some(ort_lib) = find_ort_lib() {
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

            // cuDNN 9: ORT dlopens the unversioned "libcudnn.so", and the main
            // library pulls in the sub-libraries from its own directory.
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
        } else {
            // No GPU ONNX Runtime available at build time — CPU-only bundle.
            fs::write(
                bundle_dir.join("CPU_ONLY_BUILD.txt"),
                "GPU ONNX Runtime was not available at build time; this build runs on CPU.\n",
            )
            .expect("failed to write bundle marker");
        }
    }

    // Tauri build steps: validates the resources we just prepared.
    tauri_build::build();
}

fn find_ort_lib() -> Option<std::path::PathBuf> {
    // The GPU ONNX Runtime build lives either in /opt/onnxruntime/lib (older
    // image layout) or directly in /opt/onnxruntime (current layout).
    ["/opt/onnxruntime/lib", "/opt/onnxruntime"]
        .iter()
        .map(|dir| Path::new(dir).join("libonnxruntime.so.1.31.1"))
        .find(|path| path.exists())
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
