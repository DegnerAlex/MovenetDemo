use std::path::Path;

const COMMANDS: &[&str] = &["list_cameras", "start_capture", "stop_capture", "read_frame"];

fn main() {
    tauri_plugin::Builder::new(COMMANDS)
        .android_path("android")
        .build();

    let target = std::env::var("TARGET").unwrap_or_default();
    if !target.contains("android") {
        return;
    }

    // Only touch the generated project when `tauri android init` already ran.
    if !Path::new("../../gen/android").exists() {
        return;
    }

    // Declare the CAMERA permission in the app manifest; the runtime request
    // happens in the Kotlin plugin (startCapture → permission callback).
    tauri_plugin::mobile::update_android_manifest(
        "EXERGAME CAMERA PLUGIN",
        "manifest",
        r#"<uses-permission android:name="android.permission.CAMERA" />"#.to_string(),
    )
    .expect("failed to update the Android manifest");
}
