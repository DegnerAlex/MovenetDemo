// Copyright 2026 Alex Degner (alex@ergofox.me)
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

#[cfg(not(target_os = "android"))]
mod movenet;

#[cfg(not(target_os = "android"))]
mod camera;
#[cfg(target_os = "android")]
mod camera_android;

// The re-exports are consumed by `generate_handler!` below; the macro expands
// to code the compiler does not count as a "use", hence the allow.
#[cfg(not(target_os = "android"))]
#[allow(unused_imports)]
use camera::{list_cameras, list_resolutions, start_rust_camera, stop_rust_camera};
#[cfg(target_os = "android")]
#[allow(unused_imports)]
use camera_android::{list_cameras, list_resolutions, start_rust_camera, stop_rust_camera};

#[cfg(not(target_os = "android"))]
use camera::CameraStreamState;
#[cfg(target_os = "android")]
use camera_android::CameraStreamState;

use tauri::{Manager, RunEvent, WindowEvent};
use serde::Serialize;

/// A camera resolution with its maximum supported frame rate when known:
/// the desktop reads it from the camera's frame intervals, Android does not
/// expose it yet.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolutionInfo {
    pub resolution: String,
    pub max_fps: Option<u32>,
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // WebKitGTK's DMA-BUF renderer crashes GTK on some Wayland setups
    // (Gdk protocol error 71, the window never opens). The container runs
    // with this already set via docker-compose; set it for host runs too.
    // Safe: called once at startup, before any threads spawn.
    unsafe {
        std::env::set_var("WEBKIT_DISABLE_COMPOSITING_MODE", "1");
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        // Logs go to stdout and the webview console only: the default file
        // target lives in ~/.local/share, which the container app (root) and
        // the host app (regular user) would fight over.
        .plugin(
            tauri_plugin_log::Builder::new()
                .targets([
                    tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::Stdout),
                    tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::Webview),
                ])
                .build(),
        )
        .manage(CameraStreamState::default())
        // Mobile: the camera runs in the Kotlin plugin (CameraX). Desktop:
        // the plugin is inert and `camera.rs` captures with nokhwa instead.
        .plugin(tauri_plugin_exergame_camera::init())
        .setup(|app| {
            // Needed so movenet can find the ONNX Runtime libraries bundled
            // into AppImage builds. Desktop only: on Android the runtime
            // ships as a JNI library instead.
            #[cfg(not(target_os = "android"))]
            if let Ok(dir) = app.path().resource_dir() {
                movenet::set_resource_dir(dir);
            }
            #[cfg(target_os = "android")]
            let _ = app;
            Ok(())
        })
        // The webview may be destroyed without React's unmount handlers
        // running (e.g. the user closes the window mid-stream). Stop the
        // capture thread here — joining it before the webview goes away — so
        // no frames are pushed into a dying webview.
        .on_window_event(|window, event| {
            if matches!(event, WindowEvent::Destroyed) {
                window
                    .app_handle()
                    .state::<CameraStreamState>()
                    .stop();
            }
        })
        .invoke_handler(tauri::generate_handler![
            list_cameras,
            list_resolutions,
            start_rust_camera,
            stop_rust_camera
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| match event {
            // Belt and braces: also stop the stream when the whole app exits,
            // whatever triggered it.
            RunEvent::ExitRequested { .. } | RunEvent::Exit => {
                app.state::<CameraStreamState>().stop();
            }
            _ => {}
        });
}
