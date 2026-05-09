// Prevents additional console window on Windows in release.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod api;
mod api_models;
mod cli;
mod hardware;
mod ollama;
mod preload;
mod remote_ui;
mod resolver;
mod self_update;
mod watcher;

#[cfg(target_os = "windows")]
mod windows;

#[tauri::command]
async fn detect_hardware() -> Result<hardware::HardwareProfile, String> {
    hardware::detect().map_err(|e| e.to_string())
}

#[tauri::command]
async fn ollama_pull(model: String, window: tauri::WebviewWindow) -> Result<(), String> {
    ollama::pull(&model, &window)
        .await
        .map_err(|e| e.to_string())
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

#[tauri::command]
async fn preload_modes(
    modes: Vec<String>,
    track: bool,
    warm: bool,
    window: tauri::WebviewWindow,
) -> Result<(), String> {
    use tauri::Emitter;
    preload::preload(&modes, track, warm, |evt| {
        let _ = window.emit("anyai://preload-progress", &evt);
    })
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
async fn ensure_tracked_models(warm: bool) -> Result<Vec<String>, String> {
    preload::ensure_tracked_models(warm)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn resolve_virtual_model(requested: String) -> Result<String, String> {
    resolver::translate_virtual(&requested)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn ollama_chat(
    model: String,
    messages: serde_json::Value,
    options: Option<serde_json::Value>,
) -> Result<String, String> {
    ollama::chat_once(&model, messages, options)
        .await
        .map_err(|e| e.to_string())
}

/// Effective context window for `model` in tokens. Reads `/api/show` and
/// returns the daemon's `context_length`; the title-bar saturation ring
/// uses this as the denominator.
#[tauri::command]
async fn ollama_model_context(model: String) -> Result<u32, String> {
    ollama::model_context_length(&model)
        .await
        .map_err(|e| e.to_string())
}

/// Streamed counterpart of `ollama_chat`. Emits per-token deltas on the
/// caller-supplied event channel so the GUI can paint incrementally.
///
/// Channel scheme: `anyai://chat-stream/{stream_id}` — the frontend picks
/// the id so it can subscribe before invoking, and so concurrent streams
/// don't collide. Frames carry exactly one of `delta` (visible content),
/// `thinking_delta` (reasoning from thinking models), or `done: true` with
/// a `cancelled` flag set when the stream ended via `ollama_chat_cancel`.
#[tauri::command]
async fn ollama_chat_stream(
    stream_id: String,
    model: String,
    messages: serde_json::Value,
    window: tauri::WebviewWindow,
) -> Result<(), String> {
    use tauri::Emitter;
    let event = format!("anyai://chat-stream/{stream_id}");
    let content_window = window.clone();
    let content_event = event.clone();
    let thinking_window = window.clone();
    let thinking_event = event.clone();
    let done_window = window.clone();
    let done_event = event.clone();
    ollama::chat_stream(
        &stream_id,
        &model,
        messages,
        move |delta| {
            let _ = content_window.emit(&content_event, serde_json::json!({ "delta": delta }));
        },
        move |delta| {
            let _ = thinking_window.emit(
                &thinking_event,
                serde_json::json!({ "thinking_delta": delta }),
            );
        },
        move |outcome| {
            let cancelled = matches!(outcome, ollama::ChatStreamOutcome::Cancelled);
            let _ = done_window.emit(
                &done_event,
                serde_json::json!({ "done": true, "cancelled": cancelled }),
            );
        },
    )
    .await
    .map(|_| ())
    .map_err(|e| e.to_string())
}

/// Abort an in-flight `ollama_chat_stream`. Idempotent: silently no-ops if
/// the id isn't streaming (already finished, never started, etc.).
#[tauri::command]
async fn ollama_chat_cancel(stream_id: String) {
    ollama::cancel_chat(&stream_id).await;
}

// ---------------------------------------------------------------------------
// Remote UI commands
// ---------------------------------------------------------------------------

#[derive(serde::Serialize)]
struct RemoteUiStatus {
    enabled: bool,
    running: bool,
    port: u16,
    lan_ips: Vec<String>,
    remote_active: bool,
}

#[tauri::command]
fn remote_ui_status() -> Result<RemoteUiStatus, String> {
    let cfg = resolver::load_config_value().map_err(|e| e.to_string())?;
    let enabled = cfg["remote_ui"]["enabled"].as_bool().unwrap_or(false);
    let port = cfg["remote_ui"]["port"].as_u64().unwrap_or(1474) as u16;
    Ok(RemoteUiStatus {
        enabled,
        running: remote_ui::is_running(),
        port,
        lan_ips: remote_ui::lan_ipv4_addresses(),
        remote_active: remote_ui::remote_active_now(),
    })
}

#[tauri::command]
async fn remote_ui_set_enabled(enabled: bool, port: Option<u16>) -> Result<RemoteUiStatus, String> {
    let mut cfg = resolver::load_config_value().map_err(|e| e.to_string())?;
    cfg["remote_ui"]["enabled"] = serde_json::json!(enabled);
    let final_port = if let Some(p) = port {
        cfg["remote_ui"]["port"] = serde_json::json!(p);
        p
    } else {
        cfg["remote_ui"]["port"].as_u64().unwrap_or(1474) as u16
    };
    resolver::save_config_value(&cfg).map_err(|e| e.to_string())?;
    if enabled {
        remote_ui::start(final_port)
            .await
            .map_err(|e| e.to_string())?;
    } else {
        remote_ui::stop().await;
    }
    Ok(RemoteUiStatus {
        enabled,
        running: remote_ui::is_running(),
        port: final_port,
        lan_ips: remote_ui::lan_ipv4_addresses(),
        remote_active: remote_ui::remote_active_now(),
    })
}

#[tauri::command]
fn remote_ui_qr(text: String) -> Result<String, String> {
    remote_ui::qr_svg(&text).map_err(|e| e.to_string())
}

/// The local Tauri UI calls this on mount + every 5s so the tracker knows the
/// desktop is open. Without it, only remote heartbeats would register and
/// every remote session would unnecessarily curtain a UI nobody's using.
#[tauri::command]
fn remote_ui_local_heartbeat(session_id: String) {
    remote_ui::register_local_heartbeat(&session_id);
}

/// Disconnect every remote browser. With `disable: true` also persists
/// `remote_ui.enabled = false` and tears down the listening socket so the
/// kicked device can't reconnect at all (matches "Kick & Hide" in the
/// curtain). With `disable: false` the server stays up; the tracker
/// rejects new heartbeats for a brief holdoff window so a quick refresh
/// from the phone doesn't slip past the kick.
#[tauri::command]
async fn remote_ui_kick(disable: bool) -> Result<RemoteUiStatus, String> {
    remote_ui::kick();
    if disable {
        let mut cfg = resolver::load_config_value().map_err(|e| e.to_string())?;
        cfg["remote_ui"]["enabled"] = serde_json::json!(false);
        resolver::save_config_value(&cfg).map_err(|e| e.to_string())?;
        remote_ui::stop().await;
    }
    remote_ui_status()
}

#[tauri::command]
fn update_status() -> Result<self_update::UpdateStatus, String> {
    self_update::status().map_err(|e| e.to_string())
}

#[tauri::command]
async fn update_check_now() -> Result<self_update::CheckOutcome, String> {
    self_update::check_now().await.map_err(|e| e.to_string())
}

/// Relaunch the GUI so `apply_pending_if_any` swaps in the staged binary on
/// next process start. The UI is expected to call this only after a
/// successful check that produced a `Staged` outcome (or if `pending` is
/// already non-null in `update_status`).
#[tauri::command]
fn update_apply_now(app: tauri::AppHandle) {
    app.restart();
}

/// WebKitGTK's DMA-BUF zero-copy renderer produces scrambled / torn frames
/// on Raspberry Pi GPUs under Wayland — the window draws but content is
/// unreadable, looking like the graphics "don't fit on screen." Disabling
/// DMABUF falls back to a software-composited path that renders correctly.
/// We only flip this on Linux + aarch64 because that's where the breakage
/// lives; x86_64 desktops keep the fast path. Honors a user-set value so
/// anyone wanting to re-enable DMABUF on hardware that doesn't have the
/// bug can still do so via `WEBKIT_DISABLE_DMABUF_RENDERER=0 anyai`.
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
fn workaround_pi_webkit_dmabuf() {
    if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
    }
}

fn main() {
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    workaround_pi_webkit_dmabuf();

    // If invoked from CLI with arguments, handle as CLI and exit before starting GUI.
    let args: Vec<String> = std::env::args().collect();
    let cli_mode = args.len() > 1;

    // On Windows the release binary is built as a GUI subsystem app so the
    // GUI launches from Explorer without a console flash. The flip side is
    // that cmd.exe / PowerShell don't connect any stdio when they invoke
    // anyai.exe for a CLI command, so println!/eprintln! go to the void.
    // Attach to the parent console and rewire std handles BEFORE any output
    // (incl. self_update messages) so `anyai status`, `anyai --version`,
    // etc. actually print.
    #[cfg(target_os = "windows")]
    if cli_mode {
        windows::attach_parent_console();
    }

    // First thing every process does: apply any staged self-update so the new
    // binary takes over before we open ports, sockets, or the GUI window.
    self_update::apply_pending_if_any();

    if cli_mode {
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        rt.block_on(async {
            // Race the subcommand against Ctrl-C so we always reach the
            // cleanup line below — `anyai run` blocks on stdin in a sync
            // chat loop, and a bare Ctrl-C there would terminate the
            // process before any Drop or post-await code runs, leaving
            // the spawned `ollama serve` orphaned. Subcommands that
            // install their own Ctrl-C handler (e.g. `anyai serve` for
            // graceful axum shutdown) resolve this race themselves first.
            let result = tokio::select! {
                r = cli::run(args[1..].to_vec()) => r,
                _ = tokio::signal::ctrl_c() => {
                    eprintln!("\nShutting down…");
                    Ok(())
                }
            };
            // Mirrors the GUI's RunEvent::Exit handler. ollama::stop() is a
            // no-op when AnyAI didn't spawn the daemon (the static
            // OLLAMA_PROCESS slot is empty for user-managed installs), so
            // this never disturbs an ollama the user started themselves.
            let _ = ollama::stop().await;
            if let Err(e) = result {
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
            preload_modes,
            ensure_tracked_models,
            resolve_virtual_model,
            ollama_chat,
            ollama_chat_stream,
            ollama_chat_cancel,
            ollama_model_context,
            update_status,
            update_check_now,
            update_apply_now,
            remote_ui_status,
            remote_ui_set_enabled,
            remote_ui_qr,
            remote_ui_local_heartbeat,
            remote_ui_kick,
        ])
        .setup(|app| {
            // If the configured 800x600 window can't fit on this monitor —
            // e.g. the official 7" Pi DSI screen at 800x480 — start
            // maximized so the user doesn't lose the bottom of the UI off
            // the edge of the screen. Compares physical pixels on both
            // sides; the +80 reserves room for a taskbar / dock the
            // monitor reports as part of its full size.
            {
                use tauri::Manager;
                if let Some(window) = app.get_webview_window("main") {
                    if let (Ok(outer), Ok(Some(monitor))) =
                        (window.outer_size(), window.current_monitor())
                    {
                        let m = monitor.size();
                        if outer.width > m.width || outer.height + 80 > m.height {
                            let _ = window.maximize();
                        }
                    }
                }
            }

            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let _ = ensure_config_dir(&app_handle);

                // Start watcher so tracked modes stay current in the GUI session.
                watcher::spawn_background();

                // Optionally start the OpenAI-compat server alongside the GUI.
                if let Ok(cfg) = resolver::load_config_value() {
                    let enabled = cfg["api"]["enabled"].as_bool().unwrap_or(true);
                    if !enabled {
                        return;
                    }
                    let host_str = cfg["api"]["host"].as_str().unwrap_or("127.0.0.1");
                    let host: std::net::IpAddr = match host_str.parse() {
                        Ok(h) => h,
                        Err(_) => "127.0.0.1".parse().unwrap(),
                    };
                    let port = cfg["api"]["port"].as_u64().unwrap_or(1473) as u16;
                    let cors_all = cfg["api"]["cors_allow_all"].as_bool().unwrap_or(false);
                    let bearer = cfg["api"]["bearer_token"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .map(str::to_string);
                    tokio::spawn(async move {
                        if let Err(e) = api::serve(host, port, cors_all, bearer).await {
                            eprintln!("api server failed: {e}");
                        }
                    });

                    // Auto-start the remote UI server if the user previously enabled it.
                    let remote_enabled = cfg["remote_ui"]["enabled"].as_bool().unwrap_or(false);
                    let remote_port = cfg["remote_ui"]["port"].as_u64().unwrap_or(1474) as u16;
                    if remote_enabled {
                        tokio::spawn(async move {
                            if let Err(e) = remote_ui::start(remote_port).await {
                                eprintln!("remote-ui start failed: {e}");
                            }
                        });
                    }
                }
            });

            // Bridge `remote_ui::subscribe_active()` → Tauri event so the
            // GUI can flip the curtain on without polling. Runs for the
            // lifetime of the app.
            {
                use tauri::Emitter;
                let app_handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    let mut rx = remote_ui::subscribe_active();
                    let initial = *rx.borrow();
                    let _ = app_handle.emit("anyai://remote-active-changed", initial);
                    loop {
                        if rx.changed().await.is_err() {
                            break;
                        }
                        let active = *rx.borrow();
                        let _ = app_handle.emit("anyai://remote-active-changed", active);
                    }
                });
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error building tauri application")
        .run(|_app, event| {
            if let tauri::RunEvent::Exit = event {
                let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
                rt.block_on(async {
                    let _ = ollama::stop().await;
                });
            }
        });
}

fn ensure_config_dir(_app: &tauri::AppHandle) -> anyhow::Result<()> {
    let dir = anyai_dir()?;
    std::fs::create_dir_all(&dir)?;
    std::fs::create_dir_all(dir.join("cache/manifests"))?;
    std::fs::create_dir_all(dir.join("updates"))?;
    Ok(())
}

pub fn anyai_dir() -> anyhow::Result<std::path::PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no home dir"))?;
    Ok(home.join(".anyai"))
}
