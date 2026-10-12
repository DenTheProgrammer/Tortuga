// Hide the console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod assoc;
mod players;
mod shutdown;
mod updates;

use std::{net::SocketAddr, sync::Arc, time::Duration};

use parking_lot::Mutex;
use serde::Serialize;
use tauri::{
    AppHandle, Emitter, Manager, RunEvent, WindowEvent,
    menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};
use tauri_plugin_notification::NotificationExt;
use tortuga_core::{Engine, Event, net::Route};

#[derive(Clone, Serialize)]
struct BackendInfo {
    base: String,
    token: String,
}

struct AppState {
    engine: Arc<Engine>,
    backend: BackendInfo,
    /// Sources (magnets / .torrent paths) received before the UI subscribed.
    pending: Mutex<Vec<String>>,
    ui_ready: Mutex<bool>,
    shutdown: shutdown::Shutdown,
}

fn is_source(arg: &str) -> bool {
    let a = arg.trim();
    a.starts_with("magnet:") || a.to_lowercase().ends_with(".torrent")
}

fn sources_from_args(args: impl IntoIterator<Item = String>) -> Vec<String> {
    args.into_iter().skip(1).filter(|a| is_source(a)).collect()
}

fn deliver_sources(app: &AppHandle, sources: Vec<String>) {
    if sources.is_empty() {
        return;
    }
    let state = app.state::<AppState>();
    show_main(app);
    if *state.ui_ready.lock() {
        for s in sources {
            let _ = app.emit("open-source", s);
        }
    } else {
        state.pending.lock().extend(sources);
    }
}

fn show_main(app: &AppHandle) {
    if app
        .try_state::<AppState>()
        .is_some_and(|s| s.shutdown.is_requested())
    {
        return;
    }
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

fn toggle_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        if w.is_visible().unwrap_or(false) && !w.is_minimized().unwrap_or(false) {
            let _ = w.hide();
        } else {
            show_main(app);
        }
    }
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

#[tauri::command]
fn backend_info(state: tauri::State<AppState>) -> BackendInfo {
    state.backend.clone()
}

#[tauri::command]
fn take_pending_sources(state: tauri::State<AppState>) -> Vec<String> {
    *state.ui_ready.lock() = true;
    std::mem::take(&mut *state.pending.lock())
}

#[tauri::command]
async fn detect_players() -> Vec<players::PlayerInfo> {
    tauri::async_runtime::spawn_blocking(players::detect)
        .await
        .unwrap_or_default()
}

#[tauri::command]
fn open_in_player(
    url: String,
    title: String,
    player: String,
    player_path: Option<String>,
) -> Result<(), String> {
    players::open(&url, &title, &player, player_path.as_deref()).map_err(|e| format!("{e:#}"))
}

#[tauri::command]
fn default_handler_status() -> assoc::HandlerStatus {
    assoc::status()
}

#[tauri::command]
fn make_default_handler() -> Result<(), String> {
    assoc::make_default().map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Tray & notifications
// ---------------------------------------------------------------------------

fn tr(lang: &str, ru: &'static str, en: &'static str) -> &'static str {
    if lang == "ru" { ru } else { en }
}

fn fmt_speed(bps: u64) -> String {
    let kib = bps as f64 / 1024.0;
    if kib >= 1024.0 {
        format!("{:.1} MB/s", kib / 1024.0)
    } else {
        format!("{kib:.0} KB/s")
    }
}

fn setup_tray(app: &AppHandle, engine: Arc<Engine>) -> tauri::Result<()> {
    let lang = engine.settings().language;
    let show = MenuItem::with_id(
        app,
        "show",
        tr(&lang, "Открыть Tortuga", "Open Tortuga"),
        true,
        None::<&str>,
    )?;
    let resume = MenuItem::with_id(
        app,
        "resume_all",
        tr(&lang, "Запустить все", "Resume all"),
        true,
        None::<&str>,
    )?;
    let pause = MenuItem::with_id(
        app,
        "pause_all",
        tr(&lang, "Приостановить все", "Pause all"),
        true,
        None::<&str>,
    )?;
    let turtle = CheckMenuItem::with_id(
        app,
        "turtle",
        tr(&lang, "Режим черепахи", "Turtle mode"),
        true,
        engine.settings().alt_speed_enabled,
        None::<&str>,
    )?;
    let quit = MenuItem::with_id(app, "quit", tr(&lang, "Выход", "Quit"), true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &show,
            &PredefinedMenuItem::separator(app)?,
            &resume,
            &pause,
            &turtle,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )?;

    let e = engine.clone();
    let turtle_item = turtle.clone();
    let tray = TrayIconBuilder::with_id("main")
        .icon(app.default_window_icon().cloned().expect("app icon"))
        .tooltip("Tortuga")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(move |app, event| {
            if app.state::<AppState>().shutdown.is_requested() {
                return;
            }
            let e = e.clone();
            match event.id.as_ref() {
                "show" => show_main(app),
                "resume_all" => {
                    tauri::async_runtime::spawn(async move {
                        let _ = e.resume_all().await;
                    });
                }
                "pause_all" => {
                    tauri::async_runtime::spawn(async move {
                        let _ = e.pause_all().await;
                    });
                }
                "turtle" => {
                    let on = !e.settings().alt_speed_enabled;
                    let _ = e.set_alt_speed(on);
                    let _ = turtle_item.set_checked(on);
                }
                "quit" => crate::quit(app),
                _ => {}
            }
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                toggle_main(tray.app_handle());
            }
        })
        .build(app)?;

    // Keep tooltip and the turtle checkbox in sync.
    let e = engine.clone();
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            if app.state::<AppState>().shutdown.is_requested() {
                break;
            }
            let st = e.state();
            let active = st
                .torrents
                .iter()
                .filter(|t| t.down_speed > 0 || t.up_speed > 0)
                .count();
            let tip = format!(
                "Tortuga\n↓ {}  ↑ {}\n{}: {active}",
                fmt_speed(st.session.down_speed),
                fmt_speed(st.session.up_speed),
                tr(&e.settings().language, "Активных", "Active"),
            );
            let _ = tray.set_tooltip(Some(tip));
            let _ = turtle.set_checked(e.settings().alt_speed_enabled);
        }
    });
    Ok(())
}

fn spawn_notifications(app: AppHandle, engine: Arc<Engine>) {
    let mut rx = engine.subscribe();
    tauri::async_runtime::spawn(async move {
        loop {
            let ev = match rx.recv().await {
                Ok(ev) => ev,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            };
            let s = engine.settings();
            let lang = s.language.as_str();
            let (title, body) = match ev {
                Event::Completed { name, .. } if s.notify_on_complete => {
                    (tr(lang, "Загрузка завершена", "Download complete").to_owned(), name)
                }
                Event::RssDownloaded { title, feed } => (format!("RSS: {feed}"), title),
                Event::FilesMissing { name, all, .. } => (
                    name,
                    if all {
                        tr(
                            lang,
                            "Файлы удалены с диска. Торрент на паузе — нажмите «Запустить», чтобы скачать заново.",
                            "Files were deleted. The torrent is paused — press Start to download again.",
                        )
                    } else {
                        tr(
                            lang,
                            "Часть файлов удалена с диска — они больше не качаются. Включить можно во вкладке «Файлы».",
                            "Some files were deleted — they won't be downloaded again. Re-enable them in the Files tab.",
                        )
                    }
                    .to_owned(),
                ),
                Event::NetworkChanged { status } => match status.route {
                    Route::Direct if status.vpn_active && status.reason.starts_with("auto") => (
                        "Tortuga".to_owned(),
                        tr(
                            lang,
                            "VPN мешал торрентам — переключились в обход VPN",
                            "The VPN was hurting torrents — now bypassing it",
                        )
                        .to_owned(),
                    ),
                    Route::Blocked => (
                        "Tortuga".to_owned(),
                        tr(lang, "VPN отключился — загрузки на паузе", "VPN is down — downloads paused").to_owned(),
                    ),
                    _ => continue,
                },
                _ => continue,
            };
            let _ = app.notification().builder().title(title).body(body).show();
        }
    });
}

// ---------------------------------------------------------------------------

/// Bounded cleanup also used before launching the update installer.
async fn stop_engine(app: &AppHandle) {
    let engine = app.state::<AppState>().engine.clone();
    if !shutdown::stop_within(
        move || tauri::async_runtime::block_on(engine.shutdown()),
        shutdown::GRACE_PERIOD,
    )
    .await
    {
        tracing::warn!("engine shutdown exceeded its grace period or failed");
    }
}

fn quit(app: &AppHandle) {
    if !app.state::<AppState>().shutdown.begin() {
        return;
    }
    // Arm this before any engine or window work. Even a stuck native teardown
    // cannot keep this process alive indefinitely.
    shutdown::arm_watchdog(shutdown::HARD_DEADLINE);
    let ui = app.clone();
    let _ = app.run_on_main_thread(move || {
        for window in ui.webview_windows().values() {
            let _ = window.hide();
        }
        if let Some(tray) = ui.tray_by_id("main") {
            let _ = tray.set_visible(false);
        }
    });
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let started = std::time::Instant::now();
        tracing::info!("shutdown requested");
        stop_engine(&app).await;
        tracing::info!(
            elapsed_ms = started.elapsed().as_millis(),
            "shutdown cleanup finished"
        );
        app.state::<AppState>().shutdown.finish();
        app.exit(0);
    });
}

fn main() {
    // Run by the installer's "open .torrent and magnet links with Tortuga"
    // checkbox: register and show Windows' Default apps page, no window.
    if std::env::args().any(|a| a == "--make-default") {
        if let Err(e) = assoc::make_default() {
            eprintln!("can't register Tortuga as the default app: {e}");
        }
        return;
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,librqbit=warn")),
        )
        .init();

    let initial_sources = sources_from_args(std::env::args());
    // Isolated profiles and a localhost API descriptor support reproducible
    // diagnostics without altering the user's torrent list or settings.
    let args: Vec<String> = std::env::args().collect();
    let arg_path = |flag: &str| {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .map(std::path::PathBuf::from)
    };
    let profile_dir = arg_path("--data-dir");
    let api_info_file = arg_path("--api-info-file");
    let isolated = profile_dir.is_some();

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            let sources = sources_from_args(args);
            if sources.is_empty() {
                show_main(app);
            } else {
                deliver_sources(app, sources);
            }
        }))
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .manage(updates::UpdateState::default())
        .invoke_handler(tauri::generate_handler![
            updates::pending_update,
            updates::check_update,
            updates::install_update,
            backend_info,
            take_pending_sources,
            detect_players,
            open_in_player,
            default_handler_status,
            make_default_handler,
        ])
        .setup(move |app| {
            let handle = app.handle().clone();
            let data_dir = profile_dir.clone().unwrap_or_else(|| {
                app.path()
                    .app_data_dir()
                    .unwrap_or_else(|_| tortuga_core::default_data_dir())
            });

            let (engine, addr) = tauri::async_runtime::block_on(async {
                let engine = Engine::new(data_dir).await?;
                let (addr, _task) = tortuga_core::server::serve(
                    engine.clone(),
                    SocketAddr::from(([127, 0, 0, 1], 0)),
                    None,
                )
                .await?;
                anyhow::Ok((engine, addr))
            })?;

            let settings = engine.settings();
            if let Some(path) = api_info_file.as_ref() {
                tortuga_core::util::write_json_atomic(
                    path,
                    &BackendInfo {
                        base: format!("http://{addr}"),
                        token: engine.token.clone(),
                    },
                )?;
            }
            app.manage(AppState {
                backend: BackendInfo {
                    base: format!("http://{addr}"),
                    token: engine.token.clone(),
                },
                engine: engine.clone(),
                pending: Mutex::new(initial_sources),
                ui_ready: Mutex::new(false),
                shutdown: shutdown::Shutdown::default(),
            });

            if !isolated && let Err(e) = assoc::register(true) {
                tracing::warn!("can't register file associations: {e}");
            }

            // macOS delivers magnet links as URL events instead of argv.
            #[cfg(target_os = "macos")]
            {
                use tauri_plugin_deep_link::DeepLinkExt;
                let h = handle.clone();
                app.deep_link().on_open_url(move |event| {
                    let sources = event.urls().iter().map(|u| u.to_string()).collect();
                    deliver_sources(&h, sources);
                });
            }

            setup_tray(&handle, engine.clone())?;
            spawn_notifications(handle.clone(), engine.clone());
            if !isolated {
                updates::spawn(handle.clone());
            }

            if !settings.start_minimized || !app.state::<AppState>().pending.lock().is_empty() {
                show_main(&handle);
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                let state = window.state::<AppState>();
                api.prevent_close();
                if state.shutdown.is_requested() {
                    return;
                }
                if state.engine.settings().close_to_tray {
                    let _ = window.hide();
                } else {
                    quit(window.app_handle());
                }
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building Tortuga");

    app.run(|app, event| {
        if let RunEvent::ExitRequested { api, .. } = event
            && !app.state::<AppState>().shutdown.is_ready()
        {
            api.prevent_exit();
            quit(app);
        }
    });
}
