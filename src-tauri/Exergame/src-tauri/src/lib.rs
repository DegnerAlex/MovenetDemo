use nokhwa::{
    pixel_format::RgbFormat,
    utils::{CameraIndex, RequestedFormat, RequestedFormatType},
    Camera,
};
use tauri::ipc::{Channel, Response};
use std::thread;

#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

#[tauri::command]
fn start_rust_camera(on_frame: Channel<Response>) {
    thread::spawn(move || {
        let index = CameraIndex::Index(0);
        let requested = RequestedFormat::new::<RgbFormat>(
            RequestedFormatType::AbsoluteHighestFrameRate
        );

        let mut camera = Camera::new(index, requested).expect("Не удалось открыть камеру");
        camera.open_stream().expect("Не удалось запустить поток");

        loop {
            if let Ok(frame) = camera.frame() {
                // Декодируем в RGB или отправляем сырые байты (например, MJPEG)
                let decoded = frame.decode_image::<RgbFormat>().unwrap();
                let resolution = decoded.resolution();
                let raw_bytes = decoded.into_raw();

                // Первые 8 байт — ширина и высота кадра (u32 big-endian), далее RGB-пиксели.
                let mut payload = Vec::with_capacity(8 + raw_bytes.len());
                payload.extend_from_slice(&resolution.width().to_be_bytes());
                payload.extend_from_slice(&resolution.height().to_be_bytes());
                payload.extend_from_slice(&raw_bytes);

                // Отправка кадра во фронтенд через канал
                if on_frame.send(Response::new(payload)).is_err() {
                    break; // Фронтенд закрыл канал
                }
            }
        }
    });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_log::Builder::default().build())
        .invoke_handler(tauri::generate_handler![
            greet,
            start_rust_camera
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}