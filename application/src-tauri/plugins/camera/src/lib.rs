//! Exergame camera plugin.
//!
//! On Android the actual capture runs in Kotlin (CameraX, see
//! `android/src/main/java/CameraPlugin.kt`); this crate only forwards
//! commands via `run_mobile_plugin` and exposes a small typed API that the
//! app's `camera_android` module uses to feed MoveNet. On desktop builds the
//! plugin is inert: `camera.rs` captures with `nokhwa` instead.

use serde::Deserialize;
#[cfg(target_os = "android")]
use tauri::Manager;
use tauri::{
    plugin::{Builder as PluginBuilder, PluginHandle, TauriPlugin},
    Runtime,
};

#[cfg(target_os = "android")]
const PLUGIN_IDENTIFIER: &str = "com.exergame.camera";

/// A camera reported by the device.
#[derive(Debug, Clone, serde::Serialize, Deserialize)]
pub struct CameraDescriptor {
    pub id: String,
    pub name: String,
}

/// A single captured frame: JPEG for the video preview plus the MoveNet
/// keypoints (LiteRT, decoded on the Kotlin side).
#[derive(Debug, Clone, Deserialize)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub jpeg: Vec<u8>,
    #[serde(default)]
    pub keypoints: serde_json::Value,
    #[serde(default)]
    pub inference_ms: f64,
    #[serde(default)]
    pub preprocess_ms: f64,
    #[serde(default)]
    pub provider: String,
}

/// Handle to the camera plugin.
pub struct Camera<R: Runtime>(PluginHandle<R>);

impl<R: Runtime> Clone for Camera<R> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<R: Runtime> Camera<R> {
    /// Lists usable (back-facing) cameras.
    #[cfg(mobile)]
    pub fn list_cameras(&self) -> Result<Vec<CameraDescriptor>, String> {
        let response: serde_json::Value = self
            .0
            .run_mobile_plugin("listCameras", serde_json::json!({}))
            .map_err(|err| err.to_string())?;

        let cameras = response
            .get("cameras")
            .cloned()
            .unwrap_or(serde_json::Value::Null);

        serde_json::from_value(cameras).map_err(|err| err.to_string())
    }

    /// Lists the resolutions supported by a camera, largest first.
    #[cfg(mobile)]
    pub fn list_resolutions(&self, camera_id: Option<String>) -> Result<Vec<String>, String> {
        let response: serde_json::Value = self
            .0
            .run_mobile_plugin("listResolutions", serde_json::json!({ "cameraId": camera_id }))
            .map_err(|err| err.to_string())?;

        let resolutions = response
            .get("resolutions")
            .cloned()
            .unwrap_or(serde_json::Value::Null);

        serde_json::from_value(resolutions).map_err(|err| err.to_string())
    }

    /// Starts the capture. `width`/`height` of 0 let the camera decide.
    #[cfg(mobile)]
    pub fn start_capture(
        &self,
        camera_id: Option<String>,
        width: u32,
        height: u32,
    ) -> Result<(), String> {
        let payload = serde_json::json!({
            "cameraId": camera_id,
            "width": width,
            "height": height,
        });

        self.0
            .run_mobile_plugin::<serde_json::Value>("startCapture", payload)
            .map(|_| ())
            .map_err(|err| err.to_string())
    }

    /// Returns the latest captured frame, or `None` when none has arrived yet.
    #[cfg(mobile)]
    pub fn read_frame(&self) -> Result<Option<Frame>, String> {
        let response: serde_json::Value = self
            .0
            .run_mobile_plugin("readFrame", serde_json::json!({}))
            .map_err(|err| err.to_string())?;

        if response.is_null() {
            return Ok(None);
        }

        let jpeg_base64 = response
            .get("jpeg")
            .and_then(|value| value.as_str())
            .ok_or_else(|| "frame response without jpeg payload".to_string())?;

        use base64::Engine as _;
        let jpeg = base64::engine::general_purpose::STANDARD
            .decode(jpeg_base64)
            .map_err(|err| err.to_string())?;

        Ok(Some(Frame {
            width: response
                .get("width")
                .and_then(|value| value.as_u64())
                .unwrap_or(0) as u32,
            height: response
                .get("height")
                .and_then(|value| value.as_u64())
                .unwrap_or(0) as u32,
            jpeg,
            keypoints: response
                .get("keypoints")
                .cloned()
                .unwrap_or(serde_json::Value::Null),
            inference_ms: response
                .get("inferenceMs")
                .and_then(|value| value.as_f64())
                .unwrap_or(0.0),
            preprocess_ms: response
                .get("preprocessMs")
                .and_then(|value| value.as_f64())
                .unwrap_or(0.0),
            provider: response
                .get("provider")
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .to_string(),
        }))
    }

    /// Stops the capture and releases the camera.
    #[cfg(mobile)]
    pub fn stop_capture(&self) -> Result<(), String> {
        self.0
            .run_mobile_plugin::<serde_json::Value>("stopCapture", serde_json::json!({}))
            .map(|_| ())
            .map_err(|err| err.to_string())
    }
}

/// Initializes the plugin. On desktop builds it is inert.
pub fn init<R: Runtime>() -> TauriPlugin<R> {
    PluginBuilder::new("exergame-camera")
        .setup(|app, api| {
            #[cfg(target_os = "android")]
            {
                let handle = api.register_android_plugin(PLUGIN_IDENTIFIER, "ExergameCameraPlugin")?;
                app.manage(Camera(handle));
            }
            #[cfg(not(target_os = "android"))]
            {
                // Desktop capture is handled by `camera.rs` (nokhwa); nothing
                // to manage here.
                let _ = app;
                let _ = api;
            }

            Ok(())
        })
        .build()
}
