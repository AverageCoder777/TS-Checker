#![cfg_attr(windows, windows_subsystem = "windows")]
slint::include_modules!();
use slint::Model;

use rayon::prelude::*;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::tempdir;
use zip::ZipArchive;

const DEFAULT_CONCURRENT_SERZ: usize = 8;

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
    suggested_repair: Option<String>,
    is_corrupted_link: bool,
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
    Archive {
        archive: PathBuf,
        name: String,
        /// Отпечаток архива, вычисленный один раз при сборе файлов.
        stamp: Option<crate::analysis_cache::FileStamp>,
    },
}

thread_local! {
    static ZIP_ARCHIVE_CACHE: RefCell<HashMap<PathBuf, zip::ZipArchive<fs::File>>> =
        RefCell::new(HashMap::new());
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
        let RouteFile::Archive { archive, name, .. } = self else {
            let RouteFile::Path(path) = self else {
                unreachable!()
            };
            return Ok(path.clone());
        };
        let relative = Path::new(name);
        if relative.is_absolute()
            || relative
                .components()
                .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            return Err(format!("Недопустимый путь внутри архива: {name}"));
        }
        let target = temp_dir.join(relative);
        if target.is_file() {
            return Ok(target);
        }
        let data = ZIP_ARCHIVE_CACHE.with(|cache| -> Result<Vec<u8>, String> {
            let mut cache = cache.borrow_mut();
            if !cache.contains_key(archive) {
                let file = fs::File::open(archive).map_err(|error| {
                    format!("Не удалось открыть архив {}: {error}", archive.display())
                })?;
                let zip = zip::ZipArchive::new(file).map_err(|error| {
                    format!("Не удалось прочитать архив {}: {error}", archive.display())
                })?;
                cache.insert(archive.to_path_buf(), zip);
            }
            let zip = cache.get_mut(archive).unwrap();
            let mut member = zip.by_name(name).map_err(|error| {
                format!("Не удалось прочитать {name} из архива: {error}")
            })?;
            let member_size = member.size();
            let mut reader = BufReader::new(&mut member);
            let mut data = Vec::with_capacity(member_size as usize);
            reader
                .read_to_end(&mut data)
                .map_err(|error| format!("Не удалось распаковать {name}: {error}"))?;
            Ok(data)
        })?;
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                format!("Не удалось создать папку {}: {error}", parent.display())
            })?;
        }
        let mut output = BufWriter::new(
            fs::File::create(&target)
                .map_err(|error| format!("Не удалось создать файл {}: {error}", target.display()))?,
        );
        output
            .write_all(&data)
            .map_err(|error| format!("Не удалось распаковать {name}: {error}"))?;
        output
            .flush()
            .map_err(|error| format!("Не удалось записать {name}: {error}"))?;
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
            RouteFile::Archive { archive, name, .. } => {
                format!("{}!{}", archive.display(), name)
            }
        }
    }

    /// Признаки файла для кэша разбора.
    pub(crate) fn stamp(&self) -> Option<crate::analysis_cache::FileStamp> {
        match self {
            RouteFile::Path(path) => crate::analysis_cache::FileStamp::of(path),
            RouteFile::Archive { stamp, .. } => *stamp,
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
    new_asset_path: String,
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
mod auditor;
mod blueprint;
mod external_editor;
mod inventory_ui;
mod reference;
mod routes;
mod scenario;
mod settings;
mod translit;
mod windows;

pub(crate) use assets::*;
pub(crate) use auditor::*;
pub(crate) use inventory_ui::*;
pub(crate) use reference::*;
pub(crate) use routes::*;
pub(crate) use scenario::*;
pub(crate) use settings::*;
pub(crate) use translit::*;
pub(crate) use windows::*;

fn load_route_icon_image(icon_path: &std::path::Path) -> slint::Image {
    if let Ok(data) = std::fs::read(icon_path) {
        if data.starts_with(&[0x89, 0x50, 0x4E, 0x47]) {
            if let Ok(image) = slint::Image::load_from_data(&data, Some("png")) {
                return image;
            }
        } else if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
            if let Ok(image) = slint::Image::load_from_data(&data, Some("jpeg")) {
                return image;
            }
        }
    }
    slint::Image::load_from_path(icon_path).unwrap_or_default()
}

fn load_vehicle_preview_image(
    rw_path: &std::path::Path,
    route_path: &std::path::Path,
    vehicle: &ScenarioVehicle,
    resolved: Option<&std::path::PathBuf>,
) -> slint::Image {
    let resolved = resolved
        .cloned()
        .or_else(|| crate::blueprint::resolve_blueprint_file_cached_free(
            rw_path,
            route_path,
            &vehicle.blueprint,
            &vehicle.provider,
            &vehicle.product,
        ));
    let Some(path) = resolved else {
        return slint::Image::default();
    };

    // Обычный файл на диске: картинка лежит рядом в LocoInformation/image.png.
    if !path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("ap"))
    {
        let image_path = path
            .parent()
            .unwrap_or(path.as_path())
            .join("LocoInformation")
            .join("image.png");
        if image_path.is_file() {
            if let Ok(data) = std::fs::read(&image_path) {
                if let Ok(image) = slint::Image::load_from_data(&data, Some("png")) {
                    return image;
                }
            }
            return slint::Image::load_from_path(&image_path).unwrap_or_default();
        }
        return slint::Image::default();
    }

    // Архив .ap: ищем запись блюпринта, затем соседнюю LocoInformation/image.png.
    let Ok(file) = std::fs::File::open(&path) else {
        return slint::Image::default();
    };
    let Ok(mut zip) = zip::ZipArchive::new(file) else {
        return slint::Image::default();
    };

    let stem = std::path::Path::new(&vehicle.blueprint)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| vehicle.blueprint.clone());
    let mut base_dir = String::new();
    let mut found = false;
    for index in 0..zip.len() {
        let Ok(entry) = zip.by_index(index) else { continue; };
        let name = entry.name().unwrap_or_default().to_string();
        let entry_stem = std::path::Path::new(&name)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        if entry_stem.eq_ignore_ascii_case(&stem) {
            base_dir = name
                .rsplit_once('/')
                .or_else(|| name.rsplit_once('\\'))
                .map(|(dir, _)| dir.to_owned())
                .unwrap_or_default();
            found = true;
            break;
        }
    }
    if !found {
        return slint::Image::default();
    }

    let image_name = if base_dir.is_empty() {
        "LocoInformation/image.png".to_owned()
    } else {
        format!("{}/LocoInformation/image.png", base_dir.trim_end_matches('/'))
    };

    for index in 0..zip.len() {
        let Ok(mut entry) = zip.by_index(index) else { continue; };
        let entry_name = entry.name().unwrap_or_default().to_string();
        if !entry_name.eq_ignore_ascii_case(&image_name) {
            continue;
        }
        let mut data = Vec::new();
        if std::io::Read::read_to_end(&mut entry, &mut data).is_ok() {
            if let Ok(image) = slint::Image::load_from_data(&data, Some("png")) {
                return image;
            }
        }
    }
    slint::Image::default()
}

fn filter_items(items: &mut Vec<BrokenResourceRef>, filter: &str) {
    let f = filter.to_lowercase();
    let f = match f.as_str() {
        "geometry" => "геометрия".to_string(),
        "texture" => "текстура".to_string(),
        "script" => "скрипт".to_string(),
        "sound" | "audio" => "звук".to_string(),
        "child" | "children" => "дочер".to_string(),
        other => other.to_string(),
    };
    if f.is_empty() || f == "all" || f == "все" {
        return;
    }
    items.retain(|b| {
        let kind = match b.kind {
            AssetResourceKind::Geometry => "геометрия",
            AssetResourceKind::Texture => "текстура",
            AssetResourceKind::Animation => "анимация",
            AssetResourceKind::Script => "скрипт",
            AssetResourceKind::Sound => "звуки",
            AssetResourceKind::ChildBlueprint => "дочерние",
        };
        if f.contains("геометр") {
            return kind.contains("геометр");
        }
        if f.contains("текстур") {
            return kind.contains("текстур");
        }
        if f.contains("скрипт") {
            return kind.contains("скрипт");
        }
        if f.contains("звук") || f.contains("аудио") {
            return kind.contains("звук");
        }
        if f.contains("дочер") || f.contains("child") {
            return kind.contains("дочер");
        }
        kind.contains(&f)
    });
}

fn sort_items(items: &mut Vec<BrokenResourceRef>, col: usize, asc: bool) {
    items.sort_by(|a, b| {
        let va = match col {
            0 => match a.kind {
                AssetResourceKind::Geometry => "Геометрия",
                AssetResourceKind::Texture => "Текстура",
                AssetResourceKind::Animation => "Анимация",
                AssetResourceKind::Script => "Скрипт",
                AssetResourceKind::Sound => "Звуки",
                AssetResourceKind::ChildBlueprint => "Дочерние объекты",
            }
            .to_string(),
            1 => a.expected_path.to_string_lossy().to_string(),
            _ => a.parent_file.to_string_lossy().to_string(),
        };
        let vb = match col {
            0 => match b.kind {
                AssetResourceKind::Geometry => "Геометрия",
                AssetResourceKind::Texture => "Текстура",
                AssetResourceKind::Animation => "Анимация",
                AssetResourceKind::Script => "Скрипт",
                AssetResourceKind::Sound => "Звуки",
                AssetResourceKind::ChildBlueprint => "Дочерние объекты",
            }
            .to_string(),
            1 => b.expected_path.to_string_lossy().to_string(),
            _ => b.parent_file.to_string_lossy().to_string(),
        };
        if asc {
            va.cmp(&vb)
        } else {
            vb.cmp(&va)
        }
    });
}

fn build_inventory_rows(
    state: &app::AssetInventoryWindowState,
    show_missing: bool,
) -> (Vec<slint::ModelRc<slint::StandardListViewItem>>, Vec<bool>) {
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
    let mut safe_flags = Vec::new();
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
        let mut by_tile: std::collections::BTreeMap<String, usize> =
            std::collections::BTreeMap::new();
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
        safe_flags.push(crate::assets::is_safe_for_global_replacement(
            &a.path,
            &a.uses,
        ));
    }
    (rows, safe_flags)
}

fn main() {
    // Headless CLI / "Open With" support
    let cli_args: Vec<String> = std::env::args().collect();
    if cli_args.iter().any(|a| a == "--register-shell") {
        if let Ok(exe) = std::env::current_exe() {
            println!("Windows registry entry (save as .reg and import):");
            println!("Windows Registry Editor Version 5.00");
            println!();
            println!("[HKEY_CLASSES_ROOT\\.bin\\shell\\Open with TS-Checker\\command]");
            println!("@=\"\\\"{}\\\" \\\"%1\\\"\"", exe.display());
            println!();
            println!("[HKEY_CLASSES_ROOT\\.GeoPcDx\\shell\\Open with TS-Checker\\command]");
            println!("@=\"\\\"{}\\\" \\\"%1\\\"\"", exe.display());
        }
        std::process::exit(0);
    }
    if let Some(target) = cli_args.get(1) {
        let path = std::path::PathBuf::from(target);
        let is_asset = path.is_file()
            && path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("bin") || e.eq_ignore_ascii_case("geopcdx"))
                .unwrap_or(false);
        if is_asset {
            println!("[ExternalEditor] Запуск синхронизации для: {:?}", path);
            match crate::external_editor::run_headless(&path) {
                Ok(()) => std::process::exit(0),
                Err(e) => {
                    eprintln!("{}", e);
                    std::process::exit(1);
                }
            }
        }
    }

    // Handle file association launch: TS-Checker opened with a .bin/.GeoPcDx file.
    let app = std::sync::Arc::new(std::sync::Mutex::new(app::CheckerApp::default()));
    let ui = MainWindow::new().unwrap();
    let ui_handle = ui.as_weak();
    if let Ok(mut app) = app.lock() {
        app.start_route_scan();
        ui.set_route_thumbnails_enabled(app.route_thumbnails_enabled);
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
    let app_clone13 = app.clone();
    let app_clone14 = app.clone();
    let app_clone15 = app.clone();
    let app_clone16 = app.clone();
    let app_clone17 = app.clone();
    let app_clone18 = app.clone();
    let app_clone19 = app.clone();
    let clipboard = std::sync::Arc::new(std::sync::Mutex::new(arboard::Clipboard::new().unwrap()));
    let mut last_count: usize = 0;
    let mut last_query = String::new();
    let mut last_thumbnails_enabled: bool = false;
    let mut last_status = String::new();
    let mut last_check_log = String::new();
    let app_clone7 = app.clone();
    let missing_win = MissingAssetsWindow::new().unwrap();
    let missing_handle = missing_win.as_weak();
    #[derive(Clone)]
    struct ApPickerState {
        archive_path: PathBuf,
        missing: MissingAsset,
        rw_path: String,
        entries: Vec<String>,
    }
    let ap_picker_win = ApAssetPickerWindow::new().unwrap();
    let ap_picker_handle = ap_picker_win.as_weak();
    let ap_picker_state = std::sync::Arc::new(std::sync::Mutex::new(None::<ApPickerState>));
    missing_win.on_copy_to_clipboard({
        let clipboard = clipboard.clone();
        move |text| {
            if let Ok(mut ctx) = clipboard.lock() {
                if let Err(e) = ctx.set_text(text.to_string()) {
                    eprintln!("Ошибка копирования в буфер: {:?}", e);
                }
            }
        }
    });
    missing_win.on_copy_all_to_clipboard({
        let clipboard = clipboard.clone();
        let missing_handle2 = missing_handle.clone();
        move || {
            if let Some(mw) = missing_handle2.upgrade() {
                let model = mw.get_missing_assets();
                let mut paths = Vec::new();
                for i in 0..model.row_count() {
                    if let Some(value) = model.row_data(i) {
                        paths.push(value.to_string());
                    }
                }
                if let Ok(mut ctx) = clipboard.lock() {
                    let _ = ctx.set_text(paths.join("\n"));
                }
            }
        }
    });
    missing_win.on_close_missing({
        let missing_handle3 = missing_handle.clone();
        move || {
            if let Some(mw) = missing_handle3.upgrade() {
                let _ = mw.hide();
            }
        }
    });
    missing_win.on_repair_corrupted({
        let app = app_clone7.clone();
        move |idx| {
            if let Ok(mut app) = app.lock() {
                let missing_assets = app.result.as_ref().map(|r| r.missing_assets.clone()).unwrap_or_default();
                if let Some(missing) = missing_assets.get(idx as usize).cloned() {
                    if let Some(repair) = missing.suggested_repair.clone() {
                        let assets_path = std::path::PathBuf::from(app.rw_path.trim()).join("Assets");
                        let replacement = assets_path.join(repair.replace('/', "\\"));
                        let replacement_relative = repair.clone();
                        app.start_asset_replacement(
                            missing,
                            replacement,
                            false,
                            replacement_relative,
                            std::collections::BTreeMap::new(),
                        );
                    }
                }
            }
        }
    });
    missing_win.on_repair_all_corrupted({
        let app = app_clone7.clone();
        move || {
            if let Ok(mut app) = app.lock() {
                let missing_assets = app.result.as_ref().map(|r| r.missing_assets.clone()).unwrap_or_default();
                let repairable: Vec<_> = missing_assets
                    .into_iter()
                    .filter(|asset| {
                        asset
                            .suggested_repair
                            .as_ref()
                            .is_some_and(|repair| !repair.trim().is_empty())
                    })
                    .collect();
                let repairable_count = repairable.len();
                eprintln!("[Repair] Found {} repairable assets", repairable_count);
                if repairable_count == 0 {
                    eprintln!("[Repair] No candidates resolved; fuzzy directory matching failed.");
                    return;
                }
                let assets_path = std::path::PathBuf::from(app.rw_path.trim()).join("Assets");
                let queue: Vec<_> = repairable
                    .into_iter()
                    .filter_map(|missing| {
                        let repair = missing.suggested_repair.clone()?;
                        let replacement = assets_path.join(repair.replace('/', "\\"));
                        Some((missing, replacement, repair))
                    })
                    .collect();
                app.asset_replacement_queue = queue;
                app.asset_replacement = None;
                app.missing_assets_shown = false;
                app.start_next_asset_replacement();
            }
        }
    });

    missing_win.on_inspect_tiles({
        let app = app_clone7.clone();
        let handle = missing_handle.clone();
        move |idx| {
            let (asset, tiles) = if let Ok(app) = app.lock() {
                if let Some(result) = app.result.as_ref() {
                    if let Some(missing) = result.missing_assets.get(idx as usize) {
                        let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
                        for usage in &missing.uses {
                            *counts.entry(usage.tile.clone()).or_insert(0) += 1;
                        }
                        let tiles: Vec<slint::SharedString> = counts
                            .into_iter()
                            .map(|(tile, count)| format!("{} ({})", tile, count).into())
                            .collect();
                        (missing.path.clone(), tiles)
                    } else {
                        (String::new(), Vec::new())
                    }
                } else {
                    (String::new(), Vec::new())
                }
            } else {
                (String::new(), Vec::new())
            };
            if let Some(mw) = handle.upgrade() {
                mw.set_active_tile_asset(asset.into());
                mw.set_active_tile_list(slint::ModelRc::new(slint::VecModel::from(tiles)));
            }
        }
    });
    ap_picker_win.on_search_changed({
        let state = ap_picker_state.clone();
        let handle = ap_picker_handle.clone();
        move |query| {
            let entries = state.lock().ok().and_then(|s| s.as_ref().map(|s| s.entries.clone())).unwrap_or_default();
            let q = query.to_lowercase();
            let filtered: Vec<slint::SharedString> = entries
                .iter()
                .filter(|e| q.is_empty() || e.to_lowercase().contains(&q))
                .map(|e| e.clone().into())
                .collect();
            if let Some(pw) = handle.upgrade() {
                pw.set_filtered_entries(slint::ModelRc::new(slint::VecModel::from(filtered)));
            }
        }
    });
    ap_picker_win.on_select_entry({
        let handle = ap_picker_handle.clone();
        move |idx| {
            if let Some(pw) = handle.upgrade() {
                pw.set_selected_index(idx);
            }
        }
    });
    ap_picker_win.on_confirm_selection({
        let state = ap_picker_state.clone();
        let handle = ap_picker_handle.clone();
        let app = app_clone7.clone();
        move || {
            let (archive_path, missing, rw_path, _selected_index, entry) = {
                let guard = match state.lock() { Ok(g) => g, Err(_) => return };
                let Some(s) = guard.as_ref() else { return };
                let pw = match handle.upgrade() { Some(p) => p, None => return };
                let idx = pw.get_selected_index() as usize;
                let filtered = pw.get_filtered_entries();
                let selected_entry = filtered.row_data(idx).map(|v| v.to_string()).or_else(|| s.entries.get(idx).cloned());
                let Some(entry) = selected_entry else { return };
                (s.archive_path.clone(), s.missing.clone(), s.rw_path.clone(), idx, entry)
            };
            // derive provider/product from archive parent dirs
            let assets_path = PathBuf::from(rw_path.trim()).join("Assets");
            let archive_rel = archive_path.strip_prefix(&assets_path).ok();
            let mut parts: Vec<String> = archive_rel.map(|p| p.components().map(|c| c.as_os_str().to_string_lossy().to_string()).collect()).unwrap_or_default();
            // remove archive file name
            if !parts.is_empty() { parts.pop(); }
            let provider = parts.first().cloned().unwrap_or_default();
            let product = parts.get(1).cloned().unwrap_or_default();
            // build a synthetic replacement relative path Provider/Product/...xml? Engine expects .bin file; extract entry to temp file.
            // For minimal integration, call start_asset_replacement with archive path? Instead, create temp file under assets.
            let temp_rel_dir = if provider.is_empty() || product.is_empty() {
                PathBuf::from("TempAp")
            } else {
                PathBuf::from(&provider).join(&product)
            };
            let temp_dir = assets_path.join(&temp_rel_dir);
            let _ = std::fs::create_dir_all(&temp_dir);
            let entry_file_name = std::path::Path::new(&entry).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "asset.bin".to_owned());
            let temp_file = temp_dir.join(&entry_file_name);
            // extract from archive
            if let Ok(file) = std::fs::File::open(&archive_path) {
                if let Ok(mut zip) = zip::ZipArchive::new(file) {
                    if let Ok(mut member) = zip.by_name(&entry) {
                        if let Ok(mut out) = std::fs::File::create(&temp_file) {
                            let _ = std::io::copy(&mut member, &mut out);
                        }
                    }
                }
            }
            let replacement_relative = temp_file.strip_prefix(&assets_path).map(|p| p.to_string_lossy().to_string()).unwrap_or_else(|_| entry.clone());
            if let Ok(mut app) = app.lock() {
                app.start_asset_replacement(
                    missing,
                    temp_file,
                    false,
                    replacement_relative,
                    std::collections::BTreeMap::new(),
                );
            }
            if let Some(pw) = handle.upgrade() {
                let _ = pw.hide();
            }
        }
    });
    ap_picker_win.on_cancel({
        let handle = ap_picker_handle.clone();
        move || {
            if let Some(pw) = handle.upgrade() {
                let _ = pw.hide();
            }
        }
    });

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
            let start_dir =
                existing_directory_upwards(&asset_directory(&assets_path, &missing.path))
                    .unwrap_or_else(|| assets_path.clone());
            let selected = rfd::FileDialog::new()
                .set_title("Выберите BIN/AP нового объекта")
                .add_filter("RailWorks Assets (*.bin, *.ap)", &["bin", "ap"])
                .add_filter("RailWorks Blueprint (*.bin)", &["bin"])
                .add_filter("AP Archive (*.ap)", &["ap"])
                .set_directory(&start_dir)
                .pick_file();
            if let Some(replacement) = selected {
                let ext = replacement
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("")
                    .to_ascii_lowercase();
                if ext == "ap" {
                    let entries = crate::blueprint::ap_archive_entries(&replacement);
                    let archive_name = replacement
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default();
                    if let Ok(mut state) = ap_picker_state.lock() {
                        *state = Some(ApPickerState {
                            archive_path: replacement.clone(),
                            missing: missing.clone(),
                            rw_path: rw_path.clone(),
                            entries: entries.clone(),
                        });
                    }
                    if let Some(pw) = ap_picker_handle.upgrade() {
                        pw.set_archive_name(archive_name.into());
                        let all: Vec<slint::SharedString> =
                            entries.iter().map(|e| e.clone().into()).collect();
                        pw.set_entries(slint::ModelRc::new(slint::VecModel::from(all.clone())));
                        pw.set_filtered_entries(slint::ModelRc::new(slint::VecModel::from(all)));
                        pw.set_search_query("".into());
                        pw.set_selected_index(-1);
                        let _ = pw.show();
                    }
                } else {
                    if let Ok(mut app) = app_clone7.lock() {
                        let replacement_relative = replacement
                            .strip_prefix(&assets_path)
                            .map(|p| p.to_string_lossy().to_string())
                            .unwrap_or_else(|_| {
                                replacement
                                    .file_name()
                                    .map(|n| n.to_string_lossy().to_string())
                                    .unwrap_or_default()
                            });
                        app.start_asset_replacement(
                            missing,
                            replacement,
                            false,
                            replacement_relative,
                            std::collections::BTreeMap::new(),
                        );
                    }
                }
            }
        }
    });
    let timer = slint::Timer::default();
    let ui_handle_timer = ui_handle.clone();
    let inv_handle_timer_slot = std::sync::Arc::new(std::sync::Mutex::new(None::<slint::Weak<AssetInventoryWindow>>));
    let inv_handle_timer_slot_clone = inv_handle_timer_slot.clone();
    timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(50),
        move || {
            let ui = match ui_handle_timer.upgrade() {
                Some(ui) => ui,
                None => return,
            };
            if let Ok(mut app) = app_clone.lock() {
                app.receive_background_results();
                if app.inventory_refresh_pending {
                    let inv_weak = inv_handle_timer_slot_clone.lock().ok().and_then(|g| g.clone());
                    if let Some(inv) = inv_weak.and_then(|w| w.upgrade()) {
                        let show_missing = inv.get_show_missing_only();
                        let (rows, safe) = if let Ok(state) = app.asset_inventory_state.lock() {
                            build_inventory_rows(&state, show_missing)
                        } else {
                            (Vec::new(), Vec::new())
                        };
                        inv.set_inventory_rows(slint::ModelRc::new(slint::VecModel::from(rows)));
                        inv.set_inventory_is_safe(slint::ModelRc::new(slint::VecModel::from(safe)));
                        app.inventory_refresh_pending = false;
                    }
                }
                // Show missing-assets popup after route check finishes
                if !app.is_checking()
                    && app.result.is_some()
                    && app.missing_assets_popup.is_some()
                    && !app.missing_assets_shown
                {
                    let missing_assets = app.result.as_ref().map(|r| r.missing_assets.clone()).unwrap_or_default();
                    let paths: Vec<slint::SharedString> = missing_assets.iter().map(|m| m.path.clone().into()).collect();
                    let counts: Vec<i32> = missing_assets.iter().map(|m| m.uses.len() as i32).collect();
                    let repairs: Vec<slint::SharedString> = missing_assets.iter().map(|m| m.suggested_repair.clone().unwrap_or_default().into()).collect();
                    let corrupted: Vec<bool> = missing_assets.iter().map(|m| m.is_corrupted_link).collect();
                    if let Some(mw) = missing_handle.upgrade() {
                        mw.set_missing_assets(slint::ModelRc::new(slint::VecModel::from(paths)));
                        mw.set_missing_counts(slint::ModelRc::new(slint::VecModel::from(counts)));
                        mw.set_missing_suggested_repairs(slint::ModelRc::new(slint::VecModel::from(repairs)));
                        mw.set_missing_corrupted(slint::ModelRc::new(slint::VecModel::from(corrupted)));
                        mw.set_active_tile_list(slint::ModelRc::new(slint::VecModel::<slint::SharedString>::default()));
                        mw.set_active_tile_asset("".into());
                        let _ = mw.show();
                        app.missing_assets_shown = true;
                    }
                }
                // Sync asset editor text when document loaded
                let q = ui.get_search_query().to_string();
                app.search_query = q.clone();
                if app.routes.len() == last_count && q == last_query && app.route_thumbnails_enabled == last_thumbnails_enabled {
                    if app.status != last_status {
                        last_status = app.status.clone();
                        ui.set_status(last_status.clone().into());
                    }
                    if app.check_log != last_check_log {
                        last_check_log = app.check_log.clone();
                        ui.set_check_log(last_check_log.clone().into());
                    }
                ui.set_has_result(app.result.is_some());
                ui.set_missing_assets_count(app.result.as_ref().map(|r| r.missing_assets.len() as i32).unwrap_or(0));
                ui.set_is_checking(app.is_checking());
                if app.is_checking() {
                    ui.set_show_log(true);
                }
                ui.set_route_thumbnails_enabled(app.route_thumbnails_enabled);
                ui.set_is_checking(app.is_checking());
                if app.is_checking() {
                    ui.set_show_log(true);
                }
                    return;
                }
                last_count = app.routes.len();
                last_query = q.clone();
                last_thumbnails_enabled = app.route_thumbnails_enabled;
                let query = q.to_lowercase();
                let items: Vec<RouteItem> = app
                    .routes
                    .iter()
                    .filter(|r| query.is_empty() || r.name.to_lowercase().contains(&query))
                    .map(|r| {
                        let icon = if app.route_thumbnails_enabled {
                            match r.icon_path.clone() {
                                Some(p) => load_route_icon_image(&p),
                                None => slint::Image::default(),
                            }
                        } else {
                            slint::Image::default()
                        };
                        RouteItem {
                            name: r.name.clone().into(),
                            icon_source: icon,
                        }
                    })
                    .collect();
                ui.set_route_items(slint::ModelRc::new(slint::VecModel::from(items)));
                let ignored: Vec<slint::SharedString> = app
                    .ignored_route_ids
                    .iter()
                    .map(|s| s.clone().into())
                    .collect();
                ui.set_ignored_routes(slint::ModelRc::new(slint::VecModel::from(ignored)));
                let incomplete: Vec<slint::SharedString> = app
                    .incomplete_folders
                    .iter()
                    .map(|f| f.id.clone().into())
                    .collect();
                ui.set_incomplete_folders(slint::ModelRc::new(slint::VecModel::from(incomplete)));
                if let Some(route) = app.selected_route.and_then(|i| app.routes.get(i)) {
                    ui.set_selected_route_name(route.name.clone().into());
                    ui.set_selected_route_id(route.id.clone().into());
                } else {
                    ui.set_selected_route_name("".into());
                    ui.set_selected_route_id("".into());
                }
                if app.status != last_status {
                    last_status = app.status.clone();
                    ui.set_status(last_status.clone().into());
                }
                if app.check_log != last_check_log {
                    last_check_log = app.check_log.clone();
                    ui.set_check_log(last_check_log.clone().into());
                }
                ui.set_has_result(app.result.is_some());
                ui.set_missing_assets_count(app.result.as_ref().map(|r| r.missing_assets.len() as i32).unwrap_or(0));
            }
        },
    );
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
                app.selected_route = Some(
                    app.routes
                        .iter()
                        .position(|r| {
                            r.name == filtered[idx as usize].name
                                && r.path == filtered[idx as usize].path
                        })
                        .unwrap_or(idx as usize),
                );
            } else {
                app.selected_route = None;
            }
            app.refresh_scenarios();
            let hide = app.hide_quick_drive_free_roam;
            let scen: Vec<ScenarioItem> = app
                .scenarios
                .iter()
                .filter(|s| !hide || !crate::scenario::is_hidden_scenario_type(&s.scenario_type))
                .map(|s| ScenarioItem {
                    name: s.name.clone().into(),
                    scenario_type: s.scenario_type.clone().into(),
                })
                .collect();
            if let Some(ui) = ui_handle2.upgrade() {
                ui.set_scenario_items(slint::ModelRc::new(slint::VecModel::from(scen)));
                ui.set_selected_scenario(-1);
                if let Some(route) = app.selected_route.and_then(|i| app.routes.get(i)) {
                    ui.set_selected_route_name(route.name.clone().into());
                    ui.set_selected_route_id(route.id.clone().into());
                } else {
                    ui.set_selected_route_name("".into());
                    ui.set_selected_route_id("".into());
                }
            }
        }
    });
    let ui_handle_start_check = ui_handle.clone();
    ui.on_start_check(move || {
        if let Some(ui) = ui_handle_start_check.upgrade() {
            ui.set_show_log(true);
        }
        if let Ok(mut app) = app_clone3.lock() {
            app.start_check();
        }
    });
    let app_clone_reset = app.clone();
    let ui_handle_reset = ui_handle.clone();
    ui.on_reset_log(move || {
        if let Some(ui) = ui_handle_reset.upgrade() {
            if let Ok(app) = app_clone_reset.lock() {
                ui.set_check_log(app.check_log.clone().into());
            }
        }
    });
    let inventory_win = AssetInventoryWindow::new().unwrap();
    let inv_handle = inventory_win.as_weak();
    let inv_handle2 = inventory_win.as_weak();
    let inv_handle3 = inventory_win.as_weak();
    let inv_handle4 = inventory_win.as_weak();
    let inv_handle5 = inventory_win.as_weak();
    let inv_handle9 = inventory_win.as_weak();
    let inv_handle10 = inventory_win.as_weak();
    let inv_handle11 = inventory_win.as_weak();
    if let Ok(mut slot) = inv_handle_timer_slot.lock() {
        *slot = Some(inventory_win.as_weak());
    }
    let settings_win = SettingsWindow::new().unwrap();
    let settings_handle = settings_win.as_weak();
    let settings_handle2 = settings_win.as_weak();
    let settings_handle3 = settings_win.as_weak();
    let settings_handle4 = settings_win.as_weak();
    if let Ok(app) = app_clone11.lock() {
        let available = settings::maximum_serz_processes();
        let all_presets: [usize; 6] = [1, 6, 8, 12, 16, 32];
        let presets: Vec<slint::SharedString> = all_presets
            .iter()
            .filter(|p| **p <= available)
            .map(|p| p.to_string().into())
            .collect();
        let mut current = app.serz_process_limit.clamp(1, available);
        if !presets.iter().any(|p| *p == current.to_string()) {
            current = presets
                .iter()
                .filter(|p| p.parse::<usize>().map(|v| v <= current).unwrap_or(false))
                .map(|p| p.parse::<usize>().unwrap_or(1))
                .max()
                .unwrap_or(1);
        }
        let index = presets.iter().position(|p| *p == current.to_string()).unwrap_or(0);
        settings_win.set_serz_process_limit(current as i32);
        settings_win.set_serz_process_limit_max(available as i32);
        settings_win.set_serz_presets(slint::ModelRc::new(slint::VecModel::from(presets)));
        settings_win.set_serz_process_limit_index(index as i32);
        settings_win.set_locoswap_path(app.locoswap_path.clone().into());
        settings_win.set_route_thumbnails_enabled(app.route_thumbnails_enabled);
        settings_win.set_route_scan_on_hdd(app.route_scan_on_hdd);
        settings_win.set_hide_quick_drive_free_roam(app.hide_quick_drive_free_roam);
        settings_win.set_game_path(app.rw_path.clone().into());
    }
    settings_win.on_pick_locoswap(move || {
        if let Some(sw) = settings_handle.upgrade() {
            let current = sw.get_locoswap_path().to_string();
            let mut dialog = rfd::FileDialog::new().set_title("Выберите LocoSwap.exe");
            if !current.is_empty() {
                let p = std::path::PathBuf::from(&current);
                if let Some(parent) = p.parent() {
                    dialog = dialog.set_directory(parent);
                }
            }
            if let Some(path) = dialog.pick_file() {
                sw.set_locoswap_path(path.to_string_lossy().to_string().into());
            }
        }
    });
    settings_win.on_pick_game(move || {
        if let Some(sw) = settings_handle3.upgrade() {
            let current = sw.get_game_path().to_string();
            let mut dialog = rfd::FileDialog::new().set_title("Выберите папку с игрой RailWorks");
            if !current.is_empty() {
                let p = std::path::PathBuf::from(&current);
                if p.is_dir() {
                    dialog = dialog.set_directory(&p);
                } else if let Some(parent) = p.parent() {
                    dialog = dialog.set_directory(parent);
                }
            }
            if let Some(path) = dialog.pick_folder() {
                sw.set_game_path(path.to_string_lossy().to_string().into());
            }
        }
    });
    let app_clone12 = app_clone11.clone();
    settings_win.on_serz_preset_selected(move |i| {
        if let Some(sw) = settings_handle4.upgrade() {
            let presets = sw.get_serz_presets();
            if let Some(item) = presets.row_data(i as usize) {
                if let Ok(limit) = item.to_string().parse::<usize>() {
                    let _ = sw.set_serz_process_limit(limit as i32);
                    if let Ok(mut app) = app_clone12.lock() {
                        app.serz_process_limit = limit;
                    }
                }
            }
        }
    });
    settings_win.on_save_settings(move || {
        if let Some(sw) = settings_handle2.upgrade() {
            let limit = sw.get_serz_process_limit() as usize;
            let path = sw.get_locoswap_path().to_string();
            let game_path = sw.get_game_path().to_string();
            let thumbs = sw.get_route_thumbnails_enabled();
            let hdd = sw.get_route_scan_on_hdd();
            let hide_scenarios = sw.get_hide_quick_drive_free_roam();
            if let Ok(mut app) = app_clone11.lock() {
                app.serz_process_limit = limit;
                app.locoswap_path = path.clone();
                app.rw_path = game_path.clone();
                app.route_thumbnails_enabled = thumbs;
                app.route_scan_on_hdd = hdd;
                app.hide_quick_drive_free_roam = hide_scenarios;
                let _ = crate::settings::save_serz_process_limit(limit);
                let _ = crate::settings::save_locoswap_path(&path);
                let _ = crate::settings::save_last_game_path(&game_path);
                let _ = crate::settings::save_route_thumbnail_setting(thumbs);
                let _ = crate::settings::save_route_scan_on_hdd(hdd);
                let _ = crate::settings::save_hide_quick_drive_free_roam(hide_scenarios);
            }
        }
    });
    let scenario_win = ScenarioWindow::new().unwrap();
    let scenario_handle = scenario_win.as_weak();
    let scenario_handle2 = scenario_win.as_weak();
    let ui_handle3 = ui.as_weak();
    ui.on_scenario_selected(move |idx| {
        let app = app_clone18.clone();
        let ui_handle = ui_handle3.clone();
        let scenario_handle = scenario_handle.clone();
        std::thread::spawn(move || {
            let data = (|| -> Option<(Vec<ScenarioItem>, i32, String, String, String, String, String, String, Vec<slint::SharedString>, Vec<bool>, Vec<bool>, Vec<bool>, Vec<slint::SharedString>, Vec<bool>, Vec<slint::SharedString>, String)> {
                let mut app = app.lock().ok()?;
                app.refresh_scenarios();
                let hide = app.hide_quick_drive_free_roam;
                let filtered_indices: Vec<usize> = app
                    .scenarios
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| !hide || !crate::scenario::is_hidden_scenario_type(&s.scenario_type))
                    .map(|(i, _)| i)
                    .collect();
                let ui_idx = idx as usize;
                let &scen_idx = filtered_indices.get(ui_idx)?;
                if scen_idx >= app.scenarios.len() {
                    return None;
                }
                crate::scenario::load_scenario_metadata(&mut app.scenarios[scen_idx]);

                let scen_items: Vec<ScenarioItem> = app
                    .scenarios
                    .iter()
                    .filter(|s| !hide || !crate::scenario::is_hidden_scenario_type(&s.scenario_type))
                    .map(|s| ScenarioItem {
                        name: s.name.clone().into(),
                        scenario_type: s.scenario_type.clone().into(),
                    })
                    .collect();

                let needs_load = app.scenarios[scen_idx].consists.is_none();
                if needs_load {
                    let scenario_path = app.scenarios[scen_idx].path.clone();
                    let serz_path =
                        crate::assets::preferred_serz_path(std::path::Path::new(app.rw_path.trim()));
                    let consists = crate::scenario::load_consists(&scenario_path, &serz_path);
                    app.scenarios[scen_idx].consists = Some(consists);
                }

                let scenario = app.scenarios[scen_idx].clone();
                let rw_path = app.rw_path.clone();
                let route_path = app
                    .selected_route
                    .and_then(|i| app.routes.get(i))
                    .map(|r| r.path.clone())
                    .unwrap_or_default();

                let consist_names: Vec<slint::SharedString> = scenario
                    .consists
                    .as_ref()
                    .map(|c| c.iter().map(|x| x.name.clone().into()).collect())
                    .unwrap_or_default();
                let consist_player: Vec<bool> = scenario
                    .consists
                    .as_ref()
                    .map(|c| c.iter().map(|x| x.is_player).collect())
                    .unwrap_or_default();

                if app.scenario_consist_info.is_none()
                    || app
                        .scenario_consist_info
                        .as_ref()
                        .map(|v| v.len())
                        .unwrap_or(0)
                        != consist_names.len()
                {
                    let mut info = Vec::new();
                    let mut vf = Vec::new();
                    if let Some(consists) = scenario.consists.as_ref() {
                        let (i, v): (Vec<(String, bool)>, Vec<Vec<bool>>) = consists
                            .par_iter()
                            .map(|c| {
                                let mut all_ok = true;
                                let mut flags = Vec::with_capacity(c.vehicles.len());
                                 for veh in &c.vehicles {
                                     let exists = crate::blueprint::vehicle_blueprint_exists(
                                         std::path::Path::new(&rw_path),
                                         &veh.provider,
                                         &veh.product,
                                         &veh.blueprint,
                                     );
                                     flags.push(!exists);
                                    if !exists {
                                        all_ok = false;
                                    }
                                }
                                ((c.name.clone(), all_ok), flags)
                            })
                            .unzip();
                        info = i;
                        vf = v;
                    }
                    app.scenario_consist_info = Some(info);
                    app.scenario_vehicle_files = Some(vf);
                }

                let vehicle_files = app.scenario_vehicle_files.clone().unwrap_or_default();
                app.selected_scenario = Some(scen_idx);
                app.selected_consist = Some(0);
                app.scenario_window_open = true;

                let consist_missing: Vec<bool> = app
                    .scenario_consist_info
                    .as_ref()
                    .map(|info| info.iter().map(|(_, ok)| !*ok).collect())
                    .unwrap_or_default();
                let consist_complete: Vec<bool> = app
                    .scenario_consist_info
                    .as_ref()
                    .map(|info| info.iter().map(|(_, ok)| *ok).collect())
                    .unwrap_or_default();

                let vehicles: Vec<slint::SharedString> = scenario
                    .consists
                    .as_ref()
                    .and_then(|c| c.first())
                    .map(|cc| {
                        cc.vehicles
                            .iter()
                            .map(|v| format!("{}", v.name).into())
                            .collect()
                    })
                    .unwrap_or_default();
                let missing: Vec<bool> = vehicle_files.first().cloned().unwrap_or_default();
                let paths: Vec<slint::SharedString> = scenario
                    .consists
                    .as_ref()
                    .and_then(|c| c.first())
                    .map(|cc| {
                        cc.vehicles
                            .iter()
                            .map(|v| v.blueprint.clone().into())
                            .collect()
                    })
                    .unwrap_or_default();

                let selected_vehicle_info = if let Some(v) = scenario
                    .consists
                    .as_ref()
                    .and_then(|c| c.first())
                    .and_then(|cc| cc.vehicles.first())
                {
                    format!(
                        "Поставщик: {}\nДополнение: {}\nФайл: {}\nНомер: {}",
                        v.provider, v.product, v.blueprint,
                        v.number.clone().unwrap_or_else(|| "—".to_owned())
                    )
                } else {
                    "Выберите единицу техники".to_owned()
                };

                let weather_name = crate::scenario::weather_display_name(
                    std::path::Path::new(rw_path.trim()),
                    std::path::Path::new(&route_path),
                    &crate::assets::preferred_serz_path(std::path::Path::new(rw_path.trim())),
                    &scenario.weather,
                );

                let scenario_name = scenario.name.clone();
                let scenario_title = format!(
                    "Сценарий: {}",
                    if scenario.name.trim().is_empty() {
                        "Без названия"
                    } else {
                        scenario.name.as_str()
                    }
                );
                let fallback = |value: &str| -> String {
                    if value.trim().is_empty() {
                        "Не указано".to_owned()
                    } else {
                        value.to_owned()
                    }
                };
                let scenario_author = fallback(&scenario.author);
                let scenario_season = fallback(&scenario.season);
                let scenario_start_time = fallback(&scenario.start_time);
                let scenario_weather = fallback(&weather_name);

                Some((
                    scen_items,
                    ui_idx as i32,
                    scenario_name,
                    scenario_title,
                    scenario_author,
                    scenario_season,
                    scenario_start_time,
                    scenario_weather,
                    consist_names,
                    consist_player,
                    consist_missing,
                    consist_complete,
                    vehicles,
                    missing,
                    paths,
                    selected_vehicle_info,
                ))
            })();

            if let Some((
                scen_items,
                ui_selected_scenario,
                scenario_name,
                scenario_title,
                scenario_author,
                scenario_season,
                scenario_start_time,
                scenario_weather,
                consist_names,
                consist_player,
                consist_missing,
                consist_complete,
                vehicles,
                missing,
                paths,
                selected_vehicle_info,
            )) = data
            {
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = ui_handle.upgrade() {
                        ui.set_scenario_items(slint::ModelRc::new(slint::VecModel::from(scen_items)));
                        ui.set_selected_scenario(ui_selected_scenario);
                    }
                    if let Some(sw) = scenario_handle.upgrade() {
                        sw.set_scenario_name(scenario_name.into());
                        sw.set_scenario_title(scenario_title.into());
                        sw.set_scenario_author(scenario_author.into());
                        sw.set_scenario_season(scenario_season.into());
                        sw.set_scenario_start_time(scenario_start_time.into());
                        sw.set_scenario_weather(scenario_weather.into());
                        sw.set_consist_names(slint::ModelRc::new(slint::VecModel::from(consist_names)));
                        sw.set_consist_player(slint::ModelRc::new(slint::VecModel::from(consist_player)));
                        sw.set_consist_missing(slint::ModelRc::new(slint::VecModel::from(consist_missing)));
                        sw.set_consist_complete(slint::ModelRc::new(slint::VecModel::from(consist_complete)));
                        sw.set_vehicle_names(slint::ModelRc::new(slint::VecModel::from(vehicles)));
                        sw.set_vehicle_missing(slint::ModelRc::new(slint::VecModel::from(missing)));
                        sw.set_vehicle_paths(slint::ModelRc::new(slint::VecModel::from(paths)));
                        sw.set_selected_vehicle_info(selected_vehicle_info.into());
                        sw.set_selected_consist(0);
                        let _ = sw.show();
                    }
                });
            }
        });
    });

    scenario_win.on_consist_selected(move |idx| {
        if let Ok(mut app) = app_clone15.lock() {
            let scen_idx = match app.selected_scenario {
                Some(i) => i,
                None => return,
            };
            let scenario = match app.scenarios.get(scen_idx) {
                Some(s) => s.clone(),
                None => return,
            };
            let consists = match scenario.consists.as_ref() {
                Some(c) => c,
                None => return,
            };
            let i = idx as usize;
            if i >= consists.len() {
                return;
            }
            let c = &consists[i];
            let vehicles: Vec<slint::SharedString> = c
                .vehicles
                .iter()
                .map(|v| format!("{} - {}", v.provider, v.name).into())
                .collect();
            let missing: Vec<bool> = app
                .scenario_vehicle_files
                .as_ref()
                .and_then(|vf| vf.get(i))
                .cloned()
                .unwrap_or_default();
            let paths: Vec<slint::SharedString> = c
                .vehicles
                .iter()
                .map(|v| v.blueprint.clone().into())
                .collect();
            app.selected_consist = Some(i);
            if let Some(sw) = scenario_handle2.upgrade() {
                sw.set_vehicle_names(slint::ModelRc::new(slint::VecModel::from(vehicles)));
                sw.set_vehicle_missing(slint::ModelRc::new(slint::VecModel::from(missing)));
                sw.set_vehicle_paths(slint::ModelRc::new(slint::VecModel::from(paths)));
                sw.set_selected_consist(idx);
            }
        }
    });
    let app_clone_locoswap_consist = app.clone();
    let app_clone_open_scenario_folder = app.clone();
    let app_clone_close_scenario = app.clone();
    let ui_handle_close_scenario = ui.as_weak();
    scenario_win.on_open_scenario_folder(move || {
        if let Ok(app) = app_clone_open_scenario_folder.lock() {
            if let Some(idx) = app.selected_scenario {
                if let Some(scenario) = app.scenarios.get(idx) {
                    let _ = open_in_file_explorer(&scenario.path);
                }
            }
        }
    });
    let scenario_handle_close = scenario_win.as_weak();
    scenario_win.on_close_scenario(move || {
        if let Some(sw) = scenario_handle_close.upgrade() {
            let _ = sw.hide();
        }
        let ui_handle = ui_handle_close_scenario.clone();
        if let Ok(mut app) = app_clone_close_scenario.lock() {
            app.scenario_window_open = false;
            let hide = app.hide_quick_drive_free_roam;
            let scen: Vec<ScenarioItem> = app
                .scenarios
                .iter()
                .filter(|s| !hide || !crate::scenario::is_hidden_scenario_type(&s.scenario_type))
                .map(|s| ScenarioItem {
                    name: s.name.clone().into(),
                    scenario_type: s.scenario_type.clone().into(),
                })
                .collect();
            let selected_ui = app.selected_scenario.and_then(|si| {
                app.scenarios
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| !hide || !crate::scenario::is_hidden_scenario_type(&s.scenario_type))
                    .position(|(i, _)| i == si)
            });
            if let Some(ui) = ui_handle.upgrade() {
                ui.set_scenario_items(slint::ModelRc::new(slint::VecModel::from(scen)));
                ui.set_selected_scenario(selected_ui.map(|i| i as i32).unwrap_or(-1));
            }
        }
    });
    scenario_win.on_open_locoswap(move || {
        if let Ok(app) = app_clone16.lock() {
            let path = app.locoswap_path.clone();
            if !path.is_empty() && std::path::Path::new(&path).is_file() {
                let mut cmd = std::process::Command::new(&path);
                if let Some(parent) = std::path::Path::new(&path).parent() {
                    cmd.current_dir(parent);
                }
                if let Some(idx) = app.selected_scenario {
                    if let Some(scenario) = app.scenarios.get(idx) {
                        cmd.arg(&scenario.path);
                    }
                }
                let _ = cmd.spawn();
            }
        }
    });
    scenario_win.on_open_locoswap_consist(move |idx| {
        if let Ok(app) = app_clone_locoswap_consist.lock() {
            let locoswap_path = app.locoswap_path.clone();
            if locoswap_path.is_empty() || !std::path::Path::new(&locoswap_path).is_file() {
                return;
            }
            let scen_idx = match app.selected_scenario {
                Some(i) => i,
                None => return,
            };
            let scenario = match app.scenarios.get(scen_idx) {
                Some(s) => s.clone(),
                None => return,
            };
            let consist_index = scenario
                .consists
                .as_ref()
                .and_then(|consists| consists.get(idx as usize))
                .map(|consist| consist.index)
                .unwrap_or(idx as usize);
            let mut cmd = std::process::Command::new(&locoswap_path);
            if let Some(parent) = std::path::Path::new(&locoswap_path).parent() {
                cmd.current_dir(parent);
            }
            let _ = cmd
                .arg(&scenario.path)
                .arg("--consist")
                .arg(consist_index.to_string())
                .arg("--vehicle")
                .arg("-1")
                .spawn();
        }
    });
    let app_clone_vehicle_preview = app.clone();
    let scenario_handle_preview = scenario_win.as_weak();
    scenario_win.on_vehicle_selected(move |idx| {
        if let Ok(mut app) = app_clone_vehicle_preview.lock() {
            app.selected_vehicle = Some(idx as usize);
            let rw_path = std::path::PathBuf::from(app.rw_path.trim());
            let route_path = app
                .selected_route
                .and_then(|i| app.routes.get(i))
                .map(|r| r.path.clone())
                .unwrap_or_default();
            let scen_idx = match app.selected_scenario {
                Some(i) => i,
                None => return,
            };
            let scenario = match app.scenarios.get(scen_idx) {
                Some(s) => s.clone(),
                None => return,
            };
            let consist_idx = app.selected_consist.unwrap_or(0);
            let consists = match scenario.consists.as_ref() {
                Some(c) => c,
                None => return,
            };
            let vehicle = match consists.get(consist_idx).and_then(|c| c.vehicles.get(idx as usize)) {
                Some(v) => v.clone(),
                None => return,
            };
            let resolved = crate::blueprint::resolve_blueprint_file_cached_free(
                &rw_path,
                &route_path,
                &vehicle.blueprint,
                &vehicle.provider,
                &vehicle.product,
            );
            let exists = crate::blueprint::vehicle_blueprint_exists(
                &rw_path,
                &vehicle.provider,
                &vehicle.product,
                &vehicle.blueprint,
            );
            let path_text = if exists {
                if vehicle.provider.trim().is_empty() || vehicle.product.trim().is_empty() {
                    vehicle.blueprint.clone()
                } else {
                    format!(
                        "Assets\\{}\\{}\\{}",
                        vehicle.provider.trim(),
                        vehicle.product.trim(),
                        vehicle.blueprint.trim().replace('/', "\\")
                    )
                }
            } else {
                String::new()
            };
            let image = load_vehicle_preview_image(&rw_path, &route_path, &vehicle, resolved.as_ref());
            if let Some(sw) = scenario_handle_preview.upgrade() {
                if let Some(idx) = path_text.find("Assets"){
                    sw.set_selected_vehicle_path(path_text[idx..].into());
                } else {
                    sw.set_selected_vehicle_path(path_text.into());
                }
                sw.set_selected_vehicle_image(image);
                let details = format!(
                    "Поставщик: {}\nДополнение: {}\nФайл: {}\nНомер: {}",
                    vehicle.provider, vehicle.product, vehicle.blueprint,
                    vehicle.number.clone().unwrap_or_else(|| "—".to_owned())
                );
                sw.set_selected_vehicle_info(details.into());
            }
        }
    });
    let app_clone_open_vehicle_preview = app.clone();
    scenario_win.on_open_selected_vehicle_path(move || {
        if let Ok(app) = app_clone_open_vehicle_preview.lock() {
            let scen_idx = match app.selected_scenario {
                Some(i) => i,
                None => return,
            };
            let consist_idx = app.selected_consist.unwrap_or(0);
            let vehicle_idx = match app.selected_vehicle {
                Some(i) => i,
                None => return,
            };
            let scenario = match app.scenarios.get(scen_idx) {
                Some(s) => s.clone(),
                None => return,
            };
            let consists = match scenario.consists.as_ref() {
                Some(c) => c,
                None => return,
            };
            let consist = match consists.get(consist_idx) {
                Some(c) => c,
                None => return,
            };
            let vehicle = match consist.vehicles.get(vehicle_idx) {
                Some(v) => v.clone(),
                None => return,
            };
            let rw_path = app.rw_path.clone();
            let route_path = app
                .selected_route
                .and_then(|i| app.routes.get(i))
                .map(|r| r.path.clone())
                .unwrap_or_default();
            let resolved = crate::blueprint::resolve_blueprint_file_cached_free(
                std::path::Path::new(rw_path.trim()),
                std::path::Path::new(&route_path),
                &vehicle.blueprint,
                &vehicle.provider,
                &vehicle.product,
            );
            if let Some(path) = resolved {
                if path.is_file() {
                    let _ = std::process::Command::new("explorer.exe")
                        .arg("/select,")
                        .arg(path.as_os_str())
                        .spawn();
                } else {
                    let mut cur = path.as_path();
                    while !cur.exists() {
                        match cur.parent() {
                            Some(p) => cur = p,
                            None => break,
                        }
                    }
                    let _ = crate::windows::open_in_file_explorer(cur);
                }
            }
        }
    });
    scenario_win.on_open_vehicle_path(move |idx| {
        if let Ok(app) = app_clone17.lock() {
            let scen_idx = match app.selected_scenario {
                Some(i) => i,
                None => return,
            };
            let consist_idx = app.selected_consist.unwrap_or(0);
            let scenario = match app.scenarios.get(scen_idx) {
                Some(s) => s,
                None => return,
            };
            let consists = match scenario.consists.as_ref() {
                Some(c) => c,
                None => return,
            };
            let consist = match consists.get(consist_idx) {
                Some(c) => c,
                None => return,
            };
            let vehicle = match consist.vehicles.get(idx as usize) {
                Some(v) => v,
                None => return,
            };
            let rw_path = app.rw_path.clone();
            let route_path = app
                .selected_route
                .and_then(|i| app.routes.get(i))
                .map(|r| r.path.clone())
                .unwrap_or_default();
            let resolved = crate::blueprint::resolve_blueprint_file_cached_free(
                std::path::Path::new(rw_path.trim()),
                std::path::Path::new(&route_path),
                &vehicle.blueprint,
                &vehicle.provider,
                &vehicle.product,
            );
            if let Some(path) = resolved {
                if path.is_file() {
                    let _ = std::process::Command::new("explorer.exe")
                        .arg("/select,")
                        .arg(path.as_os_str())
                        .spawn();
                  } else {
                      let mut cur = path.as_path();
                      while !cur.exists() {
                          match cur.parent() {
                              Some(p) => cur = p,
                              None => break,
                          }
                      }
                      let _ = crate::windows::open_in_file_explorer(cur);
                  }
            } else {
                  // Если блюпринт не найден, ничего не открываем.
                }

        }
    });
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
                        let (rows, safe) = build_inventory_rows(&state, show_missing);
                        inv.set_inventory_rows(slint::ModelRc::new(slint::VecModel::from(rows)));
                        inv.set_inventory_is_safe(slint::ModelRc::new(slint::VecModel::from(safe)));
                    }
            }
        }
    });
    inventory_win.on_sort_ascending(move |column| {
        if let Some(inv) = inv_handle3.upgrade() {
            let show_missing = inv.get_show_missing_only();
            if let Ok(app) = app_clone8.lock() {
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
                        let (rows, safe) = build_inventory_rows(&state, show_missing);
                        inv.set_inventory_rows(slint::ModelRc::new(slint::VecModel::from(rows)));
                        inv.set_inventory_is_safe(slint::ModelRc::new(slint::VecModel::from(safe)));
                    }
            }
        }
    });
    inventory_win.on_sort_descending(move |column| {
        if let Some(inv) = inv_handle4.upgrade() {
            let show_missing = inv.get_show_missing_only();
            if let Ok(app) = app_clone9.lock() {
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
                        let (rows, safe) = build_inventory_rows(&state, show_missing);
                        inv.set_inventory_rows(slint::ModelRc::new(slint::VecModel::from(rows)));
                        inv.set_inventory_is_safe(slint::ModelRc::new(slint::VecModel::from(safe)));
                    }
            }
        }
    });
    inventory_win.on_search_changed(move |text| {
        if let Some(inv) = inv_handle5.upgrade() {
            let show_missing = inv.get_show_missing_only();
            if let Ok(app) = app_clone10.lock() {
                if let Ok(mut state) = app.asset_inventory_state.lock() {
                    state.search = text.to_string();
                }
                    if let Ok(state) = app.asset_inventory_state.lock() {
                        let (rows, safe) = build_inventory_rows(&state, show_missing);
                        inv.set_inventory_rows(slint::ModelRc::new(slint::VecModel::from(rows)));
                        inv.set_inventory_is_safe(slint::ModelRc::new(slint::VecModel::from(safe)));
                    }
            }
        }
    });
    let mut inv_last_row: i32 = -1;
    let mut inv_last_click = Instant::now();
    const INV_DOUBLE_CLICK_MS: u64 = 400;
    inventory_win.on_row_selected(move |row| {
        if let Some(inv) = inv_handle9.upgrade() {
            inv.set_selected_asset(row);
        }
        let now = Instant::now();
        let is_double = row == inv_last_row
            && now.saturating_duration_since(inv_last_click)
                <= Duration::from_millis(INV_DOUBLE_CLICK_MS);
        inv_last_row = row;
        inv_last_click = now;
        if !is_double || row < 0 {
            return;
        }
        if let Some(inv) = inv_handle9.upgrade() {
            let rows = inv.get_inventory_rows();
            if (row as usize) >= rows.row_count() {
                return;
            }
            let first_cell = rows.row_data(row as usize).and_then(|rm| rm.row_data(0));
            let path_text = first_cell.map(|c| c.text.to_string()).unwrap_or_default();
            if path_text.is_empty() {
                return;
            }
            let rw_path = if let Ok(app) = app_clone13.lock() {
                app.rw_path.clone()
            } else {
                String::new()
            };
            if rw_path.is_empty() {
                return;
            }
            let full = std::path::PathBuf::from(rw_path.trim())
                .join("Assets")
                .join(path_text.replace('/', "\\"));
            // Open nearest existing folder; select file if present.
            let target = if full.exists() {
                // open containing folder with the file selected
                let _ = std::process::Command::new("explorer.exe")
                    .arg("/select,")
                    .arg(full.as_os_str())
                    .spawn();
                return;
            } else {
                // walk up to nearest existing parent
                let mut cur = full.as_path();
                while !cur.exists() {
                    match cur.parent() {
                        Some(p) => cur = p,
                        None => break,
                    }
                }
                cur.to_path_buf()
            };
            let _ = open_in_file_explorer(&target);
        }
    });
    inventory_win.on_open_in_editor(move || {
        if let Some(inv) = inv_handle10.upgrade() {
            let row = inv.get_current_row();
            if row < 0 {
                return;
            }
            let rows = inv.get_inventory_rows();
            if (row as usize) >= rows.row_count() {
                return;
            }
            let first_cell = rows.row_data(row as usize).and_then(|rm| rm.row_data(0));
            let path_text = first_cell.map(|c| c.text.to_string()).unwrap_or_default();
            if path_text.is_empty() {
                return;
            }
            let rw_path = if let Ok(app) = app_clone14.lock() {
                app.rw_path.clone()
            } else {
                String::new()
            };
            if rw_path.is_empty() {
                return;
            }
            let full = std::path::PathBuf::from(rw_path.trim())
                .join("Assets")
                .join(path_text.replace('/', "\\"));
            if full.is_file() {
                let serz_path = crate::assets::find_serz_path(&full)
                    .unwrap_or_else(|| std::path::PathBuf::from("serz64.exe"));
                let app_for_status = app_clone14.clone();
                let _ = crate::external_editor::open_and_sync_with_vscode(
                    &full,
                    &serz_path,
                    move |result| {
                        if let Ok(mut app) = app_for_status.lock() {
                            app.status = result.unwrap_or_else(|error| error);
                        }
                    },
                );
            }
        }
    });
    let app_clone_replace = app.clone();
    inventory_win.on_replace_inventory_asset(move |selected| {
        if selected < 0 {
            return;
        }
        let Some(inv) = inv_handle11.upgrade() else {
            return;
        };
        let rows = inv.get_inventory_rows();
        if (selected as usize) >= rows.row_count() {
            return;
        }
        let first_cell = rows.row_data(selected as usize).and_then(|rm| rm.row_data(0));
        let asset_path = first_cell.map(|c| c.text.to_string()).unwrap_or_default();
        if asset_path.is_empty() {
            return;
        }
        let rw_path = if let Ok(app) = app_clone_replace.lock() {
            app.rw_path.clone()
        } else {
            return;
        };
        if rw_path.trim().is_empty() {
            return;
        }
        let asset = if let Ok(app) = app_clone_replace.lock() {
            if let Ok(state) = app.asset_inventory_state.lock() {
                state.assets.iter().find(|a| a.path == asset_path).cloned()
            } else {
                None
            }
        } else {
            None
        };
        let Some(asset) = asset else {
            return;
        };
        if !crate::assets::is_safe_for_global_replacement(&asset.path, &asset.uses) {
            if let Ok(mut app) = app_clone_replace.lock() {
                app.status = "Замена этого ассета запрещена (путевая инфраструктура).".to_owned();
            }
            return;
        }

        let assets_dir = std::path::PathBuf::from(rw_path.trim()).join("Assets");
        let mut dialog = rfd::FileDialog::new()
            .set_title("Заменить в маршруте...")
            .set_directory(&assets_dir);
        dialog = dialog.add_filter("BIN", &["bin"]);
        let Some(picked) = dialog.pick_file() else {
            return;
        };
        let replacement_relative = picked
            .strip_prefix(&assets_dir)
            .map(|rel| rel.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| picked.to_string_lossy().replace('\\', "/"));

        let uses: Vec<_> = asset
            .uses
            .into_iter()
            .filter(|u| {
                let p = u.source_file.display().to_lowercase();
                !p.contains("tracks.bin")
                    && !p.contains("/networks/")
                    && !p.contains("\\networks\\")
            })
            .collect();
        if uses.is_empty() {
            return;
        }
        let missing = MissingAsset {
            path: asset.path.clone(),
            uses,
            suggested_repair: None,
            is_corrupted_link: false,
        };
        if let Ok(mut app) = app_clone_replace.lock() {
            app.inventory_replacement_pending_check = true;
            app.asset_replacement_queue
                .push((missing, picked, replacement_relative));
            app.start_next_asset_replacement();
        }
    });
    inventory_win.on_scan_umlauts(move || {
        if let Ok(app) = app_clone19.lock() {
            let _ = app
                .inventory_action_tx
                .send(crate::app::InventoryAction::ScanUmlauts);
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
                    inv.set_selected_asset(-1);
                    inv.set_search_query(state.search.clone().as_str().into());
                    let (rows, safe) = build_inventory_rows(&state, false);
                    inv.set_inventory_rows(slint::ModelRc::new(slint::VecModel::from(rows)));
                    inv.set_inventory_is_safe(slint::ModelRc::new(slint::VecModel::from(safe)));
                    inv.set_search_query(state.search.clone().as_str().into());
                }
                inv.show().unwrap();
            }
        }
    });
    let auditor_win = FolderAuditorWindow::new().unwrap();
    let auditor_handle = auditor_win.as_weak();
    let auditor_handle2 = auditor_win.as_weak();
    let all_audit_items: std::sync::Arc<std::sync::Mutex<Vec<BrokenResourceRef>>> =
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let current_display_items: std::sync::Arc<std::sync::Mutex<Vec<BrokenResourceRef>>> =
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let audit_sort_col: std::sync::Arc<std::sync::Mutex<usize>> = std::sync::Arc::new(std::sync::Mutex::new(0));
    let audit_sort_asc: std::sync::Arc<std::sync::Mutex<bool>> = std::sync::Arc::new(std::sync::Mutex::new(true));
    let all_audit_items_for_start = all_audit_items.clone();
    let current_display_items_for_start = current_display_items.clone();
    let audit_sort_col_for_start = audit_sort_col.clone();
    let audit_sort_asc_for_start = audit_sort_asc.clone();
    let all_audit_items_for_filter = all_audit_items.clone();
    let current_display_items_for_filter = current_display_items.clone();
    let audit_sort_col_for_filter = audit_sort_col.clone();
    let audit_sort_asc_for_filter = audit_sort_asc.clone();
    let all_audit_items_for_sort = all_audit_items.clone();
    let current_display_items_for_sort = current_display_items.clone();
    let audit_sort_col_for_sort = audit_sort_col.clone();
    let audit_sort_asc_for_sort = audit_sort_asc.clone();
    let current_display_items_for_open = current_display_items.clone();
    let app_clone_auditor = app.clone();
    ui.on_open_folder_auditor(move || {
        if let Some(w) = auditor_handle.upgrade() {
            let _ = w.show();
        }
    });
    auditor_win.on_pick_folder(move || {
        if let Some(w) = auditor_handle2.upgrade() {
            let current = w.get_folder_path().to_string();
            let mut dialog = rfd::FileDialog::new().set_title("Выберите папку для аудита");
            if !current.is_empty() {
                let p = std::path::PathBuf::from(&current);
                if p.is_dir() {
                    dialog = dialog.set_directory(&p);
                }
            }
            if let Some(path) = dialog.pick_folder() {
                w.set_folder_path(path.to_string_lossy().to_string().into());
            }
        }
    });
    let auditor_weak_for_start = auditor_win.as_weak();
    auditor_win.on_start_audit(move |folder_path| {
        println!("[Auditor] start-audit callback: {}", folder_path);
        let window_weak = auditor_weak_for_start.clone();
        let path = std::path::PathBuf::from(folder_path.as_str());
        if !path.exists() || !path.is_dir() {
            eprintln!("[Auditor] Invalid folder path: {:?}", path);
            return;
        }
        let rw_path = app_clone_auditor
            .lock()
            .map(|a| a.rw_path.clone())
            .unwrap_or_default();
        let max_threads = app_clone_auditor
            .lock()
            .map(|a| a.serz_process_limit)
            .unwrap_or(8)
            .max(1);
        let serz_path = crate::preferred_serz_path(std::path::Path::new(&rw_path));
        let assets_root = path
            .ancestors()
            .find(|p| {
                p.file_name()
                    .map(|name| name.to_string_lossy().eq_ignore_ascii_case("Assets"))
                    .unwrap_or(false)
            })
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| {
                path.parent()
                    .and_then(|p| p.parent())
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|| path.clone())
            });
        let ap_cache = ApArchiveCache::new();
        let path_for_thread = path.clone();
        let window_weak_thread = window_weak.clone();
            let all_store = all_audit_items_for_start.clone();
            let current_store = current_display_items_for_start.clone();
            let sort_col_store = audit_sort_col_for_start.clone();
            let sort_asc_store = audit_sort_asc_for_start.clone();
            let results_store = all_store.clone();
            std::thread::spawn(move || {
                println!("[Auditor] Starting audit for: {:?}", path_for_thread);
                let progress_cb: std::sync::Arc<dyn Fn(usize, usize) + Send + Sync> = {
                    let ww = window_weak_thread.clone();
                    std::sync::Arc::new(move |done, total| {
                        let ww = ww.clone();
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(w) = ww.upgrade() {
                                w.set_audit_progress(done as f32 / total as f32);
                                w.set_audit_status(format!("Сканирование: {} / {} файлов...", done, total).into());
                            }
                        });
                    })
                };
                let report = crate::audit_folder_with_progress(&path_for_thread, &assets_root, &serz_path, max_threads, &ap_cache, Some(progress_cb));
                println!("[Auditor] Audit done, broken: {}", report.broken_references.len());
                if let Ok(mut store) = results_store.lock() {
                    *store = report.broken_references.clone();
                }
                let col = *sort_col_store.lock().unwrap();
                let asc = *sort_asc_store.lock().unwrap();
                let filter = window_weak_thread.upgrade().map(|w| w.get_active_filter().to_string()).unwrap_or_else(|| "all".to_string());
                let mut items: Vec<BrokenResourceRef> = report.broken_references.clone();
                filter_items(&mut items, &filter);
                sort_items(&mut items, col, asc);
                if let Ok(mut store) = current_store.lock() {
                    *store = items.clone();
                }
                let ww = window_weak_thread.clone();
                slint::invoke_from_event_loop(move || {
                    if let Some(w) = ww.upgrade() {
                        let mut rows: Vec<slint::ModelRc<slint::StandardListViewItem>> = Vec::new();
                        for b in &items {
                            let kind = match b.kind {
                                AssetResourceKind::Geometry => "Геометрия",
                                AssetResourceKind::Texture => "Текстура",
                                AssetResourceKind::Animation => "Анимация",
                                AssetResourceKind::Script => "Скрипт",
                                AssetResourceKind::Sound => "Звуки",
                                AssetResourceKind::ChildBlueprint => "Дочерние объекты",
                            };
                            let mut item0 = slint::StandardListViewItem::default();
                            item0.text = kind.into();
                            let mut item1 = slint::StandardListViewItem::default();
                            item1.text = b.expected_path.to_string_lossy().to_string().into();
                            let mut item2 = slint::StandardListViewItem::default();
                            item2.text = b.parent_file.to_string_lossy().to_string().into();
                            rows.push(slint::ModelRc::new(slint::VecModel::from(vec![item0, item1, item2])));
                        }
                        w.set_broken_rows(slint::ModelRc::new(slint::VecModel::from(rows)));
                        w.set_audit_progress(1.0);
                        w.set_audit_status(format!("Готово: {} проблем", items.len()).into());
                        w.set_selected_row(-1);
                    }
                })
                .ok();
            });
    });
    let auditor_weak_for_filter = auditor_win.as_weak();
    auditor_win.on_filter_changed(move |filter| {
        println!("[Auditor] filter-changed: {}", filter);
        let ww = auditor_weak_for_filter.clone();
        let all_store = all_audit_items_for_filter.clone();
        let current_store = current_display_items_for_filter.clone();
        let sort_col_store = audit_sort_col_for_filter.clone();
        let sort_asc_store = audit_sort_asc_for_filter.clone();
        let all_items = all_store.lock().map(|g| g.clone()).unwrap_or_default();
        let col = *sort_col_store.lock().unwrap();
        let asc = *sort_asc_store.lock().unwrap();
        let mut items = all_items;
        filter_items(&mut items, filter.as_str());
        sort_items(&mut items, col, asc);
        if let Ok(mut store) = current_store.lock() {
            *store = items.clone();
        }
        slint::invoke_from_event_loop(move || {
            if let Some(w) = ww.upgrade() {
                w.set_active_filter(filter.clone().into());
                let mut rows: Vec<slint::ModelRc<slint::StandardListViewItem>> = Vec::new();
                for b in &items {
                    let kind = match b.kind {
                        AssetResourceKind::Geometry => "Геометрия",
                        AssetResourceKind::Texture => "Текстура",
                        AssetResourceKind::Animation => "Анимация",
                        AssetResourceKind::Script => "Скрипт",
                        AssetResourceKind::Sound => "Звуки",
                        AssetResourceKind::ChildBlueprint => "Дочерние объекты",
                    };
                    let mut item0 = slint::StandardListViewItem::default();
                    item0.text = kind.into();
                    let mut item1 = slint::StandardListViewItem::default();
                    item1.text = b.expected_path.to_string_lossy().to_string().into();
                    let mut item2 = slint::StandardListViewItem::default();
                    item2.text = b.parent_file.to_string_lossy().to_string().into();
                    rows.push(slint::ModelRc::new(slint::VecModel::from(vec![item0, item1, item2])));
                }
                w.set_broken_rows(slint::ModelRc::new(slint::VecModel::from(rows)));
                w.set_selected_row(-1);
            }
        })
        .ok();
    });
    let auditor_weak_for_open = auditor_win.as_weak();
    let current_display_for_open = current_display_items_for_open.clone();
    auditor_win.on_open_source_file(move |row| {
        let items = current_display_for_open.lock().map(|g| g.clone()).unwrap_or_default();
        if row < 0 || row as usize >= items.len() {
            return;
        }
        let target_path = items[row as usize].parent_file.clone();
        println!("[Auditor] Opening path: {:?}", target_path);
        let _ = open_in_explorer(&target_path);
        let _ = auditor_weak_for_open;
    });
    let auditor_weak_for_missing = auditor_win.as_weak();
    let current_display_for_missing = current_display_items_for_open.clone();
    auditor_win.on_open_missing_target_folder(move |row| {
        let items = current_display_for_missing.lock().map(|g| g.clone()).unwrap_or_default();
        if row < 0 || row as usize >= items.len() {
            return;
        }
        let target_path = items[row as usize].expected_path.clone();
        println!("[Auditor] Opening path: {:?}", target_path);
        let _ = open_in_explorer(&target_path);
        let _ = auditor_weak_for_missing;
    });
    let auditor_weak_for_sort = auditor_win.as_weak();
    let all_for_sort = all_audit_items_for_sort.clone();
    let current_for_sort = current_display_items_for_sort.clone();
    let sort_col_for_sort = audit_sort_col_for_sort.clone();
    let sort_asc_for_sort = audit_sort_asc_for_sort.clone();
    auditor_win.on_sort_column(move |col| {
        let col = col as usize;
        let mut c = sort_col_for_sort.lock().unwrap();
        let mut a = sort_asc_for_sort.lock().unwrap();
        if *c == col {
            *a = !*a;
        } else {
            *c = col;
            *a = true;
        }
        let col_now = *c;
        let asc_now = *a;
        drop(c);
        drop(a);
        let filter = auditor_weak_for_sort.upgrade().map(|w| w.get_active_filter().to_string()).unwrap_or_else(|| "all".to_string());
        let all_items = all_for_sort.lock().map(|g| g.clone()).unwrap_or_default();
        let mut items = all_items;
        filter_items(&mut items, &filter);
        sort_items(&mut items, col_now, asc_now);
        if let Ok(mut store) = current_for_sort.lock() {
            *store = items.clone();
        }
        let ww = auditor_weak_for_sort.clone();
        slint::invoke_from_event_loop(move || {
            if let Some(w) = ww.upgrade() {
                w.set_sort_col(col_now as i32);
                w.set_sort_ascending(asc_now);
                let mut rows: Vec<slint::ModelRc<slint::StandardListViewItem>> = Vec::new();
                for b in &items {
                    let kind = match b.kind {
                        AssetResourceKind::Geometry => "Геометрия",
                        AssetResourceKind::Texture => "Текстура",
                        AssetResourceKind::Animation => "Анимация",
                        AssetResourceKind::Script => "Скрипт",
                        AssetResourceKind::Sound => "Звуки",
                        AssetResourceKind::ChildBlueprint => "Дочерние объекты",
                    };
                    let mut item0 = slint::StandardListViewItem::default();
                    item0.text = kind.into();
                    let mut item1 = slint::StandardListViewItem::default();
                    item1.text = b.expected_path.to_string_lossy().to_string().into();
                    let mut item2 = slint::StandardListViewItem::default();
                    item2.text = b.parent_file.to_string_lossy().to_string().into();
                    rows.push(slint::ModelRc::new(slint::VecModel::from(vec![item0, item1, item2])));
                }
                w.set_broken_rows(slint::ModelRc::new(slint::VecModel::from(rows)));
                w.set_selected_row(-1);
            }
        })
        .ok();
    });
    let auditor_weak_for_edit = auditor_win.as_weak();
    let current_display_for_edit = current_display_items.clone();
    let app_clone_edit = app.clone();
    auditor_win.on_edit_in_vscode(move |row| {
        let items = current_display_for_edit
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default();
        if row < 0 || row as usize >= items.len() {
            return;
        }
        let item = &items[row as usize];
        let asset_path = if item.parent_file.extension().map(|e| e.eq_ignore_ascii_case("bin") || e.eq_ignore_ascii_case("geopcdx")).unwrap_or(false) {
            item.parent_file.clone()
        } else if item.expected_path.extension().map(|e| e.eq_ignore_ascii_case("bin") || e.eq_ignore_ascii_case("geopcdx")).unwrap_or(false) {
            item.expected_path.clone()
        } else {
            let w = auditor_weak_for_edit.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(win) = w.upgrade() {
                    win.set_editor_status("Выбранный элемент не является .bin/.GeoPcDx".into());
                }
            });
            return;
        };
    let rw_path = app_clone_edit.lock().map(|a| a.rw_path.clone()).unwrap_or_default();
    let serz_path = crate::assets::find_serz_path(&asset_path)
        .or_else(|| {
            let p = crate::preferred_serz_path(std::path::Path::new(&rw_path));
            if p.is_file() {
                Some(p)
            } else {
                None
            }
        })
        .unwrap_or_else(|| PathBuf::from("serz.exe"));
        let weak = auditor_weak_for_edit.clone();
        std::thread::spawn(move || {
            let cb_weak = weak.clone();
            let callback = move |res: Result<String, String>| {
                let msg = match res {
                    Ok(s) => s,
                    Err(e) => format!("✗ {}", e),
                };
                let w = cb_weak.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(win) = w.upgrade() {
                        win.set_editor_status(msg.into());
                    }
                });
            };
            if let Err(e) = crate::external_editor::open_and_sync_with_vscode(&asset_path, &serz_path, callback) {
                let w = weak.clone();
                let msg = format!("✗ {}", e);
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(win) = w.upgrade() {
                        win.set_editor_status(msg.into());
                    }
                });
            }
        });
    });

    ui.run().unwrap();
}

#[cfg(test)]
mod tests {
    use super::{
        check_route, decode_xml_text, extract_asset_references, extract_scenario_clues,
        fallback_serz_path, inspect_route_folder, is_skipped_route_scan_directory,
        parse_route_name, preferred_serz_path, scan_routes,
        scan_routes_with_ignored, validate_serz_process, ScenarioClueCollection,
    };
    use std::fs;
    use std::io::Write;
    #[cfg(windows)]
    use std::path::Path;
    use std::path::PathBuf;

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
        // Три папки маршрута и RouteProperties.xml в корне маршрута.
        assert_eq!(included.len(), 4);
        assert!(!included_names.iter().any(|name| name == "ThirdParty"));
        assert!(included.contains(&route.join("RouteProperties.xml")));

        let without_scenarios = super::assets::collect_route_files(&route, true).unwrap();
        assert_eq!(without_scenarios.len(), 3);
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
        let ignored_scan = scan_routes_with_ignored(temp.path(), &ignored_ids, 4).unwrap();
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
        assert!(super::is_suspicious_archive_member(1024 * 1024 * 1024, 0));
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
        let route = std::path::PathBuf::from(
            r"X:\SteamLibrary\steamapps\common\RailWorks\Content\Routes\8378d7b6-30ad-4c29-a5ac-16d3c2b26ff7",
        );
        let rw = std::path::PathBuf::from(r"X:\SteamLibrary\steamapps\common\RailWorks");
        let assets = rw.join("Assets");
        let mut serz = rw.join("serz64.exe");
        if !serz.is_file() {
            serz = rw.join("serz.exe");
        }
        println!("using serz: {} exists={}", serz.display(), serz.is_file());
        let start = std::time::Instant::now();
        let res = check_route(&route, &assets, &serz, true, 8, |msg| {
            println!("[{:.1}s] {}", start.elapsed().as_secs_f32(), msg);
        });
        println!(
            "RESULT after {:.1}s: {:?}",
            start.elapsed().as_secs_f32(),
            res.map(|r| (
                r.asset_inventory.len(),
                r.missing_assets.len(),
                r.conversion_errors.len(),
                r.structure_issues.len()
            ))
        );
    }
}
