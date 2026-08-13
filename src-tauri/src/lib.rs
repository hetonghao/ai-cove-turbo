//! Native core for AI Cove Turbo.

#![allow(clippy::unreachable)] // Tauri's command macro emits an internal unreachable branch.
#![allow(clippy::redundant_pub_crate)] // Private modules expose crate-scoped test seams.

mod codex_thread_title;
pub(crate) mod config;
pub(crate) mod proxy;
pub(crate) mod runtime;

#[cfg(test)]
mod benchmark;
#[cfg(test)]
mod codex_thread_title_tests;
#[cfg(test)]
mod transport_ack_benchmark;

#[cfg(test)]
mod updater_tests {
    use super::{DEFAULT_UPDATER_ENDPOINT, UPDATER_ENDPOINT, updater_endpoint};

    #[test]
    fn updater_endpoint_matches_the_compile_time_value() {
        assert_eq!(
            updater_endpoint().map(|endpoint| endpoint.to_string()),
            Ok(UPDATER_ENDPOINT.to_owned()),
        );
    }

    #[test]
    fn production_endpoint_remains_the_fallback() {
        assert_eq!(
            DEFAULT_UPDATER_ENDPOINT,
            "https://ai-cove.com/downloads/turbo/latest.json",
        );
    }
}

use std::{
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use proxy::ConnectionSnapshot;
use runtime::{AppRuntime, AppStatus, RuntimePaths};
#[cfg(target_os = "macos")]
use tauri::menu::{IconMenuItem, NativeIcon};
use tauri::{
    AppHandle, Manager, RunEvent, State, WindowEvent,
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::TrayIconBuilder,
};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt as AutostartExt};
use tauri_plugin_updater::UpdaterExt;

const MAIN_WINDOW_LABEL: &str = "main";
const TRAY_ID: &str = "ai-cove-turbo";
const OPEN_MENU_ID: &str = "open";
const OPEN_AI_COVE_MENU_ID: &str = "open-ai-cove";
const QUIT_MENU_ID: &str = "quit";
const AI_COVE_URL: &str = "https://ai-cove.com";
const DEFAULT_UPDATER_ENDPOINT: &str = "https://ai-cove.com/downloads/turbo/latest.json";
const UPDATER_ENDPOINT: &str = match option_env!("TURBO_UPDATER_ENDPOINT") {
    Some(endpoint) if !endpoint.is_empty() => endpoint,
    _ => DEFAULT_UPDATER_ENDPOINT,
};

/// Starts the Turbo desktop application.
///
/// # Errors
/// Returns an error when Tauri cannot initialize or run the application.
pub fn run() -> tauri::Result<()> {
    let updater = updater_public_key()
        .map_or_else(tauri_plugin_updater::Builder::new, |public_key| {
            tauri_plugin_updater::Builder::new().pubkey(public_key)
        });
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            Some(vec!["--background"]),
        ))
        .plugin(updater.build())
        .invoke_handler(tauri::generate_handler![
            get_app_status,
            get_connection_snapshot,
            get_codex_thread_info,
            open_ai_cove,
            set_compression,
            set_websocket,
            set_autostart,
            set_dock_visible,
            restart_codex,
            retry_takeover,
            set_ai_cove_upstream,
            confirm_non_ai_cove,
            check_for_updates,
            install_update,
        ])
        .setup(|app| {
            let home = app.path().home_dir()?;
            let data_dir = app.path().app_data_dir()?;
            let runtime = AppRuntime::new(RuntimePaths {
                config_path: home.join(".codex/config.toml"),
                data_dir,
            });
            app.manage(Arc::clone(&runtime));
            initialize_desktop_preferences(app.handle(), &runtime);
            install_tray(app)?;
            if !background_requested() {
                show_main_window(app.handle());
            }
            tauri::async_runtime::spawn(async move {
                runtime.initialize().await;
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
            if matches!(event, WindowEvent::Focused(true)) {
                let runtime = Arc::clone(window.state::<Arc<AppRuntime>>().inner());
                tauri::async_runtime::spawn(async move {
                    runtime.verify_codex_restart().await;
                });
            }
        })
        .build(tauri::generate_context!())?;

    app.run(|app_handle, event| match event {
        #[cfg(target_os = "macos")]
        RunEvent::Reopen { .. } => show_main_window(app_handle),
        RunEvent::ExitRequested { api, .. } => {
            let runtime = Arc::clone(app_handle.state::<Arc<AppRuntime>>().inner());
            if tauri::async_runtime::block_on(runtime.shutdown()).is_err() {
                api.prevent_exit();
                show_main_window(app_handle);
            }
        }
        _ => {}
    });
    Ok(())
}

fn install_tray(app: &tauri::App) -> tauri::Result<()> {
    #[cfg(target_os = "macos")]
    let status = IconMenuItem::with_native_icon(
        app,
        "AI Cove Turbo 正在运行",
        false,
        Some(NativeIcon::StatusAvailable),
        None::<&str>,
    )?;
    #[cfg(not(target_os = "macos"))]
    let status = MenuItem::new(app, "AI Cove Turbo 正在运行", false, None::<&str>)?;
    let status_separator = PredefinedMenuItem::separator(app)?;
    let open = MenuItem::with_id(app, OPEN_MENU_ID, "打开主界面", true, None::<&str>)?;
    let open_ai_cove = MenuItem::with_id(
        app,
        OPEN_AI_COVE_MENU_ID,
        "打开 AI Cove",
        true,
        None::<&str>,
    )?;
    let version = MenuItem::new(
        app,
        concat!("版本 ", env!("CARGO_PKG_VERSION")),
        false,
        None::<&str>,
    )?;
    let quit_separator = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, QUIT_MENU_ID, "退出 Turbo", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &status,
            &status_separator,
            &open,
            &open_ai_cove,
            &version,
            &quit_separator,
            &quit,
        ],
    )?;
    let tray = TrayIconBuilder::with_id(TRAY_ID)
        .menu(&menu)
        .show_menu_on_left_click(true)
        .tooltip("AI Cove Turbo")
        .on_menu_event(|app, event| match event.id.as_ref() {
            OPEN_MENU_ID => show_main_window(app),
            OPEN_AI_COVE_MENU_ID => {
                let _open_result = open_ai_cove_url();
            }
            QUIT_MENU_ID => quit_after_restore(app),
            _ => {}
        });
    #[cfg(target_os = "macos")]
    let tray = tray
        .icon(tauri::image::Image::from_bytes(include_bytes!(
            "../icons/tray-template.png"
        ))?)
        .icon_as_template(true);
    #[cfg(not(target_os = "macos"))]
    let tray = match app.default_window_icon() {
        Some(icon) => tray.icon(icon.clone()).title("Turbo"),
        None => tray.title("Turbo"),
    };
    tray.build(app)?;
    Ok(())
}

fn open_ai_cove_url() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    Command::new("open")
        .arg(AI_COVE_URL)
        .spawn()
        .map_err(|error| error.to_string())?;
    #[cfg(target_os = "windows")]
    Command::new("cmd")
        .args(["/C", "start", "", AI_COVE_URL])
        .spawn()
        .map_err(|error| error.to_string())?;
    #[cfg(all(unix, not(target_os = "macos")))]
    Command::new("xdg-open")
        .arg(AI_COVE_URL)
        .spawn()
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn initialize_desktop_preferences(app: &AppHandle, runtime: &Arc<AppRuntime>) {
    let autolaunch = app.autolaunch();
    let enabled = if runtime.autostart_initialized() {
        autolaunch.is_enabled().unwrap_or(false)
    } else {
        autolaunch.enable().is_ok()
    };
    runtime.set_autostart_state(enabled, true);

    #[cfg(target_os = "macos")]
    {
        if !runtime.dock_initialized() {
            runtime.set_dock_state(true);
        }
        let visible = runtime.dock_visible();
        let policy = if visible {
            tauri::ActivationPolicy::Regular
        } else {
            tauri::ActivationPolicy::Accessory
        };
        let _ = app.set_activation_policy(policy);
        let _ = app.set_dock_visibility(visible);
    }
}

fn show_main_window(app: &AppHandle) {
    let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) else {
        return;
    };
    let _ = window.unminimize();
    let _ = window.show();
    let _ = window.set_focus();
}

fn background_requested() -> bool {
    std::env::args_os().any(|argument| argument == "--background")
}

fn quit_after_restore(app: &AppHandle) {
    let runtime = Arc::clone(app.state::<Arc<AppRuntime>>().inner());
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if runtime.shutdown().await.is_ok() {
            app.exit(0);
        } else {
            show_main_window(&app);
        }
    });
}

#[tauri::command]
async fn get_app_status(runtime: State<'_, Arc<AppRuntime>>) -> Result<AppStatus, String> {
    Ok(runtime.status().await)
}

#[tauri::command]
async fn get_connection_snapshot(
    runtime: State<'_, Arc<AppRuntime>>,
) -> Result<ConnectionSnapshot, String> {
    Ok(runtime.connection_snapshot().await)
}

#[tauri::command]
async fn get_codex_thread_info(
    app: AppHandle,
    thread_id: String,
) -> Option<codex_thread_title::CodexThreadInfo> {
    let home = app.path().home_dir().ok()?;
    let codex_home = std::env::var_os("CODEX_HOME")
        .map_or_else(|| home.join(".codex"), std::path::PathBuf::from);
    codex_thread_title::read(codex_home.join("state_5.sqlite"), thread_id).await
}

#[tauri::command]
fn open_ai_cove() -> Result<(), String> {
    open_ai_cove_url()
}

#[tauri::command]
async fn set_compression(
    runtime: State<'_, Arc<AppRuntime>>,
    enabled: bool,
) -> Result<AppStatus, String> {
    runtime.set_compression(enabled);
    Ok(runtime.status().await)
}

#[tauri::command]
async fn set_websocket(
    runtime: State<'_, Arc<AppRuntime>>,
    enabled: bool,
) -> Result<AppStatus, String> {
    runtime
        .set_websocket(enabled)
        .map_err(|error| error.to_string())?;
    Ok(runtime.status().await)
}

#[tauri::command]
async fn set_autostart(
    app: AppHandle,
    runtime: State<'_, Arc<AppRuntime>>,
    enabled: bool,
) -> Result<AppStatus, String> {
    let result = if enabled {
        app.autolaunch().enable()
    } else {
        app.autolaunch().disable()
    };
    result.map_err(|error| error.to_string())?;
    runtime.set_autostart_state(enabled, true);
    Ok(runtime.status().await)
}

#[tauri::command]
async fn set_dock_visible(
    app: AppHandle,
    runtime: State<'_, Arc<AppRuntime>>,
    visible: bool,
) -> Result<AppStatus, String> {
    #[cfg(target_os = "macos")]
    {
        let policy = if visible {
            tauri::ActivationPolicy::Regular
        } else {
            tauri::ActivationPolicy::Accessory
        };
        app.set_activation_policy(policy)
            .map_err(|error| error.to_string())?;
        app.set_dock_visibility(visible)
            .map_err(|error| error.to_string())?;
        runtime.set_dock_state(visible);
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (app, visible);
    Ok(runtime.status().await)
}

#[tauri::command]
async fn restart_codex(runtime: State<'_, Arc<AppRuntime>>) -> Result<AppStatus, String> {
    tauri::async_runtime::spawn_blocking(restart_codex_desktop)
        .await
        .map_err(|error| error.to_string())??;
    runtime.mark_desktop_restarted();
    Ok(runtime.status().await)
}

#[tauri::command]
async fn retry_takeover(runtime: State<'_, Arc<AppRuntime>>) -> Result<AppStatus, String> {
    runtime.retry_takeover().await;
    Ok(runtime.status().await)
}

#[tauri::command]
async fn set_ai_cove_upstream(runtime: State<'_, Arc<AppRuntime>>) -> Result<AppStatus, String> {
    runtime
        .set_ai_cove_upstream()
        .await
        .map_err(|error| error.to_string())?;
    Ok(runtime.status().await)
}

#[tauri::command]
async fn confirm_non_ai_cove(runtime: State<'_, Arc<AppRuntime>>) -> Result<AppStatus, String> {
    runtime.confirm_non_ai_cove().await;
    Ok(runtime.status().await)
}

#[tauri::command]
async fn check_for_updates(
    app: AppHandle,
    runtime: State<'_, Arc<AppRuntime>>,
) -> Result<AppStatus, String> {
    if updater_public_key().is_none() {
        runtime.set_update_status("unconfigured", "当前构建未注入 Turbo 独立更新公钥", 0);
        return Ok(runtime.status().await);
    }
    runtime.set_update_status("checking", "正在检查更新", 0);
    let endpoint = updater_endpoint()?;
    let updater = app
        .updater_builder()
        .endpoints(vec![endpoint])
        .map_err(|error| error.to_string())?
        .build()
        .map_err(|error| error.to_string())?;
    match updater.check().await.map_err(|error| error.to_string())? {
        Some(update) => {
            runtime.set_update_status("available", &format!("发现新版本 {}", update.version), 0);
        }
        None => runtime.set_update_status("current", "当前已是最新版本", 100),
    }
    Ok(runtime.status().await)
}

#[tauri::command]
async fn install_update(
    app: AppHandle,
    runtime: State<'_, Arc<AppRuntime>>,
) -> Result<AppStatus, String> {
    if updater_public_key().is_none() {
        runtime.set_update_status("unconfigured", "当前构建未注入 Turbo 独立更新公钥", 0);
        return Ok(runtime.status().await);
    }
    let endpoint = updater_endpoint()?;
    let updater = app
        .updater_builder()
        .endpoints(vec![endpoint])
        .map_err(|error| error.to_string())?
        .build()
        .map_err(|error| error.to_string())?;
    let Some(update) = updater.check().await.map_err(|error| error.to_string())? else {
        runtime.set_update_status("current", "当前已是最新版本", 100);
        return Ok(runtime.status().await);
    };
    runtime.set_update_status("downloading", "正在下载签名更新", 0);
    let progress_runtime = Arc::clone(runtime.inner());
    let downloaded = Arc::new(AtomicU64::new(0));
    let progress_downloaded = Arc::clone(&downloaded);
    let bytes = match update
        .download(
            move |chunk, total| {
                let downloaded =
                    progress_downloaded.fetch_add(chunk as u64, Ordering::Relaxed) + chunk as u64;
                let progress = total
                    .filter(|total| *total > 0)
                    .map_or(0, |total| ((downloaded * 100) / total).min(99) as u8);
                progress_runtime.set_update_status("downloading", "正在下载签名更新", progress);
            },
            || {},
        )
        .await
    {
        Ok(bytes) => bytes,
        Err(error) => {
            runtime.set_update_status("error", &format!("下载失败，可重新下载：{error}"), 0);
            return Err(error.to_string());
        }
    };
    runtime.set_update_status("installing", "签名已验证，正在安装", 100);
    if let Err(error) = runtime.shutdown().await {
        runtime.set_update_status(
            "error",
            &format!("恢复 Codex 配置失败，已取消安装：{error}"),
            0,
        );
        return Err(error.to_string());
    }
    if let Err(error) = update.install(bytes) {
        runtime.set_update_status("error", &format!("安装失败：{error}"), 0);
        runtime.resume_after_failed_update().await;
        return Err(error.to_string());
    }
    app.restart();
}

fn updater_endpoint() -> Result<url::Url, String> {
    url::Url::parse(UPDATER_ENDPOINT).map_err(|error| error.to_string())
}

fn updater_public_key() -> Option<&'static str> {
    option_env!("TURBO_UPDATER_PUBLIC_KEY").filter(|key| !key.trim().is_empty())
}

#[cfg(target_os = "macos")]
fn restart_codex_desktop() -> Result<(), String> {
    let script = r#"if application id "com.openai.codex" is running then
tell application id "com.openai.codex" to quit
end if"#;
    let status = Command::new("/usr/bin/osascript")
        .args(["-e", script])
        .status()
        .map_err(|error| error.to_string())?;
    if !status.success() {
        return Err("无法优雅退出 Codex Desktop".to_owned());
    }
    std::thread::sleep(Duration::from_millis(500));
    let status = Command::new("/usr/bin/open")
        .args(["-b", "com.openai.codex"])
        .status()
        .map_err(|error| error.to_string())?;
    status
        .success()
        .then_some(())
        .ok_or_else(|| "无法重新打开 Codex Desktop".to_owned())
}

#[cfg(target_os = "windows")]
fn restart_codex_desktop() -> Result<(), String> {
    let script = r#"$path = Join-Path $env:LOCALAPPDATA 'Programs\Codex\Codex.exe'
if (-not (Test-Path $path)) { throw '未找到 Codex Desktop 可执行文件' }
$processes = Get-Process -Name Codex -ErrorAction SilentlyContinue | Where-Object { $_.Path -and [string]::Equals($_.Path, $path, [System.StringComparison]::OrdinalIgnoreCase) }
$processes | ForEach-Object {
  [void]$_.CloseMainWindow()
  if (-not $_.WaitForExit(5000)) { throw 'Codex Desktop 未能优雅退出' }
}
Start-Process -FilePath $path"#;
    let status = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .status()
        .map_err(|error| error.to_string())?;
    status
        .success()
        .then_some(())
        .ok_or_else(|| "无法重启 Codex Desktop".to_owned())
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn restart_codex_desktop() -> Result<(), String> {
    Err("当前平台不支持重启 Codex Desktop".to_owned())
}
