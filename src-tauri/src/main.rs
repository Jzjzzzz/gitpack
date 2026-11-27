#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod anchors;
mod engine;
mod remote;

use serde::Serialize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{Manager, State};

#[derive(Default)]
struct PreviewState {
    next: AtomicU64,
    latest: Mutex<Option<(String, Arc<ScopedPreview>)>>,
}

struct ScopedPreview {
    scope: String,
    observed: anchors::ScopeAnchors,
    preview: Arc<engine::Preview>,
}

#[derive(Serialize)]
struct ScanResult {
    id: String,
    #[serde(flatten)]
    preview: Arc<engine::Preview>,
}

#[tauri::command]
async fn git_version() -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(engine::git_version)
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn scan_range(
    app: tauri::AppHandle,
    state: State<'_, PreviewState>,
    root: PathBuf,
    start: Option<i64>,
    end_exclusive: Option<i64>,
) -> Result<ScanResult, String> {
    let serial = state.next.fetch_add(1, Ordering::SeqCst) + 1;
    let data = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let scoped = tauri::async_runtime::spawn_blocking(move || -> Result<ScopedPreview, String> {
        let scope = anchors::scope_key(&root)?;
        let saved = anchors::load(&data, &scope)?;
        let preview = engine::scan_with_anchors(&root, start, end_exclusive, &saved.commits)?;
        Ok(ScopedPreview { scope, observed: saved, preview: Arc::new(preview) })
    })
        .await
        .map_err(|e| e.to_string())??;
    let preview = Arc::clone(&scoped.preview);
    let id = serial.to_string();
    let mut latest = state.latest.lock().map_err(|_| "预览状态无法读取")?;
    if state.next.load(Ordering::SeqCst) != serial {
        return Err("已开始新的扫描，请使用最新预览。".into());
    }
    *latest = Some((id.clone(), Arc::new(scoped)));
    Ok(ScanResult { id, preview })
}

fn saved_preview(state: &PreviewState, id: &str) -> Result<Arc<ScopedPreview>, String> {
    state.latest.lock().map_err(|_| "预览状态无法读取")?
        .as_ref().filter(|(saved, _)| saved == id)
        .map(|(_, preview)| Arc::clone(preview))
        .ok_or_else(|| "预览已过期，请重新扫描。".into())
}

#[tauri::command]
async fn check_remote(state: State<'_, PreviewState>, id: String, repo_index: usize) -> Result<remote::RemoteStatus, String> {
    let snapshot = saved_preview(&state, &id)?;
    tauri::async_runtime::spawn_blocking(move || {
        let repo = snapshot.preview.repositories.get(repo_index).ok_or("预览中没有此仓库")?;
        Ok(remote::check(repo))
    })
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn file_diff(
    state: State<'_, PreviewState>, id: String, repo_index: usize, change_index: usize,
) -> Result<engine::FileDiff, String> {
    let preview = saved_preview(&state, &id)?;
    tauri::async_runtime::spawn_blocking(move || engine::file_diff(&preview.preview, repo_index, change_index))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn export_zip(app: tauri::AppHandle, state: State<'_, PreviewState>, id: String, dest: PathBuf, confirm_rebuild: bool) -> Result<anchors::ExportResult, String> {
    let preview = saved_preview(&state, &id)?;
    let data = app.path().app_data_dir().map_err(|e| e.to_string())?;
    tauri::async_runtime::spawn_blocking(move || anchors::export(&data, &preview.scope, &preview.observed, &preview.preview, &dest, confirm_rebuild))
        .await
        .map_err(|e| e.to_string())?
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(PreviewState::default())
        .invoke_handler(tauri::generate_handler![git_version, scan_range, check_remote, file_diff, export_zip])
        .run(tauri::generate_context!())
        .expect("桌面应用启动失败");
}
