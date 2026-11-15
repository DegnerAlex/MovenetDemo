use std::path::Path;

const COMMANDS: &[&str] = &["list_cameras", "start_capture", "stop_capture", "read_frame"];

fn main() {
    tauri_plugin::Builder::new(COMMANDS)
        .android_path("android")
        .build();

    copy_ort_android_libs();
}

// Copies the Android ONNX Runtime build into the generated app's jniLibs so
// that dlopen("libonnxruntime.so") resolves at runtime (the app itself is
// built with `load-dynamic`).
fn copy_ort_android_libs() {
    let Ok(target) = std::env::var("TARGET") else {
        return;
    };
    if !target.contains("android") {
        return;
    }

    let abi = if target.starts_with("aarch64") {
        "arm64-v8a"
    } else if target.starts_with("x86_64") {
        "x86_64"
    } else {
        return;
    };

    let source = Path::new("../ort-android").join(abi).join("libonnxruntime.so");
    if !source.exists() {
        println!(
            "cargo:warning=ort-android/{abi}/libonnxruntime.so is missing: the Android app will fail to load ONNX Runtime"
        );
        return;
    }

    // Only touch the generated project when `tauri android init` already ran.
    if !Path::new("../../gen/android").exists() {
        return;
    }

    println!("cargo:rerun-if-changed={}", source.display());

    // cwd during build scripts is the plugin crate directory
    // (src-tauri/plugins/camera), so the app's gen folder is two levels up.
    let gen = Path::new("../../gen/android/app/src/main/jniLibs").join(abi);
    std::fs::create_dir_all(&gen).expect("failed to create jniLibs directory");
    std::fs::copy(&source, gen.join("libonnxruntime.so")).expect("failed to copy libonnxruntime.so");
}
