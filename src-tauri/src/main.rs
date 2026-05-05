// Prevents additional console window on Windows in release.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod hardware;
mod ollama;
mod cli;


#[tauri::command]
async fn detect_hardware() -> Result<hardware::HardwareProfile, String> {
    hardware::detect().map_err(|e| e.to_string())
}

#[tauri::command]
async fn ollama_pull(model: String, window: tauri::WebviewWindow) -> Result<(), String> {
    ollama::pull(&model, &window).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn ollama_ensure_running() -> Result<(), String> {
    ollama::ensure_running().await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn ollama_installed() -> bool {
    ollama::is_installed()
}

#[tauri::command]
async fn ollama_install() -> Result<(), String> {
    ollama::install().await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn ollama_stop() -> Result<(), String> {
    ollama::stop().await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn ollama_list_models() -> Result<Vec<ollama::ModelInfo>, String> {
    ollama::list_models().await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn ollama_delete_model(name: String) -> Result<(), String> {
    ollama::delete_model(&name).await.map_err(|e| e.to_string())
}

fn main() {
    // If invoked from CLI with arguments, handle as CLI and exit before starting GUI.
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        rt.block_on(async {
            if let Err(e) = cli::run(args[1..].to_vec()).await {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        });
        return;
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_http::init())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            detect_hardware,
            ollama_pull,
            ollama_ensure_running,
            ollama_installed,
            ollama_install,
            ollama_stop,
            ollama_list_models,
            ollama_delete_model,
        ])
        .setup(|app| {
            // Ensure config dir exists on startup.
            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let _ = ensure_config_dir(&app_handle);
            });
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error building tauri application")
        .run(|_app, event| {
            if let tauri::RunEvent::Exit = event {
                let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
                rt.block_on(async { let _ = ollama::stop().await; });
            }
        });
}

fn ensure_config_dir(_app: &tauri::AppHandle) -> anyhow::Result<()> {
    let dir = anyai_dir()?;
    std::fs::create_dir_all(&dir)?;
    std::fs::create_dir_all(dir.join("cache/sources"))?;
    std::fs::create_dir_all(dir.join("cache/manifests"))?;
    Ok(())
}

pub fn anyai_dir() -> anyhow::Result<std::path::PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no home dir"))?;
    Ok(home.join(".anyai"))
}
