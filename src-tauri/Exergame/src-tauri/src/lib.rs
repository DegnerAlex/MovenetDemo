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

            // Headless self-test (EXERGAME_SELFTEST=1): run the recognition
            // pipeline on a synthetic frame and on the real camera, then exit
            // before any window opens. Used to compare the packaged binary's
            // inference against the dev environment.
            #[cfg(not(target_os = "android"))]
            if std::env::var("EXERGAME_SELFTEST").is_ok_and(|value| !value.is_empty()) {
                let width = 320u32;
                let height = 240u32;
                let frame: Vec<u8> = (0..width as usize * height as usize * 3)
                    .map(|i| (i % 255) as u8)
                    .collect();
                for run in 0..25 {
                    let started = std::time::Instant::now();
                    match movenet::recognize_frame(width, height, &frame) {
                        Ok(pose) => eprintln!(
                            "selftest synth {:2}: {:6.2} ms (preprocess {:.2}, inference {:.2}) provider {}, max fps {:.1}",
                            run,
                            started.elapsed().as_secs_f64() * 1000.0,
                            pose.preprocess_ms,
                            pose.inference_ms - pose.preprocess_ms,
                            pose.provider,
                            pose.max_fps,
                        ),
                        Err(err) => eprintln!("selftest synth {run} failed: {err}"),
                    }
                }

                // Real camera cycle, timed the same way the streaming thread
                // does it: frame wait, MJPEG decode, recognition.
                use nokhwa::{
                    pixel_format::RgbFormat,
                    utils::{CameraIndex, RequestedFormat, RequestedFormatType, Resolution},
                    Camera,
                };
                let requested = RequestedFormat::new::<RgbFormat>(
                    RequestedFormatType::HighestResolution(Resolution::new(width, height)),
                );
                match Camera::new(CameraIndex::Index(0), requested) {
                    Ok(mut camera) => match camera.open_stream() {
                        Ok(()) => {
                            for run in 0..30 {
                                let wait = std::time::Instant::now();
                                let frame = match camera.frame() {
                                    Ok(frame) => frame,
                                    Err(err) => {
                                        eprintln!("selftest cam {run}: frame error: {err}");
                                        continue;
                                    }
                                };
                                let wait_ms = wait.elapsed().as_secs_f64() * 1000.0;

                                let decode = std::time::Instant::now();
                                let decoded = frame.decode_image::<RgbFormat>();
                                let decode_ms = decode.elapsed().as_secs_f64() * 1000.0;
                                let Ok(decoded) = decoded else {
                                    eprintln!("selftest cam {run}: decode error");
                                    continue;
                                };

                                let recognize = std::time::Instant::now();
                                let pose = movenet::recognize_frame(
                                    decoded.width(),
                                    decoded.height(),
                                    &decoded.into_raw(),
                                );
                                let recognize_ms =
                                    recognize.elapsed().as_secs_f64() * 1000.0;
                                match pose {
                                    Ok(pose) => eprintln!(
                                        "selftest cam   {:2}: wait {wait_ms:5.1}, decode {decode_ms:5.1}, recognize {recognize_ms:5.2} (pre {:.2}, inf {:.2}) provider {}",
                                        run,
                                        pose.preprocess_ms,
                                        pose.inference_ms - pose.preprocess_ms,
                                        pose.provider,
                                    ),
                                    Err(err) => {
                                        eprintln!("selftest cam {run}: {err}")
                                    }
                                }
                            }
                            let _ = camera.stop_stream();
                        }
                        Err(err) => eprintln!("selftest: open_stream failed: {err}"),
                    },
                    Err(err) => eprintln!("selftest: camera open failed: {err}"),
                }

                // With EXERGAME_SELFTEST_STREAM also set, keep running: the
                // webview autostarts the real stream (see RunEvent::Ready)
                // and the `pose:` lines show the live-UI recognition time.
                let keep_running =
                    std::env::var("EXERGAME_SELFTEST_STREAM").is_ok_and(|value| !value.is_empty());
                if !keep_running {
                    std::process::exit(0);
                }
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
            list_cameras,
            list_resolutions,
            start_rust_camera,
            stop_rust_camera
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| match event {
            // Self-test stream autostart (EXERGAME_SELFTEST_STREAM=1): drive
            // the real frontend start flow once the webview is ready, so the
            // packaged binary can be measured with the full UI running.
            RunEvent::Ready => {
                let autostart =
                    std::env::var("EXERGAME_SELFTEST_STREAM").is_ok_and(|value| !value.is_empty());
                if autostart {
                    if let Some(window) = app.get_webview_window("main") {
                        let _ = window.eval(
                            "setTimeout(() => window.__exergameSelftest && window.__exergameSelftest(), 1500);",
                        );
                    }
                }
            }
            // Belt and braces: also stop the stream when the whole app exits,
            // whatever triggered it.
            RunEvent::ExitRequested { .. } | RunEvent::Exit => {
                app.state::<CameraStreamState>().stop();
            }
            _ => {}
        });
}
