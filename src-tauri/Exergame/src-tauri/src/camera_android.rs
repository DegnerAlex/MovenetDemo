//! Android implementation of the camera commands.
//!
//! Mirrors `camera.rs`: same command names, same frame payload format
//! (12-byte big-endian header + RGB pixels + JSON pose tail). Frames come
//! from the Kotlin plugin (CameraX) as JPEG and are decoded here; the pose
//! was already recognized on the Kotlin side (LiteRT + GPU delegate) and
//! arrives with the frame, so this module only transports it.

use serde::Serialize;
use std::{
    io::Cursor,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
};
use tauri::{
    ipc::{Channel, Response},
    AppHandle, Manager, Runtime, State,
};

use crate::ResolutionInfo;
use tauri_plugin_exergame_camera::Camera;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CameraDescriptor {
    pub index: String,
    pub name: String,
}

pub struct StreamHandle {
    pub stop: Arc<AtomicBool>,
    pub thread: thread::JoinHandle<()>,
}

#[derive(Default)]
pub struct CameraStreamState {
    pub handle: Mutex<Option<StreamHandle>>,
}

impl CameraStreamState {
    pub fn stop(&self) {
        let handle = self
            .handle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();

        if let Some(handle) = handle {
            handle.stop.store(true, Ordering::Relaxed);
            // Wait for the thread so the camera gets released.
            let _ = handle.thread.join();
        }
    }
}

/// Parses a resolution string like "640x480" into (width, height).
fn parse_resolution(value: &str) -> Option<(u32, u32)> {
    let (width, height) = value.split_once('x')?;
    let width = width.trim().parse::<u32>().ok()?;
    let height = height.trim().parse::<u32>().ok()?;

    if width == 0 || height == 0 {
        return None;
    }

    Some((width, height))
}

#[tauri::command(async)]
pub fn list_cameras<R: Runtime>(app: AppHandle<R>) -> Result<Vec<CameraDescriptor>, String> {
    let camera = app.state::<Camera<R>>();
    let cameras = camera.list_cameras()?;

    Ok(cameras
        .into_iter()
        .map(|camera| CameraDescriptor {
            index: camera.id,
            name: camera.name,
        })
        .collect())
}

#[tauri::command(async)]
pub fn list_resolutions<R: Runtime>(
    camera_index: String,
    app: AppHandle<R>,
) -> Result<Vec<ResolutionInfo>, String> {
    let camera = app.state::<Camera<R>>();
    let mut resolutions = camera.list_resolutions(Some(camera_index))?;

    // Largest first; drop duplicates (the same size in several layouts).
    // The plugin does not expose frame rate ranges yet, so the FPS field
    // stays empty on Android.
    resolutions.sort_unstable_by(|a, b| {
        let area = |value: &String| -> u64 {
            let (w, h) = value
                .split_once('x')
                .map_or((0, 0), |(w, h)| (w.parse().unwrap_or(0), h.parse().unwrap_or(0)));
            w as u64 * h as u64
        };

        area(b).cmp(&area(a)).then_with(|| b.cmp(a))
    });
    resolutions.dedup();

    Ok(resolutions
        .into_iter()
        .map(|resolution| ResolutionInfo {
            resolution,
            max_fps: None,
        })
        .collect())
}

#[tauri::command(async)]
pub fn start_rust_camera<R: Runtime>(
    camera_index: String,
    resolution: Option<String>,
    on_frame: Channel<Response>,
    app: AppHandle<R>,
    state: State<'_, CameraStreamState>,
) -> Result<(), String> {
    // Stop the previous stream before starting a new camera.
    state.stop();

    let (width, height) = resolution
        .as_deref()
        .and_then(parse_resolution)
        .unwrap_or((0, 0));

    let stop = Arc::new(AtomicBool::new(false));
    let stop_for_thread = Arc::clone(&stop);
    let (started_tx, started_rx) = mpsc::channel::<Result<(), String>>();

    let thread_handle = thread::spawn(move || {
        let camera: Camera<R> = app.state::<Camera<R>>().inner().clone();

        if let Err(err) = camera.start_capture(Some(camera_index), width, height) {
            let _ = started_tx.send(Err(err));
            return;
        }

        // Tell the frontend that the camera opened successfully.
        if started_tx.send(Ok(())).is_err() {
            let _ = camera.stop_capture();
            return;
        }

        let mut decode_error_logged = false;

        while !stop_for_thread.load(Ordering::Relaxed) {
            match camera.read_frame() {
                Ok(Some(frame)) => {
                    let Some((frame_width, frame_height, rgb)) = decode_jpeg(&frame.jpeg) else {
                        if !decode_error_logged {
                            eprintln!("failed to decode the captured frame");
                            decode_error_logged = true;
                        }
                        continue;
                    };

                    // The pose was recognized on the Kotlin side (LiteRT +
                    // GPU delegate); only the transport happens here.
                    let pose_json = serde_json::to_vec(&serde_json::json!({
                        "width": frame.width,
                        "height": frame.height,
                        "keypoints": frame.keypoints,
                        "inferenceMs": frame.inference_ms,
                        "preprocessMs": frame.preprocess_ms,
                        "maxFps": 1000.0 / frame.inference_ms.max(0.001),
                        "provider": frame.provider,
                    }))
                    .unwrap_or_default();

                    // First 12 bytes: width, height and JSON tail length (u32
                    // big-endian), then RGB pixels, then JSON with the pose.
                    let mut payload =
                        Vec::with_capacity(12 + rgb.len() + pose_json.len());
                    payload.extend_from_slice(&frame_width.to_be_bytes());
                    payload.extend_from_slice(&frame_height.to_be_bytes());
                    payload.extend_from_slice(&(pose_json.len() as u32).to_be_bytes());
                    payload.extend_from_slice(&rgb);
                    payload.extend_from_slice(&pose_json);

                    if on_frame.send(Response::new(payload)).is_err() {
                        break; // The frontend closed the channel
                    }
                }
                Ok(None) => {
                    // The camera is starting up; retry shortly.
                    thread::sleep(std::time::Duration::from_millis(16));
                }
                Err(err) => {
                    eprintln!("read_frame: {err}");
                    thread::sleep(std::time::Duration::from_millis(100));
                }
            }
        }

        // Stop the stream and release the camera.
        let _ = camera.stop_capture();
    });

    let start_result = match started_rx.recv() {
        Ok(result) => result,
        Err(err) => {
            let _ = thread_handle.join();
            return Err(err.to_string());
        }
    };

    match start_result {
        Ok(()) => {
            let mut guard = state
                .handle
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *guard = Some(StreamHandle {
                stop,
                thread: thread_handle,
            });
            Ok(())
        }
        Err(err) => {
            let _ = thread_handle.join();
            Err(err)
        }
    }
}

#[tauri::command(async)]
pub fn stop_rust_camera(state: State<'_, CameraStreamState>) {
    state.inner().stop();
}

/// Decodes a JPEG frame into raw RGB pixels.
fn decode_jpeg(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let mut decoder = jpeg_decoder::Decoder::new(Cursor::new(bytes));
    decoder.read_info().ok()?;

    let info = decoder.info()?;
    if info.pixel_format != jpeg_decoder::PixelFormat::RGB24 {
        return None;
    }

    let pixels = decoder.decode().ok()?;
    Some((info.width as u32, info.height as u32, pixels))
}
