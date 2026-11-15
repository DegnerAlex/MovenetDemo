use nokhwa::{
    pixel_format::{RgbFormat, YuyvFormat},
    query,
    utils::{ApiBackend, CameraIndex, RequestedFormat, RequestedFormatType, Resolution},
    Camera,
};
use serde::Serialize;
use std::{
    collections::HashMap,
    os::fd::AsRawFd,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
};
use tauri::ipc::{Channel, Response};

use crate::{movenet, ResolutionInfo};

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
            if open_camera(parse_camera_index(&index), None).is_err() {
                return None;
            }

            Some(CameraDescriptor {
                index,
                name: camera.human_name(),
            })
        })
        .collect())
}

/// Opens a camera trying the RGB format preference first, then YUYV: virtual
/// cameras (the OBS loopback, for example) expose a single raw format — most
/// often YUYV — which the RGB preference alone may not negotiate. Every
/// frame is decoded to RGB later anyway, so the caller never sees the
/// difference.
///
/// On the "Framerate not whole number" failure the camera's frame interval
/// is normalized first: UVC cameras with auto exposure report the actual
/// (fractional) frame rate through V4L2, and the nokhwa V4L2 backend refuses
/// to open such a camera. Rounding the interval down to a whole frame rate
/// makes the camera openable again.
fn open_camera(index: CameraIndex, resolution: Option<Resolution>) -> Result<Camera, String> {
    let kind = match resolution {
        Some(res) => RequestedFormatType::HighestResolution(res),
        None => RequestedFormatType::AbsoluteHighestFrameRate,
    };

    let device = match &index {
        CameraIndex::Index(device) => Some(*device),
        _ => None,
    };

    let mut last_error = String::from("no format could open the camera");

    // RGB: the format the regular webcams negotiate.
    match Camera::new(index.clone(), RequestedFormat::new::<RgbFormat>(kind)) {
        Ok(camera) => return Ok(camera),
        Err(err) => last_error = err.to_string(),
    }
    if last_error.contains("not whole number") && device.is_some() {
        let _ = normalize_frame_interval(device.expect("checked above"));
        if let Ok(camera) = Camera::new(index.clone(), RequestedFormat::new::<RgbFormat>(kind)) {
            return Ok(camera);
        }
    }

    // YUYV: the format virtual cameras (OBS loopback) expose.
    match Camera::new(index.clone(), RequestedFormat::new::<YuyvFormat>(kind)) {
        Ok(camera) => return Ok(camera),
        Err(err) => last_error = err.to_string(),
    }
    if last_error.contains("not whole number") && device.is_some() {
        let _ = normalize_frame_interval(device.expect("checked above"));
        if let Ok(camera) = Camera::new(index.clone(), RequestedFormat::new::<YuyvFormat>(kind)) {
            return Ok(camera);
        }
    }

    Err(last_error)
}

const V4L2_BUF_TYPE_VIDEO_CAPTURE: u32 = 1;

const fn v4l2_iowr(nr: u32) -> u32 {
    // sizeof(struct v4l2_streamparm) == 804 (type + the 200-u32 union).
    (3 << 30) | (804 << 16) | (0x56 << 8) | nr
}

const VIDIOC_G_PARM: u32 = v4l2_iowr(59);
const VIDIOC_S_PARM: u32 = v4l2_iowr(62);

/// The kernel `struct v4l2_streamparm`, flattened: `type` followed by the
/// 200-`u32` union. Only the capture member is used, whose layout inside the
/// union is fixed by the kernel ABI: capability, timeperframe (numerator,
/// denominator), capturemode, …
#[repr(C)]
struct V4l2Streamparm {
    param_type: u32,
    data: [u32; 200],
}

/// Rounds the camera's current frame rate down to a whole number (V4L2
/// timeperframe), so the nokhwa V4L2 backend accepts the camera.
fn normalize_frame_interval(device: u32) -> Result<(), String> {
    let path = format!("/dev/video{device}");
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|err| format!("cannot open {path}: {err}"))?;

    let mut parm = V4l2Streamparm {
        param_type: V4L2_BUF_TYPE_VIDEO_CAPTURE,
        data: [0; 200],
    };

    // SAFETY: a plain VIDIOC_G_PARM ioctl with a correctly sized struct.
    if unsafe { libc::ioctl(file.as_raw_fd(), VIDIOC_G_PARM as libc::c_ulong, &mut parm) } < 0 {
        return Err("VIDIOC_G_PARM failed".to_string());
    }

    let (numerator, denominator) = (parm.data[1], parm.data[2]);
    if numerator == 1 && denominator % numerator == 0 {
        return Ok(()); // already a whole frame rate
    }

    let whole_fps = (denominator / numerator).max(1);

    let mut out = V4l2Streamparm {
        param_type: V4L2_BUF_TYPE_VIDEO_CAPTURE,
        data: [0; 200],
    };
    out.data[1] = 1; // timeperframe numerator: 1
    out.data[2] = whole_fps; // timeperframe denominator: whole fps

    // SAFETY: a plain VIDIOC_S_PARM ioctl with a correctly sized struct.
    if unsafe { libc::ioctl(file.as_raw_fd(), VIDIOC_S_PARM as libc::c_ulong, &mut out) } < 0 {
        return Err("VIDIOC_S_PARM failed".to_string());
    }

    Ok(())
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
pub fn list_resolutions(camera_index: String) -> Result<Vec<ResolutionInfo>, String> {
    // The camera is opened for the duration of the query: the lists of
    // supported resolutions and frame rates can only be read from an open
    // device.
    let mut camera = open_camera(parse_camera_index(&camera_index), None)
        .map_err(|err| err.to_string())?;

    // Highest supported frame rate per resolution across every pixel format
    // (the backend reports the frame rates next to each resolution).
    let mut frame_rates: HashMap<Resolution, u32> = HashMap::new();
    let formats = camera.compatible_fourcc().unwrap_or_default();
    for format in formats {
        if let Ok(list) = camera.compatible_list_by_resolution(format) {
            for (resolution, rates) in list {
                let best = rates.iter().copied().max().unwrap_or(0);
                frame_rates
                    .entry(resolution)
                    .and_modify(|current| *current = (*current).max(best))
                    .or_insert(best);
            }
        }
    }

    let mut infos: Vec<ResolutionInfo> = frame_rates
        .into_iter()
        .map(|(resolution, max_fps)| ResolutionInfo {
            resolution: format!("{}x{}", resolution.width_x, resolution.height_y),
            max_fps: (max_fps > 0).then_some(max_fps),
        })
        .collect();

    // From the largest resolution down.
    infos.sort_unstable_by(|a, b| {
        let area = |value: &ResolutionInfo| -> u64 {
            let (w, h) = value
                .resolution
                .split_once('x')
                .map_or((0, 0), |(w, h)| (w.parse().unwrap_or(0), h.parse().unwrap_or(0)));
            w as u64 * h as u64
        };

        area(b).cmp(&area(a)).then_with(|| b.resolution.cmp(&a.resolution))
    });

    Ok(infos)
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

        let mut camera = match open_camera(index, resolution.as_deref().and_then(parse_resolution))
        {
            Ok(camera) => camera,
            Err(err) => {
                let _ = started_tx.send(Err(err));
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
                // Some decoders (YUYV frames from v4l2loopback devices) emit
                // buffers with trailing padding: the payload layout assumes
                // exactly width*height*3 RGB bytes, so normalize the length.
                let mut raw_bytes = decoded.into_raw();
                raw_bytes.resize(width as usize * height as usize * 3, 0);

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
