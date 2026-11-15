use nokhwa::{
    pixel_format::RgbFormat,
    query,
    utils::{ApiBackend, CameraIndex, RequestedFormat, RequestedFormatType, Resolution},
    Camera,
};
use serde::Serialize;
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
};
use tauri::ipc::{Channel, Response};

use crate::movenet;

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
            // Wait for the thread to finish so the camera gets released.
            let _ = handle.thread.join();
        }
    }
}

#[tauri::command(async)]
pub fn list_cameras() -> Result<Vec<CameraDescriptor>, String> {
    let cameras = query(ApiBackend::Auto).map_err(|err| err.to_string())?;

    Ok(cameras
        .into_iter()
        .filter_map(|camera| {
            let index = camera.index().as_string();

            // Only expose cameras we can actually open: some V4L2 nodes
            // (e.g. metadata-only nodes) enumerate fine but support no
            // capture format, which makes Camera::new fail with
            // "Failed to Fufill" later on.
            let requested =
                RequestedFormat::new::<RgbFormat>(RequestedFormatType::AbsoluteHighestFrameRate);
            if Camera::new(parse_camera_index(&index), requested).is_err() {
                return None;
            }

            Some(CameraDescriptor {
                index,
                name: camera.human_name(),
            })
        })
        .collect())
}

fn parse_camera_index(value: &str) -> CameraIndex {
    value
        .parse::<u32>()
        .map(CameraIndex::Index)
        .unwrap_or_else(|_| CameraIndex::String(value.to_string()))
}

/// Parses a resolution string like "640x480".
fn parse_resolution(value: &str) -> Option<Resolution> {
    let (width, height) = value.split_once('x')?;
    let width = width.trim().parse::<u32>().ok()?;
    let height = height.trim().parse::<u32>().ok()?;

    if width == 0 || height == 0 {
        return None;
    }

    Some(Resolution::new(width, height))
}

#[tauri::command(async)]
pub fn list_resolutions(camera_index: String) -> Result<Vec<String>, String> {
    let index = parse_camera_index(&camera_index);
    let requested = RequestedFormat::new::<RgbFormat>(
        RequestedFormatType::AbsoluteHighestFrameRate,
    );

    // The camera is opened for the duration of the query: the list of
    // supported resolutions can only be read from an open device.
    let mut camera = Camera::new(index, requested).map_err(|err| err.to_string())?;

    let mut resolutions: Vec<Resolution> = Vec::new();
    let formats = camera.compatible_fourcc().unwrap_or_default();
    for format in formats {
        if let Ok(map) = camera.compatible_list_by_resolution(format) {
            resolutions.extend(map.into_keys());
        }
    }

    // From the largest resolution down; drop duplicates (the same resolution
    // advertised in several formats).
    resolutions.sort_unstable_by(|a, b| {
        (b.width_x as u64 * b.height_y as u64)
            .cmp(&(a.width_x as u64 * a.height_y as u64))
            .then_with(|| b.width_x.cmp(&a.width_x))
    });
    resolutions.dedup();

    Ok(resolutions
        .into_iter()
        .map(|res| format!("{}x{}", res.width_x, res.height_y))
        .collect())
}

#[tauri::command(async)]
pub fn start_rust_camera(
    camera_index: String,
    resolution: Option<String>,
    on_frame: Channel<Response>,
    state: tauri::State<'_, CameraStreamState>,
) -> Result<(), String> {
    // Stop the previous stream before starting a new camera.
    state.stop();

    let stop = Arc::new(AtomicBool::new(false));
    let stop_for_thread = Arc::clone(&stop);
    let (started_tx, started_rx) = mpsc::channel::<Result<(), String>>();

    let thread_handle = thread::spawn(move || {
        let index = parse_camera_index(&camera_index);
        // If a resolution is selected, use it with the highest FPS; otherwise
        // the camera picks the format with the maximum frame rate.
        let requested = match resolution.as_deref().and_then(parse_resolution) {
            Some(res) => {
                RequestedFormat::new::<RgbFormat>(RequestedFormatType::HighestResolution(res))
            }
            None => RequestedFormat::new::<RgbFormat>(
                RequestedFormatType::AbsoluteHighestFrameRate,
            ),
        };

        let mut camera = match Camera::new(index, requested) {
            Ok(camera) => camera,
            Err(err) => {
                let _ = started_tx.send(Err(err.to_string()));
                return;
            }
        };

        if let Err(err) = camera.open_stream() {
            let _ = started_tx.send(Err(err.to_string()));
            return;
        }

        // Tell the frontend that the camera opened successfully.
        if started_tx.send(Ok(())).is_err() {
            let _ = camera.stop_stream();
            return;
        }

        let mut pose_error_logged = false;
        let mut frames_since_log: u32 = 0;

        while !stop_for_thread.load(Ordering::Relaxed) {
            if let Ok(frame) = camera.frame() {
                let Ok(decoded) = frame.decode_image::<RgbFormat>() else {
                    continue;
                };

                let width = decoded.width();
                let height = decoded.height();
                let raw_bytes = decoded.into_raw();

                // Recognize the pose right in the capture thread: while the
                // inference runs, new frames are simply not picked up, so the
                // stream FPS equals the recognition FPS.
                let pose_json = match movenet::recognize_frame(width, height, &raw_bytes) {
                    Ok(pose) => {
                        // Once every ~5 seconds log the timing breakdown: it
                        // shows what dominates — inference or preprocessing.
                        frames_since_log += 1;
                        if frames_since_log >= 60 {
                            frames_since_log = 0;
                            eprintln!(
                                "pose: {:.0} ms total (preprocess {:.0} ms, inference {:.0} ms), provider {}, max fps {:.1}, frame {}x{}",
                                pose.inference_ms,
                                pose.preprocess_ms,
                                pose.inference_ms - pose.preprocess_ms,
                                pose.provider,
                                pose.max_fps,
                                width,
                                height,
                            );
                        }

                        serde_json::to_vec(&pose).unwrap_or_default()
                    }
                    Err(err) => {
                        if !pose_error_logged {
                            eprintln!("recognize_frame: {err}");
                            pose_error_logged = true;
                        }

                        // The frame still goes to the frontend, just without pose data.
                        serde_json::to_vec(&movenet::PoseEstimate {
                            width,
                            height,
                            keypoints: Vec::new(),
                            inference_ms: 0.0,
                            preprocess_ms: 0.0,
                            max_fps: 0.0,
                            provider: "",
                        })
                        .unwrap_or_default()
                    }
                };

                // First 12 bytes: width, height and JSON tail length (u32
                // big-endian), then RGB pixels, then JSON with the recognition
                // results (keypoints, inferenceMs, maxFps).
                let mut payload =
                    Vec::with_capacity(12 + raw_bytes.len() + pose_json.len());
                payload.extend_from_slice(&width.to_be_bytes());
                payload.extend_from_slice(&height.to_be_bytes());
                payload.extend_from_slice(&(pose_json.len() as u32).to_be_bytes());
                payload.extend_from_slice(&raw_bytes);
                payload.extend_from_slice(&pose_json);

                // Send the frame to the frontend through the channel
                if on_frame.send(Response::new(payload)).is_err() {
                    break; // The frontend closed the channel
                }
            }
        }

        // Stop the stream and release the device.
        let _ = camera.stop_stream();
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
pub fn stop_rust_camera(state: tauri::State<'_, CameraStreamState>) {
    state.inner().stop();
}
