use nax_core::Preset;
use serde::Serialize;

#[derive(Serialize)]
struct CoreInfo {
    version: &'static str,
    extension: &'static str,
    format_version: u16,
    presets: Vec<&'static str>,
}

/// Basic info from the core library, used by the UI to show it's connected.
#[tauri::command]
fn core_info() -> CoreInfo {
    CoreInfo {
        version: nax_core::VERSION,
        extension: nax_core::EXTENSION,
        format_version: nax_core::FORMAT_VERSION,
        presets: Preset::ALL.iter().map(|p| p.name()).collect(),
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![core_info])
        .run(tauri::generate_context!())
        .expect("error while running Nanarchive");
}
