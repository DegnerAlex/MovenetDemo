mod camera;
mod movenet;

use camera::CameraStreamState;
use tauri::{Manager, RunEvent, WindowEvent};

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
        .setup(|app| {
            // Needed so movenet can find the ONNX Runtime libraries bundled
            // into AppImage builds.
            if let Ok(dir) = app.path().resource_dir() {
                movenet::set_resource_dir(dir);
            }
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
            camera::list_cameras,
            camera::list_resolutions,
            camera::start_rust_camera,
            camera::stop_rust_camera
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
