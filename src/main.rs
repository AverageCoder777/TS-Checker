slint::include_modules!();
use slint::Model;

use rayon::prelude::*;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;
use tempfile::tempdir;
use walkdir::WalkDir;
use zip::ZipArchive;

/// Сколько процессов Serz запускать по умолчанию.
///
/// Конвертация файлов идёт параллельно и упирается в процессор, поэтому
/// полезно задействовать все ядра. Значение всё равно ограничивается
/// числом доступных процессоров.
const DEFAULT_CONCURRENT_SERZ: usize = 8;
const MAX_CONCURRENT_ROUTE_SCANS: usize = 4;

#[derive(Clone, Serialize, Deserialize)]
struct Route {
    id: String,
    name: String,
    path: PathBuf,
    #[serde(default)]
    icon_path: Option<PathBuf>,
}

#[derive(Clone, Serialize, Deserialize)]
struct IncompleteRouteFolder {
    id: String,
    path: PathBuf,
    #[serde(default)]
    icon_path: Option<PathBuf>,
    problems: Vec<String>,
    inventory: Vec<String>,
    scenario_clues: Vec<String>,
    file_count: usize,
    suggested_name: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
struct RouteScanResult {
    routes: Vec<Route>,
    incomplete_folders: Vec<IncompleteRouteFolder>,
}

enum RouteScanItem {
    Route(Route),
    Incomplete(IncompleteRouteFolder),
}

struct CheckResult {
    missing_assets: Vec<MissingAsset>,
    asset_inventory: Vec<ReferencedAsset>,
    umlaut_assets: Vec<UmlautAsset>,
    conversion_errors: Vec<String>,
    structure_issues: Vec<String>,
}

#[derive(Clone)]
struct MissingAsset {
    path: String,
    uses: Vec<AssetUse>,
}

#[derive(Clone)]
struct ReferencedAsset {
    path: String,
    uses: Vec<AssetUse>,
    missing: bool,
}

#[derive(Clone)]
struct UmlautAsset {
    path: String,
    uses: Vec<AssetUse>,
}

#[derive(Clone)]
struct AssetUse {
    source_file: RouteFile,
    tile: String,
    /// Что за объект: объект маршрута, файл состава или что-то другое.
    kind: AssetKind,
}

/// Файл маршрута: обычный файл на диске или запись архива .ap.
///
/// Игра читает архив .ap так, будто его нет: записи лежат рядом с файлами
/// маршрута. Поэтому архив читается прямо из zip, без распаковки целиком.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum RouteFile {
    /// Файл лежит на диске.
    Path(PathBuf),
    /// Запись архива .ap.
    Archive { archive: PathBuf, name: String },
}

impl RouteFile {
    /// Имя файла маршрута относительно его папки.
    pub(crate) fn route_name(&self, route_path: &Path) -> Option<String> {
        match self {
            RouteFile::Path(path) => {
                let relative = path.strip_prefix(route_path).ok()?;
                Some(relative.to_string_lossy().replace('\\', "/"))
            }
            RouteFile::Archive { name, .. } => Some(name.replace('\\', "/")),
        }
    }

    /// Имя файла без папок: по нему определяется тайл.
    pub(crate) fn file_name(&self) -> String {
        match self {
            RouteFile::Path(path) => path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            RouteFile::Archive { name, .. } => Path::new(name)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
        }
    }

    /// Куда класть изменённую версию файла.
    ///
    /// Запись архива кладётся рядом с архивом: игра читает сначала архив, а
    /// потом файлы рядом с ним, поэтому изменённая версия перекроет оригинал, а
    /// сам архив останется нетронутым.
    pub(crate) fn install_target(&self, route_path: &Path) -> PathBuf {
        match self {
            RouteFile::Path(path) => path.clone(),
            RouteFile::Archive { name, .. } => route_path.join(name.replace('\\', "/")),
        }
    }

    /// Готовит файл на диске: запись архива распаковывается во временный файл.
    pub(crate) fn materialize(&self, temp_dir: &Path) -> Result<PathBuf, String> {
        let RouteFile::Archive { archive, name } = self else {
            let RouteFile::Path(path) = self else {
                unreachable!()
            };
            return Ok(path.clone());
        };
        let file = fs::File::open(archive)
            .map_err(|error| format!("Не удалось открыть архив {}: {error}", archive.display()))?;
        let mut zip = zip::ZipArchive::new(file).map_err(|error| {
            format!("Не удалось прочитать архив {}: {error}", archive.display())
        })?;
        let mut member = zip
            .by_name(name)
            .map_err(|error| format!("Не удалось прочитать {name} из архива: {error}"))?;
        let relative = Path::new(name);
        if relative.is_absolute()
            || relative
                .components()
                .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            return Err(format!("Недопустимый путь внутри архива: {name}"));
        }
        let target = temp_dir.join(relative);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("Не удалось создать папку {}: {error}", parent.display()))?;
        }
        let mut output = fs::File::create(&target)
            .map_err(|error| format!("Не удалось создать файл {}: {error}", target.display()))?;
        std::io::copy(&mut member, &mut output)
            .map_err(|error| format!("Не удалось распаковать {name}: {error}"))?;
        Ok(target)
    }

    /// Расширение файла.
    pub(crate) fn extension(&self) -> Option<String> {
        match self {
            RouteFile::Path(path) => path
                .extension()
                .and_then(|extension| extension.to_str())
                .map(|extension| extension.to_owned()),
            RouteFile::Archive { name, .. } => Path::new(name)
                .extension()
                .and_then(|extension| extension.to_str())
                .map(|extension| extension.to_owned()),
        }
    }

    /// Как файл показывать в интерфейсе.
    pub(crate) fn display(&self) -> String {
        match self {
            RouteFile::Path(path) => path.display().to_string(),
            RouteFile::Archive { archive, name } => {
                format!("{}!{}", archive.display(), name)
            }
        }
    }

    /// Признаки файла для кэша разбора.
    pub(crate) fn stamp(&self) -> Option<crate::analysis_cache::FileStamp> {
        match self {
            RouteFile::Path(path) => crate::analysis_cache::FileStamp::of(path),
            // У записи архива признаки берутся у самого архива: когда он меняется,
            // меняются и все его записи.
            RouteFile::Archive { archive, .. } => crate::analysis_cache::FileStamp::of(archive),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AssetKind {
    Loft,
    Scenery,
    /// Файл состава: в XML он лежит внутри `iRailVehicleBlueprint`.
    Vehicle,
    Other,
}

impl AssetKind {
    /// Файл состава — это поезд, а не объект маршрута.
    pub(crate) fn is_vehicle(self) -> bool {
        self == AssetKind::Vehicle
    }

    /// Код типа для кэша разбора.
    pub(crate) fn to_code(self) -> u8 {
        match self {
            AssetKind::Loft => 0,
            AssetKind::Scenery => 1,
            AssetKind::Vehicle => 2,
            AssetKind::Other => 3,
        }
    }

    /// Восстанавливает тип по коду из кэша.
    pub(crate) fn from_code(code: u8) -> Self {
        match code {
            0 => AssetKind::Loft,
            1 => AssetKind::Scenery,
            2 => AssetKind::Vehicle,
            _ => AssetKind::Other,
        }
    }
}

struct AssetReplacementReport {
    replaced_files: usize,
    replaced_instances: usize,
    backups: Vec<PathBuf>,
    replacement_asset: String,
    log_file: Option<PathBuf>,
    log_error: Option<String>,
}

struct UmlautRepairReport {
    changed_files: usize,
    changed_references: usize,
    backups: Vec<PathBuf>,
    files: Vec<PathBuf>,
    new_asset_path: String,
    log_file: Option<PathBuf>,
    log_error: Option<String>,
}

#[derive(Serialize)]
struct AssetReplacementLogEntry {
    timestamp_unix_seconds: u64,
    operation: String,
    route_id: Option<String>,
    route_name: Option<String>,
    route_path: Option<PathBuf>,
    original_files: Vec<String>,
    missing_asset: Option<String>,
    replacement_asset: Option<String>,
    replaced_files: usize,
    replaced_instances: usize,
    uses: Vec<AssetReplacementLogUse>,
    backups: Vec<PathBuf>,
}

#[derive(Serialize)]
struct AssetReplacementLogUse {
    tile: String,
    source_file: String,
}

struct RouteScanOutcome {
    result: RouteScanResult,
    from_cache: bool,
    notice: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct RouteScanCache {
    #[serde(default)]
    icon_scan_version: u8,
    game_path: String,
    folder_ids: Vec<String>,
    ignored_route_ids: Vec<String>,
    result: RouteScanResult,
}

mod analysis_cache;
mod app;
mod assets;
mod blueprint;
mod editor;
mod inventory_ui;
mod reference;
mod routes;
mod scenario;
mod settings;
mod translit;
mod windows;

pub(crate) use assets::*;
pub(crate) use editor::*;
pub(crate) use inventory_ui::*;
pub(crate) use reference::*;
pub(crate) use routes::*;
pub(crate) use scenario::*;
pub(crate) use settings::*;
pub(crate) use translit::*;
pub(crate) use windows::*;

fn parse_asset_launch_args(
    args: impl IntoIterator<Item = std::ffi::OsString>,
) -> Result<Option<PathBuf>, String> {
    let mut args = args.into_iter();
    let Some(path) = args.next() else {
        return Ok(None);
    };
    if args.next().is_some() {
        return Err("Ожидался путь ровно к одному файлу BIN или GeoPcDx.".to_owned());
    }
    let path = PathBuf::from(path);
    if !is_viewable_asset_name(&path.to_string_lossy()) {
        return Err(format!(
            "Поддерживаются только файлы .bin и .GeoPcDx: {}",
            path.display()
        ));
    }
    Ok(Some(path))
}

fn load_route_icon_image(icon_path: &std::path::Path) -> slint::Image {
    match image::open(icon_path) {
        Ok(img) => {
            let rgba = img.resize_exact(32, 32, image::imageops::FilterType::Triangle).to_rgba8();
            let (w, h) = rgba.dimensions();
            let raw = rgba.into_raw();
            let buffer = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(&raw, w, h);
            slint::Image::from_rgba8(buffer)
        }
        Err(_) => slint::Image::default(),
    }
}

fn build_inventory_rows(state: &app::AssetInventoryWindowState, show_missing: bool) -> Vec<slint::ModelRc<slint::StandardListViewItem>> {
    let mut assets: Vec<&ReferencedAsset> = state.assets.iter().collect();
    match state.sort.column {
        inventory_ui::InventorySortColumn::Path => {
            if state.sort.descending {
                assets.sort_by(|a, b| b.path.cmp(&a.path));
            } else {
                assets.sort_by(|a, b| a.path.cmp(&b.path));
            }
        }
        inventory_ui::InventorySortColumn::Usage => {
            if state.sort.descending {
                assets.sort_by(|a, b| b.uses.len().cmp(&a.uses.len()));
            } else {
                assets.sort_by(|a, b| a.uses.len().cmp(&b.uses.len()));
            }
        }
    }
    let search = state.search.to_lowercase();
    let mut rows = Vec::new();
    for a in assets {
        if show_missing && !a.missing {
            continue;
        }
        if !search.is_empty() {
            let matches = a.path.to_lowercase().contains(&search)
                || a.uses.iter().any(|u| {
                    u.tile.to_lowercase().contains(&search)
                        || u.source_file.display().to_lowercase().contains(&search)
                });
            if !matches {
                continue;
            }
        }
        let mut by_tile: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
        for u in a.uses.iter() {
            *by_tile.entry(u.tile.clone()).or_insert(0) += 1;
        }
        let tiles: String = by_tile
            .iter()
            .map(|(tile, count)| format!("{} ({})", tile.trim_start_matches("тайл "), count))
            .collect::<Vec<_>>()
            .join(", ");
        let path_text = a.path.clone();

        let mut item0 = slint::StandardListViewItem::default();
        item0.text = path_text.as_str().into();
        let mut item1 = slint::StandardListViewItem::default();
        item1.text = a.uses.len().to_string().as_str().into();
        let mut item2 = slint::StandardListViewItem::default();
        item2.text = tiles.as_str().into();
        let row_items = vec![item0, item1, item2];
        let row: slint::ModelRc<slint::StandardListViewItem> =
            slint::ModelRc::new(slint::VecModel::from(row_items));
        rows.push(row);
    }
    rows
}

fn main() {
    // Handle file association launch: TS-Checker opened with a .bin/.GeoPcDx file.
    let startup_asset = parse_asset_launch_args(std::env::args_os().skip(1));
    if let Ok(Some(_)) | Err(_) = startup_asset {
        // asset-only mode
        let app = std::sync::Arc::new(std::sync::Mutex::new(match startup_asset {
            Ok(Some(path)) => app::CheckerApp::asset_only(path),
            Ok(None) => app::CheckerApp::default(),
            Err(error) => app::CheckerApp::asset_only_error(error),
        }));
        let editor_win = AssetEditorWindow::new().unwrap();
        let editor_handle = editor_win.as_weak();
        let editor_handle2 = editor_win.as_weak();
        let app_clone = app.clone();
        let app_clone2 = app.clone();
        editor_win.on_save(move || {
            if let Some(ew) = editor_handle.upgrade() {
                let text = ew.get_editor_text();
                if let Ok(mut app) = app_clone.lock() {
                    if let Some(doc) = app.asset_viewer_document.as_mut() {
                        doc.xml = text.to_string();
                    }
                    app.start_asset_save();
                }
            }
        });
        let timer = slint::Timer::default();
        timer.start(slint::TimerMode::Repeated, std::time::Duration::from_millis(50), move || {
            if let Ok(mut app) = app_clone2.lock() {
                app.receive_background_results();
                if app.asset_viewer_open && app.editor_sync_pending {
                    if let Some(doc) = app.asset_viewer_document.as_ref() {
                        let xml = doc.xml.clone().replace('\t', "    ");
                        let status = app.status.clone();
                        app.editor_sync_pending = false;
                        if let Some(ew) = editor_handle2.upgrade() {
                            ew.set_editor_text(xml.into());
                            ew.set_status(status.into());
                            let _ = ew.show();
                        }
                    }
                }
            }
        });
        editor_win.run().unwrap();
        return;
    }
    let app = std::sync::Arc::new(std::sync::Mutex::new(app::CheckerApp::default()));
    let ui = MainWindow::new().unwrap();
    let ui_handle = ui.as_weak();
    if let Ok(mut app) = app.lock() {
        app.start_route_scan();
    }
    let app_clone = app.clone();
    let app_clone2 = app.clone();
    let app_clone3 = app.clone();
    let app_clone4 = app.clone();
    let app_clone5 = app.clone();
    let app_clone6 = app.clone();
    let app_clone8 = app.clone();
    let app_clone9 = app.clone();
    let app_clone10 = app.clone();
    let app_clone11 = app.clone();
    let app_clone12 = app.clone();
    let app_clone13 = app.clone();
    let app_clone14 = app.clone();
    let app_clone15 = app.clone();
    let app_clone16 = app.clone();
    let app_clone17 = app.clone();
    let app_clone18 = app.clone();
    let app_clone19 = app.clone();
    let app_clone20 = app.clone();
    let app_clone21 = app.clone();
    let mut last_count: usize = 0;
    let mut last_query = String::new();
    let app_clone7 = app.clone();
    let missing_win = MissingAssetsWindow::new().unwrap();
    let missing_handle = missing_win.as_weak();
    missing_win.on_replace_missing(move |idx| {
        let (missing, rw_path) = {
            if let Ok(app) = app_clone7.lock() {
                if let Some(result) = app.result.as_ref() {
                    if let Some(missing) = result.missing_assets.get(idx as usize) {
                        (Some(missing.clone()), app.rw_path.clone())
                    } else {
                        (None, String::new())
                    }
                } else {
                    (None, String::new())
                }
            } else {
                (None, String::new())
            }
        };
        if let Some(missing) = missing {
            let assets_path = PathBuf::from(rw_path.trim()).join("Assets");
            let start_dir = existing_directory_upwards(&asset_directory(&assets_path, &missing.path))
                .unwrap_or_else(|| assets_path.clone());
            let selected = rfd::FileDialog::new()
                .set_title("Выберите BIN нового объекта")
                .add_filter("RailWorks BIN", &["bin"])
                .set_directory(&start_dir)
                .pick_file();
            if let Some(replacement) = selected {
                if let Ok(mut app) = app_clone7.lock() {
                    let replacement_relative = replacement.strip_prefix(&assets_path).map(|p| p.to_string_lossy().to_string()).unwrap_or_else(|_| replacement.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default());
                    app.start_asset_replacement(missing, replacement, false, replacement_relative, std::collections::BTreeMap::new());
                }
            }
        }
    });
    let mut missing_window_shown = false;
    let editor_win = AssetEditorWindow::new().unwrap();
    let editor_handle = editor_win.as_weak();
    let editor_handle2 = editor_win.as_weak();
    editor_win.on_save(move || {
        if let Some(ew) = editor_handle.upgrade() {
            let text = ew.get_editor_text();
            if let Ok(mut app) = app_clone20.lock() {
                if let Some(doc) = app.asset_viewer_document.as_mut() {
                    doc.xml = text.to_string();
                }
                app.start_asset_save();
            }
        }
    });
    // Show editor window when asset_viewer_open becomes true
    // (handled in timer below)

    let timer = slint::Timer::default();
    timer.start(slint::TimerMode::Repeated, std::time::Duration::from_millis(50), move || {
        let ui = match ui_handle.upgrade() {
            Some(ui) => ui,
            None => return,
        };
        if let Ok(mut app) = app_clone.lock() {
            app.receive_background_results();
            // Show missing-assets popup after route check finishes
            if !app.is_checking() && app.result.is_some() && app.missing_assets_popup.is_some() && !app.missing_assets_shown {
                let paths: Vec<slint::SharedString> = app
                    .missing_assets_popup
                    .as_ref()
                    .unwrap()
                    .iter()
                    .map(|p| p.as_str().into())
                    .collect();
                if let Some(mw) = missing_handle.upgrade() {
                    mw.set_missing_assets(slint::ModelRc::new(slint::VecModel::from(paths)));
                    let _ = mw.show();
                    app.missing_assets_shown = true;
                }
            }
            // Sync asset editor text when document loaded
            if app.asset_viewer_open && app.editor_sync_pending {
                if let Some(doc) = app.asset_viewer_document.as_ref() {
                    let xml = doc.xml.clone().replace('\t', "    ");
                    let status = app.status.clone();
                    app.editor_sync_pending = false;
                    if let Some(ew) = editor_handle2.upgrade() {
                        ew.set_editor_text(xml.into());
                        ew.set_status(status.into());
                        let _ = ew.show();
                    }
                }
            }
            let q = ui.get_search_query().to_string();
            app.search_query = q.clone();
            if app.routes.len() == last_count && q == last_query {
                ui.set_status(app.status.clone().into());
                ui.set_has_result(app.result.is_some());
                return;
            }
            last_count = app.routes.len();
            last_query = q.clone();
            let query = q.to_lowercase();
            let items: Vec<RouteItem> = app
                .routes
                .iter()
                .filter(|r| query.is_empty() || r.name.to_lowercase().contains(&query))
                .map(|r| {
                    let icon = match r.icon_path.clone() {
                        Some(p) => load_route_icon_image(&p),
                        None => slint::Image::default(),
                    };
                    RouteItem {
                        name: r.name.clone().into(),
                        icon_source: icon,
                    }
                })
                .collect();
            ui.set_route_items(slint::ModelRc::new(slint::VecModel::from(items)));
            let ignored: Vec<slint::SharedString> = app.ignored_route_ids.iter().map(|s| s.clone().into()).collect();
            ui.set_ignored_routes(slint::ModelRc::new(slint::VecModel::from(ignored)));
            let incomplete: Vec<slint::SharedString> = app.incomplete_folders.iter().map(|f| f.id.clone().into()).collect();
            ui.set_incomplete_folders(slint::ModelRc::new(slint::VecModel::from(incomplete)));
            ui.set_status(app.status.clone().into());
            ui.set_has_result(app.result.is_some());
        }
    });
    let ui_handle2 = ui.as_weak();
    ui.on_route_selected(move |idx| {
        if let Some(ui) = ui_handle2.upgrade() {
            ui.set_selected_route(idx);
        }
        if let Ok(mut app) = app_clone2.lock() {
            let q = app.search_query.to_lowercase();
            let filtered: Vec<&Route> = app
                .routes
                .iter()
                .filter(|r| q.is_empty() || r.name.to_lowercase().contains(&q))
                .collect();
            if idx >= 0 && ((idx as usize) < filtered.len()) {
                app.selected_route = Some(app.routes.iter().position(|r| r.name == filtered[idx as usize].name && r.path == filtered[idx as usize].path).unwrap_or(idx as usize));
            } else {
                app.selected_route = None;
            }
            app.refresh_scenarios();
            let scen: Vec<slint::SharedString> = app.scenarios.iter().map(|s| s.name.clone().into()).collect();
            if let Some(ui) = ui_handle2.upgrade() {
                ui.set_scenario_items(slint::ModelRc::new(slint::VecModel::from(scen)));
                ui.set_selected_scenario(-1);
            }
        }
    });
    let ui_handle3 = ui.as_weak();
    ui.on_start_check(move || {
        if let Ok(mut app) = app_clone3.lock() {
            app.start_check();
        }
    });
    let inventory_win = AssetInventoryWindow::new().unwrap();
    let inv_handle = inventory_win.as_weak();
    let inv_handle2 = inventory_win.as_weak();
    let inv_handle3 = inventory_win.as_weak();
    let inv_handle4 = inventory_win.as_weak();
    let inv_handle5 = inventory_win.as_weak();
    let inv_handle6 = inventory_win.as_weak();
    let inv_handle7 = inventory_win.as_weak();
    let inv_handle8 = inventory_win.as_weak();
    let inv_handle9 = inventory_win.as_weak();
    let inv_handle10 = inventory_win.as_weak();
    let settings_win = SettingsWindow::new().unwrap();
    let settings_handle = settings_win.as_weak();
    let settings_handle2 = settings_win.as_weak();
    if let Ok(app) = app_clone11.lock() {
        settings_win.set_serz_process_limit(app.serz_process_limit as f32);
        settings_win.set_locoswap_path(app.locoswap_path.clone().into());
        settings_win.set_route_thumbnails_enabled(app.route_thumbnails_enabled);
    }
    settings_win.on_pick_locoswap(move || {
        if let Some(sw) = settings_handle.upgrade() {
            let current = sw.get_locoswap_path().to_string();
            let mut dialog = rfd::FileDialog::new().set_title("Выберите LocoSwap.exe");
            if !current.is_empty() {
                let p = std::path::PathBuf::from(&current);
                if let Some(parent) = p.parent() { dialog = dialog.set_directory(parent); }
            }
            if let Some(path) = dialog.pick_file() {
                sw.set_locoswap_path(path.to_string_lossy().to_string().into());
            }
        }
    });
    settings_win.on_save_settings(move || {
        if let Some(sw) = settings_handle2.upgrade() {
            let limit = sw.get_serz_process_limit() as usize;
            let path = sw.get_locoswap_path().to_string();
            let thumbs = sw.get_route_thumbnails_enabled();
            if let Ok(mut app) = app_clone11.lock() {
                app.serz_process_limit = limit;
                app.locoswap_path = path.clone();
                app.route_thumbnails_enabled = thumbs;
                let _ = crate::settings::save_serz_process_limit(limit);
                let _ = crate::settings::save_locoswap_path(&path);
                let _ = crate::settings::save_route_thumbnail_setting(thumbs);
            }
        }
    });
    let scenario_win = ScenarioWindow::new().unwrap();
    let scenario_handle = scenario_win.as_weak();
    let scenario_handle2 = scenario_win.as_weak();
    let scenario_handle3 = scenario_win.as_weak();
    ui.on_scenario_selected(move |idx| {
        // load scenario data and show window
        if let Ok(mut app) = app_clone18.lock() {
            // ensure scenarios loaded for selected route
            app.refresh_scenarios();
            let scen_idx = idx as usize;
            if scen_idx >= app.scenarios.len() { return; }
            // lazy load consists
            let needs_load = app.scenarios[scen_idx].consists.is_none();
            if needs_load {
                let scenario_path = app.scenarios[scen_idx].path.clone();
                let serz_path = crate::assets::preferred_serz_path(std::path::Path::new(app.rw_path.trim()));
                let consists = crate::scenario::load_consists(&scenario_path, &serz_path);
                app.scenarios[scen_idx].consists = Some(consists);
            }
            let scenario = app.scenarios[scen_idx].clone();
            let rw_path = app.rw_path.clone();
            let route_path = app.selected_route.and_then(|i| app.routes.get(i)).map(|r| r.path.clone()).unwrap_or_default();
            // build consist names
            let consist_names: Vec<slint::SharedString> = scenario.consists.as_ref().map(|c| c.iter().map(|x| x.name.clone().into()).collect()).unwrap_or_default();
            // check vehicle files for all consists (cache once)
            if app.scenario_consist_info.is_none() || app.scenario_consist_info.as_ref().map(|v| v.len()).unwrap_or(0) != consist_names.len() {
                let mut info = Vec::new();
                let mut vf: Vec<Vec<bool>> = Vec::new();
                if let Some(consists) = scenario.consists.as_ref() {
                    for c in consists {
                        let mut all_ok = true;
                        let mut flags = Vec::new();
                        for v in &c.vehicles {
                            let exists = crate::blueprint::resolve_blueprint_file_cached_free(
                                std::path::Path::new(&rw_path), &route_path, &v.blueprint, &v.provider, &v.product,
                            ).is_some();
                            flags.push(exists);
                            if !exists { all_ok = false; }
                        }
                        info.push((c.name.clone(), all_ok));
                        vf.push(flags);
                    }
                }
                app.scenario_consist_info = Some(info);
                app.scenario_vehicle_files = Some(vf);
            }
            let consist_info = app.scenario_consist_info.clone().unwrap_or_default();
            let vehicle_files = app.scenario_vehicle_files.clone().unwrap_or_default();
            // store for later consist selection: put into a temp via app fields? We'll store in scenario_vehicle_files (already).
            app.selected_scenario = Some(scen_idx);
            app.selected_consist = Some(0);
            app.scenario_window_open = true;
            // populate window
            if let Some(sw) = scenario_handle.upgrade() {
                sw.set_scenario_name(scenario.name.clone().into());
                sw.set_consist_names(slint::ModelRc::new(slint::VecModel::from(consist_names)));
                // vehicles for first consist
                let vehicles: Vec<slint::SharedString> = scenario.consists.as_ref().and_then(|c| c.first()).map(|cc| cc.vehicles.iter().map(|v| format!("{} - {}", v.provider, v.name).into()).collect()).unwrap_or_default();
                let missing: Vec<bool> = vehicle_files.first().cloned().unwrap_or_default();
                let paths: Vec<slint::SharedString> = scenario.consists.as_ref().and_then(|c| c.first()).map(|cc| cc.vehicles.iter().map(|v| v.blueprint.clone().into()).collect()).unwrap_or_default();
                sw.set_vehicle_names(slint::ModelRc::new(slint::VecModel::from(vehicles)));
                sw.set_vehicle_missing(slint::ModelRc::new(slint::VecModel::from(missing)));
                sw.set_vehicle_paths(slint::ModelRc::new(slint::VecModel::from(paths)));
                sw.set_selected_consist(0);
                let _ = sw.show();
            }
        }
    });
    scenario_win.on_consist_selected(move |idx| {
        if let Ok(mut app) = app_clone15.lock() {
            let scen_idx = match app.selected_scenario { Some(i) => i, None => return };
            let scenario = match app.scenarios.get(scen_idx) { Some(s) => s.clone(), None => return };
            let consists = match scenario.consists.as_ref() { Some(c) => c, None => return };
            let i = idx as usize;
            if i >= consists.len() { return; }
            let c = &consists[i];
            let vehicles: Vec<slint::SharedString> = c.vehicles.iter().map(|v| format!("{} - {}", v.provider, v.name).into()).collect();
            let missing: Vec<bool> = app.scenario_vehicle_files.as_ref().and_then(|vf| vf.get(i)).cloned().unwrap_or_default();
            let paths: Vec<slint::SharedString> = c.vehicles.iter().map(|v| v.blueprint.clone().into()).collect();
            app.selected_consist = Some(i);
            if let Some(sw) = scenario_handle2.upgrade() {
                sw.set_vehicle_names(slint::ModelRc::new(slint::VecModel::from(vehicles)));
                sw.set_vehicle_missing(slint::ModelRc::new(slint::VecModel::from(missing)));
                sw.set_vehicle_paths(slint::ModelRc::new(slint::VecModel::from(paths)));
                sw.set_selected_consist(idx);
            }
        }
    });
    scenario_win.on_open_locoswap(move || {
        if let Ok(app) = app_clone16.lock() {
            let path = app.locoswap_path.clone();
            if !path.is_empty() && std::path::Path::new(&path).is_file() {
                let _ = std::process::Command::new(&path).spawn();
            }
        }
    });
    scenario_win.on_open_vehicle_path(move |idx| {
        if let Ok(app) = app_clone17.lock() {
            let scen_idx = match app.selected_scenario { Some(i) => i, None => return };
            let consist_idx = app.selected_consist.unwrap_or(0);
            let scenario = match app.scenarios.get(scen_idx) { Some(s) => s, None => return };
            let consists = match scenario.consists.as_ref() { Some(c) => c, None => return };
            let consist = match consists.get(consist_idx) { Some(c) => c, None => return };
            let vehicle = match consist.vehicles.get(idx as usize) { Some(v) => v, None => return };
            let rw_path = app.rw_path.clone();
            let full = std::path::PathBuf::from(rw_path.trim()).join("Assets").join(vehicle.blueprint.replace('/', "\\"));
            let target = if full.exists() { full.clone() } else {
                let mut cur = full.as_path();
                while !cur.exists() { match cur.parent() { Some(p) => cur = p, None => break } }
                cur.to_path_buf()
            };
            if full.is_file() {
                let _ = std::process::Command::new("explorer.exe").arg(format!("/select,\"{}\"", full.display())).spawn();
            } else {
                let _ = crate::windows::open_in_file_explorer(&target);
            }
        }
    });
    let ui_handle4 = ui.as_weak();
    ui.on_open_route_folder(move || {
        if let Ok(app) = app_clone4.lock() {
            if let Some(idx) = app.selected_route {
                if let Some(route) = app.routes.get(idx) {
                    let _ = open_in_file_explorer(&route.path);
                }
            }
        }
    });
    inventory_win.on_toggle_missing_only(move || {
        if let Some(inv) = inv_handle2.upgrade() {
            let cur = inv.get_show_missing_only();
            inv.set_show_missing_only(!cur);
            let show_missing = !cur;
            if let Ok(app) = app_clone6.lock() {
                if let Ok(state) = app.asset_inventory_state.lock() {
                    let rows = build_inventory_rows(&state, show_missing);
                    inv.set_inventory_rows(slint::ModelRc::new(slint::VecModel::from(rows)));
                }
            }
        }
    });
    inventory_win.on_sort_ascending(move |column| {
        if let Some(inv) = inv_handle3.upgrade() {
            let show_missing = inv.get_show_missing_only();
            if let Ok(mut app) = app_clone8.lock() {
                if let Ok(mut state) = app.asset_inventory_state.lock() {
                    match column {
                        0 => {
                            state.sort.column = inventory_ui::InventorySortColumn::Path;
                            state.sort.descending = false;
                        }
                        1 => {
                            state.sort.column = inventory_ui::InventorySortColumn::Usage;
                            state.sort.descending = false;
                        }
                        _ => {}
                    }
                }
                if let Ok(state) = app.asset_inventory_state.lock() {
                    let rows = build_inventory_rows(&state, show_missing);
                    inv.set_inventory_rows(slint::ModelRc::new(slint::VecModel::from(rows)));
                }
            }
        }
    });
    inventory_win.on_sort_descending(move |column| {
        if let Some(inv) = inv_handle4.upgrade() {
            let show_missing = inv.get_show_missing_only();
            if let Ok(mut app) = app_clone9.lock() {
                if let Ok(mut state) = app.asset_inventory_state.lock() {
                    match column {
                        0 => {
                            state.sort.column = inventory_ui::InventorySortColumn::Path;
                            state.sort.descending = true;
                        }
                        1 => {
                            state.sort.column = inventory_ui::InventorySortColumn::Usage;
                            state.sort.descending = true;
                        }
                        _ => {}
                    }
                }
                if let Ok(state) = app.asset_inventory_state.lock() {
                    let rows = build_inventory_rows(&state, show_missing);
                    inv.set_inventory_rows(slint::ModelRc::new(slint::VecModel::from(rows)));
                }
            }
        }
    });
    inventory_win.on_search_changed(move |text| {
        if let Some(inv) = inv_handle5.upgrade() {
            let show_missing = inv.get_show_missing_only();
            if let Ok(mut app) = app_clone10.lock() {
                if let Ok(mut state) = app.asset_inventory_state.lock() {
                    state.search = text.to_string();
                }
                if let Ok(state) = app.asset_inventory_state.lock() {
                    let rows = build_inventory_rows(&state, show_missing);
                    inv.set_inventory_rows(slint::ModelRc::new(slint::VecModel::from(rows)));
                }
            }
        }
    });
    inventory_win.on_row_clicked(move |row| {
        if let Some(inv) = inv_handle9.upgrade() {
            if row < 0 { return; }
            let rows = inv.get_inventory_rows();
            if (row as usize) >= rows.row_count() { return; }
            let first_cell = rows.row_data(row as usize).and_then(|rm| rm.row_data(0));
            let path_text = first_cell.map(|c| c.text.to_string()).unwrap_or_default();
            // debug log
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(r"C:\Users\kosty\AppData\Local\Temp\kilo\dblclick.log") {
                let _ = writeln!(f, "row={} path_text={:?}", row, path_text);
            }
            if path_text.is_empty() { return; }
            let rw_path = if let Ok(app) = app_clone13.lock() { app.rw_path.clone() } else { String::new() };
            if rw_path.is_empty() { return; }
            let full = std::path::PathBuf::from(rw_path.trim()).join("Assets").join(path_text.replace('/', "\\"));
            // Open nearest existing folder; select file if present.
            let target = if full.exists() {
                // open containing folder with the file selected
                let _ = std::process::Command::new("explorer.exe")
                    .arg(format!("/select,\"{}\"", full.display()))
                    .spawn();
                return;
            } else {
                // walk up to nearest existing parent
                let mut cur = full.as_path();
                while !cur.exists() {
                    match cur.parent() { Some(p) => cur = p, None => break }
                }
                cur.to_path_buf()
            };
            let _ = open_in_file_explorer(&target);
        }
    });
    inventory_win.on_open_in_editor(move || {
        if let Some(inv) = inv_handle10.upgrade() {
            let row = inv.get_current_row();
            if row < 0 { return; }
            let rows = inv.get_inventory_rows();
            if (row as usize) >= rows.row_count() { return; }
            let first_cell = rows.row_data(row as usize).and_then(|rm| rm.row_data(0));
            let path_text = first_cell.map(|c| c.text.to_string()).unwrap_or_default();
            if path_text.is_empty() { return; }
            let rw_path = if let Ok(app) = app_clone14.lock() { app.rw_path.clone() } else { String::new() };
            if rw_path.is_empty() { return; }
            let full = std::path::PathBuf::from(rw_path.trim()).join("Assets").join(path_text.replace('/', "\\"));
            if full.is_file() {
                if let Ok(mut app) = app_clone14.lock() {
                    app.start_asset_view(full);
                }
            }
        }
    });
    inventory_win.on_scan_umlauts(move || {
        if let Ok(app) = app_clone19.lock() {
            let _ = app.inventory_action_tx.send(crate::app::InventoryAction::ScanUmlauts);
        }
    });
    let settings_handle3 = settings_win.as_weak();
    ui.on_open_settings(move || {
        if let Some(sw) = settings_handle3.upgrade() {
            let _ = sw.show();
        }
    });
    ui.on_show_inventory(move || {
        if let Ok(mut app) = app_clone5.lock() {
            app.open_asset_inventory();
            if let Some(inv) = inv_handle.upgrade() {
                if let Ok(state) = app.asset_inventory_state.lock() {
                    inv.set_show_missing_only(false);
                    inv.set_search_query(state.search.clone().as_str().into());
                    let rows = build_inventory_rows(&state, false);
                    inv.set_inventory_rows(slint::ModelRc::new(slint::VecModel::from(rows)));
                }
                inv.show().unwrap();
            }
        }
    });
    ui.run().unwrap();
}

#[cfg(test)]
mod tests {
    use super::{
        check_route, decode_xml_text, extract_asset_references, extract_scenario_clues, fallback_serz_path,
        inspect_route_folder, is_skipped_route_scan_directory, is_valid_game_path,
        parse_asset_launch_args, parse_route_name, preferred_serz_path, scan_routes,
        scan_routes_with_ignored, validate_serz_process, ScenarioClueCollection,
    };
    use crate::app::CheckerApp;
        use image::GenericImageView;
    use std::ffi::OsString;
    use std::fs;
    use std::io::Write;
    #[cfg(windows)]
    use std::path::Path;
    use std::path::PathBuf;

    #[test]
    fn validates_game_path_for_startup_scan() {
        let temp = tempfile::tempdir().unwrap();
        assert!(!is_valid_game_path(temp.path()));

        fs::create_dir_all(temp.path().join("Content/Routes")).unwrap();
        assert!(!is_valid_game_path(temp.path()));

        fs::create_dir_all(temp.path().join("Assets")).unwrap();
        assert!(is_valid_game_path(temp.path()));
    }

    #[test]
    fn parses_asset_launch_arguments_without_starting_for_other_extensions() {
        assert_eq!(parse_asset_launch_args([]).unwrap(), None);
        assert_eq!(
            parse_asset_launch_args([OsString::from("asset.GeoPcDx")]).unwrap(),
            Some(PathBuf::from("asset.GeoPcDx"))
        );
        assert!(parse_asset_launch_args([OsString::from("archive.ap")]).is_err());
        assert!(parse_asset_launch_args([
            OsString::from("asset.bin"),
            OsString::from("another.bin")
        ])
        .is_err());
    }

    #[test]
    fn route_list_stops_growing_after_longest_name_fits() {
        let content_width = 700.0;
        let (narrow_routes, narrow_check) =
            super::app::route_panel_widths(900.0, 8.0, content_width);
        let (wide_routes, wide_check) = super::app::route_panel_widths(1800.0, 8.0, content_width);

        assert!(narrow_routes + 0.001 >= narrow_check * 2.0);
        assert_eq!(wide_routes, content_width);
        assert_eq!(wide_check, 1800.0 - 8.0 - content_width);
        assert!(wide_check > narrow_check);
    }

    #[test]
    fn only_skips_scenarios_when_requested_during_asset_check() {
        assert!(!is_skipped_route_scan_directory("Scenarios", false));
        assert!(is_skipped_route_scan_directory("scenarios", true));
        assert!(!is_skipped_route_scan_directory("Networks", true));
    }

    #[test]
    fn route_check_only_collects_files_from_allowed_directories() {
        let temp = tempfile::tempdir().unwrap();
        let route = temp.path().join("route");
        for directory in [
            "Terrain",
            "Scenery",
            "Scenarios",
            "RouteInfo",
            "Networks",
            "MixMap",
            "ThirdParty",
        ] {
            let folder = route.join(directory);
            fs::create_dir_all(&folder).unwrap();
            fs::write(folder.join("asset.bin"), []).unwrap();
        }
        fs::write(route.join("RouteProperties.xml"), "<route />").unwrap();

        let included = super::assets::collect_route_files(&route, false).unwrap();
        let included_names: Vec<_> = included
            .iter()
            .map(|path| {
                path.strip_prefix(&route)
                    .unwrap()
                    .components()
                    .next()
                    .unwrap()
                    .as_os_str()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        // Шесть папок маршрута и RouteProperties.xml в корне маршрута.
        assert_eq!(included.len(), 7);
        assert!(!included_names.iter().any(|name| name == "ThirdParty"));
        assert!(included.contains(&route.join("RouteProperties.xml")));

        let without_scenarios = super::assets::collect_route_files(&route, true).unwrap();
        assert_eq!(without_scenarios.len(), 6);
        assert_eq!(
            super::assets::unexpected_route_directories(&route).unwrap(),
            ["ThirdParty"]
        );
    }

    #[test]
    fn uses_serz32_as_fallback_for_serz64() {
        let temp = tempfile::tempdir().unwrap();
        let serz64 = temp.path().join("serz64.exe");
        let serz = temp.path().join("serz.exe");
        fs::write(&serz64, []).unwrap();
        assert_eq!(fallback_serz_path(&serz64), None);
        fs::write(&serz, []).unwrap();
        assert_eq!(fallback_serz_path(&serz64), Some(serz));
        assert_eq!(
            fallback_serz_path(temp.path().join("serz.exe").as_path()),
            None
        );
    }

    #[test]
    fn accepts_serz_progress_output_when_process_succeeds() {
        assert!(validate_serz_process(true, "Converting input.bin to output.xml", "").is_ok());
        assert!(validate_serz_process(true, "", "").is_ok());
        assert!(
            validate_serz_process(false, "Could not open input file", "")
                .unwrap_err()
                .contains("Could not open input file")
        );
        assert!(
            validate_serz_process(true, "Converting input.bin", "warning")
                .unwrap_err()
                .contains("warning")
        );
    }

    #[cfg(windows)]
    #[test]
    fn removes_extended_prefix_from_legacy_windows_tool_paths() {
        assert_eq!(
            super::legacy_windows_path(Path::new(r"\\?\X:\RailWorks\file.bin")),
            PathBuf::from(r"X:\RailWorks\file.bin")
        );
        assert_eq!(
            super::legacy_windows_path(Path::new(r"\\?\UNC\server\share\file.bin")),
            PathBuf::from(r"\\server\share\file.bin")
        );
        assert_eq!(
            super::legacy_windows_path(Path::new(r"X:\RailWorks\file.bin")),
            PathBuf::from(r"X:\RailWorks\file.bin")
        );
    }

    #[test]
    fn parses_english_route_name_and_xml_entities() {
        let xml = r#"<DisplayName><Localisation-cUserLocalisedString><English d:type="cDeltaString">London &amp; Brighton</English></Localisation-cUserLocalisedString></DisplayName>"#;
        assert_eq!(parse_route_name(xml).as_deref(), Some("London & Brighton"));
        assert_eq!(decode_xml_text("Fu&#223;ganger"), "Fußganger");
        assert_eq!(decode_xml_text("Fu&#xDF;ganger"), "Fußganger");
    }

    #[test]
    fn falls_back_to_another_localized_name() {
        let xml = r#"<DisplayName><Localisation-cUserLocalisedString><German>Hamburg</German></Localisation-cUserLocalisedString></DisplayName>"#;
        assert_eq!(parse_route_name(xml).as_deref(), Some("Hamburg"));
    }

    #[test]
    fn falls_back_when_english_route_name_is_empty() {
        let xml = r#"<DisplayName><Localisation-cUserLocalisedString><English d:type="cDeltaString"></English><German d:type="cDeltaString">[EZY] Bad Altburger Streckennetz/German</German></Localisation-cUserLocalisedString></DisplayName>"#;
        assert_eq!(
            parse_route_name(xml).as_deref(),
            Some("[EZY] Bad Altburger Streckennetz/German")
        );
    }

    #[test]
    fn extracts_blueprint_references_across_xml_lines() {
        let xml = r#"
            <Provider d:type="cDeltaString">Kuju</Provider>
            <Product d:type="cDeltaString">RailSimulator</Product>
            <BlueprintID d:type="cDeltaString">
                Scenery\Buildings\Station.XML
            </BlueprintID>
        "#;

        assert_eq!(
            extract_asset_references(xml),
            ["Kuju/RailSimulator/Scenery/Buildings/Station.bin".to_owned()]
                .into_iter()
                .collect()
        );
    }

    #[test]
    fn ignores_blueprint_paths_that_escape_the_assets_directory() {
        let xml = r#"
            <Provider>Kuju</Provider>
            <Product>RailSimulator</Product>
            <BlueprintID>..\Outside\asset.xml</BlueprintID>
        "#;

        assert!(extract_asset_references(xml).is_empty());
    }

    #[test]
    fn prefers_serz64_and_falls_back_to_serz() {
        let temp = tempfile::tempdir().unwrap();
        let serz = temp.path().join("serz.exe");
        let serz64 = temp.path().join("serz64.exe");
        fs::write(&serz, []).unwrap();
        assert_eq!(preferred_serz_path(temp.path()), serz);

        fs::write(&serz64, []).unwrap();
        assert_eq!(preferred_serz_path(temp.path()), serz64);
    }

    #[test]
    fn separates_complete_routes_from_incomplete_folder_reports() {
        let temp = tempfile::tempdir().unwrap();
        let routes_root = temp.path().join("Content/Routes");
        let valid_route = routes_root.join("valid-id");
        let incomplete = routes_root.join("leftovers");
        let emptier = routes_root.join("empty-folder");
        fs::create_dir_all(&valid_route).unwrap();
        fs::create_dir_all(incomplete.join("Scenery")).unwrap();
        fs::create_dir_all(&emptier).unwrap();
        fs::write(
            valid_route.join("RouteProperties.xml"),
            "<DisplayName><English>Valid route</English></DisplayName>",
        )
        .unwrap();
        for directory in ["Networks", "RouteInformation", "Scenery", "Terrain"] {
            let path = valid_route.join(directory);
            fs::create_dir_all(&path).unwrap();
            fs::write(path.join("data.bin"), []).unwrap();
        }
        fs::write(incomplete.join("Scenery/orphan.dat"), [1, 2, 3]).unwrap();

        let result = scan_routes(temp.path()).unwrap();

        assert_eq!(result.routes.len(), 1);
        assert_eq!(result.routes[0].name, "Valid route");
        assert_eq!(result.incomplete_folders.len(), 2);
        assert_eq!(result.incomplete_folders[0].id, "empty-folder");
        assert_eq!(result.incomplete_folders[0].file_count, 0);
        assert_eq!(result.incomplete_folders[1].id, "leftovers");
        assert_eq!(result.incomplete_folders[1].file_count, 1);
        assert!(result.incomplete_folders[1]
            .inventory
            .iter()
            .any(|entry| entry.contains("Scenery") && entry.contains("orphan.dat")));
    }

    #[test]
    fn does_not_mark_route_incomplete_when_optional_directory_is_missing() {
        let temp = tempfile::tempdir().unwrap();
        let route_path = temp.path().join("Content/Routes/route");
        fs::create_dir_all(&route_path).unwrap();
        fs::write(
            route_path.join("RouteProperties.xml"),
            "<DisplayName><English>Complete route</English></DisplayName>",
        )
        .unwrap();
        for directory in ["Networks", "RouteInformation", "Scenery", "Terrain"] {
            let directory_path = route_path.join(directory);
            fs::create_dir_all(&directory_path).unwrap();
            fs::write(directory_path.join("data.bin"), []).unwrap();
        }
        let ignored_incomplete_path = temp.path().join("Content/Routes/ignored-incomplete");
        fs::create_dir_all(&ignored_incomplete_path).unwrap();

        let scan = scan_routes(temp.path()).unwrap();
        assert_eq!(scan.incomplete_folders.len(), 1);
        assert_eq!(scan.incomplete_folders[0].id, "ignored-incomplete");
        assert_eq!(scan.routes.len(), 1);
        assert_eq!(scan.routes[0].name, "Complete route");

        let ignored_ids = ["route".to_owned(), "ignored-incomplete".to_owned()]
            .into_iter()
            .collect();
        let ignored_scan = scan_routes_with_ignored(temp.path(), &ignored_ids).unwrap();
        assert!(ignored_scan.incomplete_folders.is_empty());
        assert_eq!(ignored_scan.routes.len(), 2);
    }

    #[test]
    fn extracts_scenario_names_descriptions_and_place_names() {
        let xml = r#"
            <ScenarioProperties>
                <ScenarioName>
                    <Localisation-cUserLocalisedString>
                        <English>Express to Brighton</English>
                    </Localisation-cUserLocalisedString>
                </ScenarioName>
                <ScenarioDescription>Stopping at Lewes and Falmer</ScenarioDescription>
                <StartLocationName>London Victoria</StartLocationName>
                <StartLocation>74151eaf-456f-41e7-8b38-6f48ee8ef031</StartLocation>
            </ScenarioProperties>
        "#;

        let clues = extract_scenario_clues(xml);

        assert!(clues
            .iter()
            .any(|clue| clue == "ScenarioName: Express to Brighton"));
        assert!(clues
            .iter()
            .any(|clue| clue == "ScenarioDescription: Stopping at Lewes and Falmer"));
        assert!(clues
            .iter()
            .any(|clue| clue == "StartLocationName: London Victoria"));
        assert!(!clues
            .iter()
            .any(|clue| clue.contains("74151eaf-456f-41e7-8b38-6f48ee8ef031")));
    }

    #[test]
    fn combines_repeated_start_locations_across_scenarios() {
        let mut clues = ScenarioClueCollection::default();
        clues.add(
            "<ScenarioProperties><StartLocation>King's Cross</StartLocation></ScenarioProperties>",
            "scenario-1/ScenarioProperties.xml",
        );
        clues.add(
            "<ScenarioProperties><StartLocation> King's   Cross </StartLocation></ScenarioProperties>",
            "scenario-2/ScenarioProperties.xml",
        );
        clues.add(
            "<ScenarioProperties><StartLocation>Victoria</StartLocation></ScenarioProperties>",
            "scenario-3/ScenarioProperties.xml",
        );

        let clues = clues.into_clues();
        assert!(clues
            .iter()
            .any(|clue| clue == "StartLocation: King's Cross (упоминается в 2 сценариях)"));
        assert!(clues.iter().any(|clue| clue == "StartLocation: Victoria"));
        assert_eq!(
            clues
                .iter()
                .filter(|clue| clue.starts_with("StartLocation: King's Cross"))
                .count(),
            1
        );
    }

    #[test]
    fn includes_scenario_properties_clues_in_incomplete_route_report() {
        let temp = tempfile::tempdir().unwrap();
        let route_path = temp.path().join("Content/Routes/broken-route");
        let scenario_path = route_path.join("Scenarios/scenario-1");
        fs::create_dir_all(&scenario_path).unwrap();
        fs::write(
            scenario_path.join("ScenarioProperties.xml"),
            "<ScenarioProperties><ScenarioName><English>Coastal run</English></ScenarioName><ScenarioDescription>Through Hastings</ScenarioDescription></ScenarioProperties>",
        )
        .unwrap();

        let report = inspect_route_folder(&route_path, "broken-route")
            .unwrap()
            .unwrap();

        assert!(report
            .scenario_clues
            .iter()
            .any(|clue| clue.contains("ScenarioName: Coastal run")));
        assert!(report
            .scenario_clues
            .iter()
            .any(|clue| clue.contains("ScenarioDescription: Through Hastings")));
    }

    #[test]
    fn reports_empty_required_route_directories() {
        let temp = tempfile::tempdir().unwrap();
        let route = temp.path().join("empty");
        fs::create_dir_all(&route).unwrap();
        for directory in ["Networks", "RouteInformation", "Scenery", "Terrain"] {
            fs::create_dir(route.join(directory)).unwrap();
        }

        let report = inspect_route_folder(&route, "empty").unwrap().unwrap();

        assert_eq!(report.file_count, 0);
        assert!(report
            .problems
            .iter()
            .any(|problem| problem.contains("RouteProperties")));
        for directory in ["Networks", "RouteInformation", "Scenery", "Terrain"] {
            assert!(report
                .problems
                .iter()
                .any(|problem| problem.contains(directory) && problem.contains("пуста")));
        }
    }

    #[test]
    fn treats_only_extreme_compression_ratios_as_suspicious() {
        // Настоящие файлы в .ap сжимаются в десятки раз, а не в тысячи.
        assert!(!super::is_suspicious_archive_member(
            50 * 1024 * 1024,
            2 * 1024 * 1024
        ));
        assert!(!super::is_suspicious_archive_member(1024, 10));
        assert!(super::is_suspicious_archive_member(
            1024 * 1024 * 1024,
            1024 * 1024
        ));
        assert!(super::is_suspicious_archive_member(
            1024 * 1024 * 1024,
            0
        ));
    }

    #[test]
    fn finds_route_icon_in_route_information() {
        let temp = tempfile::tempdir().unwrap();
        let route = temp.path().join("incomplete-route");
        let route_information = route.join("RouteInformation");
        fs::create_dir_all(&route_information).unwrap();
        let dated_image = route_information.join("20220721195917_1.jpg");
        let standard_icon = route_information.join("Image.png");
        fs::write(&dated_image, []).unwrap();
        fs::write(&standard_icon, []).unwrap();

        assert_eq!(super::find_route_icon(&route).unwrap(), Some(standard_icon));
    }

    #[test]
    fn extracts_route_icon_from_ap_archive() {
        let temp = tempfile::tempdir().unwrap();
        let route = temp.path().join("route");
        fs::create_dir_all(&route).unwrap();
        let archive_path = route.join("route-data.ap");
        let archive_file = fs::File::create(&archive_path).unwrap();
        let mut archive = zip::ZipWriter::new(archive_file);
        archive
            .start_file(
                "RouteInformation/Image.png",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
        archive.write_all(b"archived image").unwrap();
        archive.finish().unwrap();

        let icon_path = super::find_route_icon_with_cache(&route, &temp.path().join("icon-cache"))
            .unwrap()
            .unwrap();

        assert_eq!(fs::read(icon_path).unwrap(), b"archived image");
    }

    #[test]
    fn decodes_jpeg_image_with_png_extension() {
        let temp = tempfile::tempdir().unwrap();
        let image_path = temp.path().join("image.png");
        let mut contents = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(2, 1)
            .write_to(&mut contents, image::ImageFormat::Jpeg)
            .unwrap();
        fs::write(&image_path, contents.into_inner()).unwrap();

        let decoded = super::app::decode_route_icon(&image_path).unwrap();

        assert_eq!(decoded.dimensions(), (2, 1));
    }

    #[test]
    fn incomplete_route_report_contains_route_icon_path() {
        let temp = tempfile::tempdir().unwrap();
        let route = temp.path().join("incomplete-route");
        let icon = route.join("RouteInformation/Image.png");
        fs::create_dir_all(icon.parent().unwrap()).unwrap();
        fs::write(&icon, []).unwrap();

        let report = inspect_route_folder(&route, "incomplete-route")
            .unwrap()
            .unwrap();

        assert_eq!(report.icon_path, Some(icon));
    }

    #[test]
    fn complete_route_scan_includes_route_icon_path() {
        let temp = tempfile::tempdir().unwrap();
        let route = temp.path().join("route");
        fs::create_dir_all(route.join("RouteInformation")).unwrap();
        fs::write(
            route.join("RouteProperties.xml"),
            "<DisplayName><Localisation-cUserLocalisedString><English>Test route</English></Localisation-cUserLocalisedString></DisplayName>",
        )
        .unwrap();
        let icon = route.join("RouteInformation/Image.png");
        fs::write(&icon, []).unwrap();
        for directory in ["Networks", "RouteInformation", "Scenery", "Terrain"] {
            let path = route.join(directory);
            fs::create_dir_all(&path).unwrap();
            fs::write(path.join("content.bin"), []).unwrap();
        }

        let scan_item =
            super::scan_route_folder(&route, "route", &std::collections::HashSet::new()).unwrap();

        match scan_item {
            super::RouteScanItem::Route(route) => assert_eq!(route.icon_path, Some(icon)),
            _ => panic!("Ожидался полный маршрут."),
        }
    }

    #[test]
    fn persists_route_thumbnail_setting() {
        let temp = tempfile::tempdir().unwrap();
        let setting_path = temp.path().join("settings/route_thumbnails_enabled.txt");

        assert!(!super::read_route_thumbnail_setting(&setting_path).unwrap());
        super::write_route_thumbnail_setting(&setting_path, true).unwrap();
        assert!(super::read_route_thumbnail_setting(&setting_path).unwrap());
        super::write_route_thumbnail_setting(&setting_path, false).unwrap();
        assert!(!super::read_route_thumbnail_setting(&setting_path).unwrap());
    }

    #[test]
    fn persists_serz_process_limit_with_validation() {
        let temp = tempfile::tempdir().unwrap();
        let setting_path = temp.path().join("settings/serz_process_limit.txt");

        super::write_serz_process_limit(&setting_path, 1).unwrap();
        assert_eq!(fs::read_to_string(&setting_path).unwrap(), "1");
        assert!(super::write_serz_process_limit(&setting_path, 0).is_err());
        assert_eq!(super::read_serz_process_limit(&setting_path).unwrap(), 1);
    }

    #[test]
    fn refuses_to_recycle_a_folder_outside_routes_root() {
        let temp = tempfile::tempdir().unwrap();
        let routes_root = temp.path().join("Content/Routes");
        let outside = temp.path().join("valuable-data");
        fs::create_dir_all(&routes_root).unwrap();
        fs::create_dir_all(&outside).unwrap();

        let error = super::move_folders_to_recycle_bin(
            &routes_root,
            std::slice::from_ref(&outside),
        )
        .unwrap_err();

        assert!(error.contains("вне папки"));
        assert!(outside.is_dir());
    }

    #[test]
    fn invalidates_route_scan_cache_file() {
        let temp = tempfile::tempdir().unwrap();
        let cache_path = temp.path().join("route_cache.json");
        fs::write(&cache_path, b"stale").unwrap();

        super::invalidate_route_scan_cache_at(&cache_path).unwrap();

        assert!(!cache_path.exists());
    }

    #[test]
    fn recognizes_route_structure_and_name_inside_ap_archive() {
        let temp = tempfile::tempdir().unwrap();
        let route_path = temp.path().join("Content/Routes/archive-route");
        fs::create_dir_all(&route_path).unwrap();
        let archive_path = route_path.join("route-data.ap");
        let archive_file = fs::File::create(&archive_path).unwrap();
        let mut archive = zip::ZipWriter::new(archive_file);
        let options = zip::write::SimpleFileOptions::default();
        archive.start_file("RouteProperties.xml", options).unwrap();
        archive
            .write_all(b"<DisplayName><English>Archived Route</English></DisplayName>")
            .unwrap();
        for directory in ["Networks", "RouteInformation", "Scenery", "Terrain"] {
            archive
                .start_file(format!("{directory}/route-data.bin"), options)
                .unwrap();
            archive.write_all(b"data").unwrap();
        }
        archive.finish().unwrap();

        let result = scan_routes(temp.path()).unwrap();

        assert_eq!(result.incomplete_folders.len(), 0);
        assert_eq!(result.routes.len(), 1);
        assert_eq!(result.routes[0].name, "Archived Route");
    }

    #[test]
    fn suggests_ap_filename_when_route_name_files_are_missing() {
        let temp = tempfile::tempdir().unwrap();
        let route_path = temp.path().join("Content/Routes/unknown-route");
        fs::create_dir_all(&route_path).unwrap();
        let archive_file = fs::File::create(route_path.join("Nordbahn.ap")).unwrap();
        zip::ZipWriter::new(archive_file).finish().unwrap();

        for directory in ["Networks", "Scenery", "Terrain"] {
            let directory_path = route_path.join(directory);
            fs::create_dir_all(&directory_path).unwrap();
            fs::write(directory_path.join("content.bin"), b"data").unwrap();
        }

        let result = scan_routes(temp.path()).unwrap();

        assert_eq!(result.incomplete_folders.len(), 1);
        assert_eq!(
            result.incomplete_folders[0].suggested_name.as_deref(),
            Some("Nordbahn")
        );
    }
    #[test]
    fn repro_route_check_hang() {
        let route = std::path::PathBuf::from(r"X:\SteamLibrary\steamapps\common\RailWorks\Content\Routes\8378d7b6-30ad-4c29-a5ac-16d3c2b26ff7");
        let rw = std::path::PathBuf::from(r"X:\SteamLibrary\steamapps\common\RailWorks");
        let assets = rw.join("Assets");
        let mut serz = rw.join("serz64.exe");
        if !serz.is_file() { serz = rw.join("serz.exe"); }
        println!("using serz: {} exists={}", serz.display(), serz.is_file());
        let start = std::time::Instant::now();
        let res = check_route(&route, &assets, &serz, true, 8, |msg| {
            println!("[{:.1}s] {}", start.elapsed().as_secs_f32(), msg);
        });
        println!("RESULT after {:.1}s: {:?}", start.elapsed().as_secs_f32(), res.map(|r| (r.asset_inventory.len(), r.missing_assets.len(), r.conversion_errors.len(), r.structure_issues.len())));
    }
    #[test]
    fn repro_ui_check_flow() {
        let mut app = CheckerApp::default();
        app.rw_path = r"X:\SteamLibrary\steamapps\common\RailWorks".to_owned();
        app.start_route_scan();
        let start = std::time::Instant::now();
        loop {
            app.receive_background_results();
            if !app.routes.is_empty() { break; }
            if start.elapsed().as_secs() > 60 { panic!("route scan timeout"); }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        println!("routes: {}", app.routes.len());
        // select the target route
        if let Some(idx) = app.routes.iter().position(|r| r.id == "8378d7b6-30ad-4c29-a5ac-16d3c2b26ff7") {
            app.selected_route = Some(idx);
        }
        assert!(app.selected_route.is_some(), "no selected route");
        app.start_check();
        let start = std::time::Instant::now();
        loop {
            app.receive_background_results();
            if !app.is_checking() { break; }
            if start.elapsed().as_secs() > 120 { panic!("check timeout after {:?}", start.elapsed()); }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        println!("check done status={}", app.status);
        assert!(app.result.is_some());
    }
}
