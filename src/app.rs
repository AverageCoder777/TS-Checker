//! egui application state, windows, and user interactions.

use super::*;
use std::collections::HashMap;
use std::path::Path;

enum CheckMessage {
    Progress(String),
    Finished(Result<CheckResult, String>),
}

enum InventoryAction {
    ScanUmlauts,
    Repair {
        asset: UmlautAsset,
        fixes: CharacterFixes,
    },
}

#[derive(Default)]
pub(crate) struct AssetInventoryWindowState {
    open: bool,
    pub(crate) search: String,
    show_umlauts: bool,
    show_tiles: bool,
    scanning_umlauts: bool,
    error: Option<String>,
    pub(crate) assets: Vec<ReferencedAsset>,
    umlaut_assets: Vec<UmlautAsset>,
    /// Ассет, для которого открыто окно подтверждения транслитерации.
    pending_repair: Option<UmlautAsset>,
    /// Замены символов, предложенные пользователем.
    character_fixes: CharacterFixes,
    pub(crate) sort: InventorySort,
}

pub(crate) fn decode_route_icon(path: &Path) -> image::ImageResult<image::DynamicImage> {
    let reader = image::ImageReader::open(path).map_err(image::ImageError::IoError)?;
    let reader = reader.with_guessed_format().map_err(image::ImageError::IoError)?;
    reader.decode()
}

pub(crate) fn route_panel_widths(
    available_width: f32,
    panel_gap: f32,
    route_content_width: f32,
) -> (f32, f32) {
    let usable_width = (available_width - panel_gap).max(0.0);
    let routes_width = route_content_width.min(usable_width * 2.0 / 3.0);
    let check_width = (usable_width - routes_width).max(0.0);
    (routes_width, check_width)
}

/// Строка таблицы инвентаря: путь ассета, список использований, признак отсутствия
/// на диске и ассет с умляутами, если его нужно починить.
/// Данные для отрисовки таблицы инвентаря.
type SavedAsset = (PathBuf, String, Option<PathBuf>);

/// Результат проверки ассетов на умляуты: найденные ассеты и список ошибок.
type InventoryScanOutcome = (Vec<UmlautAsset>, Vec<String>);

pub(crate) struct CheckerApp {
    pub(crate) rw_path: String,
    startup_scan_started: bool,
    pub(crate) routes: Vec<Route>,
    pub(crate) selected_route: Option<usize>,
    /// Сценарии выбранного маршрута.
    scenarios: Vec<RouteScenario>,
    /// Маршрут, для которого прочитаны сценарии.
    scenarios_route: Option<String>,
    /// Выбранный сценарий в списке.
    selected_scenario: Option<usize>,
    /// Выбранный состав внутри открытого окна сценария.
    selected_consist: Option<usize>,
    /// Открыто ли окно выбранного сценария.
    scenario_window_open: bool,
    /// Кэш разрешённых путей блюпринтов техники, чтобы не обходить Assets
    /// повторно при каждом кадре отрисовки окна сценария.
    /// Результат проверки наличия файлов для составов открытого сценария,
    /// считается один раз, чтобы не пересчитывать при каждом кадре egui.
    scenario_consist_info: Option<Vec<(String, bool)>>,
    /// Кэш наличия файлов для техники: [состав][вагон] = файл_на_месте.
    scenario_vehicle_files: Option<Vec<Vec<bool>>>,
    pub(crate) search_query: String,
    ignore_scenarios: bool,
    pub(crate) result: Option<CheckResult>,
    pub(crate) status: String,
    pub(crate) incomplete_folders: Vec<IncompleteRouteFolder>,
    incomplete_window_open: bool,
    selected_incomplete: Option<usize>,
    pub(crate) ignored_route_ids: HashSet<String>,
    ignore_list_window_open: bool,
    route_scan: Option<Receiver<Result<RouteScanOutcome, String>>>,
    check: Option<Receiver<CheckMessage>>,
    asset_replacement: Option<Receiver<(String, Result<AssetReplacementReport, String>)>>,
    umlaut_repair: Option<Receiver<(String, Result<UmlautRepairReport, String>)>>,
    asset_replacement_feedback: Option<Result<String, String>>,
    /// Ассет, для которого в главном окне открыто подтверждение транслитерации.
    pending_umlaut_repair: Option<UmlautAsset>,
    pending_asset_replacement: Option<PendingAssetReplacement>,
    /// Замены символов, предложенные пользователем для сломанных имён.
    character_fixes: CharacterFixes,
    pending_delete: Option<Vec<PathBuf>>,
    delete_paths: Option<Vec<PathBuf>>,
    delete_rx: Option<Receiver<Result<usize, String>>>,
    delete_feedback: Option<Result<usize, String>>,
    settings_window_open: bool,
    asset_viewer_open: bool,
    asset_editor_only: bool,
    asset_viewer_document: Option<EditorDocument>,
    asset_viewer_load: Option<Receiver<Result<EditorDocument, String>>>,
    asset_viewer_save: Option<Receiver<Result<SavedAsset, String>>>,
    route_thumbnails_enabled: bool,
    serz_process_limit: usize,
    locoswap_path: String,
    attempted_route_icons: HashSet<PathBuf>,
    pub(crate) asset_inventory_state: Arc<Mutex<AssetInventoryWindowState>>,
    inventory_action_tx: Sender<InventoryAction>,
    inventory_action_rx: Receiver<InventoryAction>,
    inventory_scan: Option<Receiver<Result<InventoryScanOutcome, String>>>,
    pub(crate) missing_assets_popup: Option<Vec<String>>,
    pub(crate) missing_assets_shown: bool,
}

impl Default for CheckerApp {
    fn default() -> Self {
        let rw_path = load_last_game_path();
        let (ignored_route_ids, mut status) = match load_ignored_route_ids() {
            Ok(ids) => (ids,
                "Укажите путь к RailWorks и просканируйте маршруты.".to_owned(),
            ),
            Err(error) => (HashSet::new(), error),
        };
        let (route_thumbnails_enabled, settings_warning) = match load_route_thumbnail_setting() {
            Ok(enabled) => (enabled, None),
            Err(error) => (false, Some(error)),
        };
        let (serz_process_limit, serz_settings_warning) = match load_serz_process_limit() {
            Ok(limit) => (limit, None),
            Err(error) => (
                DEFAULT_CONCURRENT_SERZ.min(maximum_serz_processes()),
                Some(error),
            ),
        };
        let (inventory_action_tx, inventory_action_rx) = mpsc::channel();
        if let Some(warning) = settings_warning {
            status.push(' ');
            status.push_str(&warning);
        }
        if let Some(warning) = serz_settings_warning {
            status.push(' ');
            status.push_str(&warning);
        }
        Self {
            rw_path,
            startup_scan_started: false,
            routes: Vec::new(),
            incomplete_folders: Vec::new(),
            incomplete_window_open: false,
            selected_incomplete: None,
            ignored_route_ids,
            ignore_list_window_open: false,
            selected_route: None,
            scenarios: Vec::new(),
            scenarios_route: None,
            selected_scenario: None,
            selected_consist: None,
            scenario_window_open: false,
            scenario_consist_info: None,
            scenario_vehicle_files: None,
            search_query: String::new(),
            ignore_scenarios: true,
            result: None,
            status,
            route_scan: None,
            check: None,
            asset_replacement: None,
            umlaut_repair: None,
            asset_replacement_feedback: None,
            pending_umlaut_repair: None,
            pending_asset_replacement: None,
            character_fixes: CharacterFixes::new(),
            pending_delete: None,
            delete_paths: None,
            delete_rx: None,
            delete_feedback: None,
            settings_window_open: false,
            asset_viewer_open: false,
            asset_editor_only: false,
            asset_viewer_document: None,
            asset_viewer_load: None,
            asset_viewer_save: None,
            route_thumbnails_enabled,
            serz_process_limit,
            locoswap_path: load_locoswap_path(),
            attempted_route_icons: HashSet::new(),
            asset_inventory_state: Arc::new(Mutex::new(AssetInventoryWindowState::default())),
            inventory_action_tx,
            inventory_action_rx,
            inventory_scan: None,
            missing_assets_popup: None,
            missing_assets_shown: false,
        }
    }
}

impl CheckerApp {
    fn show_asset_inventory_viewport(&self, ctx: &egui::Context) {
        let is_open = match self.asset_inventory_state.lock() {
            Ok(state) => state.open,
            Err(error) => {
                eprintln!("Не удалось получить состояние окна инвентаря: {error}");
                return;
            }
        };
        if !is_open {return;}

        let state = Arc::clone(&self.asset_inventory_state);
        let action_tx = self.inventory_action_tx.clone();
        let assets_root = PathBuf::from(self.rw_path.trim()).join("Assets");
        let repair_busy = self.check.is_some()
            || self.asset_replacement.is_some()
            || self.inventory_scan.is_some()
            || self.umlaut_repair.is_some()
            || self.selected_route.is_none();
        let scan_busy = repair_busy || self.inventory_scan.is_some();
        ctx.show_viewport_deferred(
            egui::ViewportId::from_hash_of("asset_inventory"),
            egui::ViewportBuilder::default().with_title("Инвентарь ассетов маршрута").with_inner_size([900.0, 700.0]),
            move |ui, _class| {
                let mut state = match state.lock() {
                    Ok(state) => state,
                    Err(error) => {
                        eprintln!("Не удалось получить состояние окна инвентаря: {error}");
                        return;
                    }
                };
                if ui.ctx().input(|input| input.viewport().close_requested())
                {
                    state.open = false;
                    return;
                }

                egui::CentralPanel::default().show(ui, |ui| {
                    ui.heading("Все используемые ассеты маршрута");
                    ui.horizontal(|ui| {
                        ui.label(format!("Всего: {}", state.assets.len()));
                        if ui.add_enabled(
                                !state.scanning_umlauts && !scan_busy,
                                egui::Button::new("Проверить все ассеты на умляуты"),
                            ).clicked()
                        {
                            state.scanning_umlauts = true;
                            state.show_umlauts = true;
                            state.error = None;
                            if let Err(error) = action_tx.send(InventoryAction::ScanUmlauts) {
                                state.scanning_umlauts = false;
                                state.error =
                                    Some(format!("Не удалось запустить проверку: {error}"));
                            } else {}
                        }
                        ui.selectable_value(&mut state.show_umlauts, false, "Все ассеты");
                        ui.selectable_value(&mut state.show_umlauts, true, "Требуют проверки");
                        if ui.button(if state.show_tiles {"Скрыть тайлы"} else {"Показать тайлы"}).clicked()
                        {
                            state.show_tiles = !state.show_tiles;
                        }
                    });
                    if state.scanning_umlauts {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("Проверка имён и цепочек ссылок...");
                        });
                    } else if state.show_umlauts {
                        ui.label(format!("Найдено для проверки: {}",state.umlaut_assets.len()));
                    }
                    if let Some(error) = &state.error {
                        ui.colored_label(egui::Color32::LIGHT_RED, error);
                    }
                    ui.add(
                        egui::TextEdit::singleline(&mut state.search).hint_text("Поиск по пути, тайлу или исходному файлу"),
                    );
                    ui.separator();

                    let search = state.search.to_lowercase();
                    // Карты собираются один раз на кадр: поиск по списку для
                    // каждой строки стоил бы времени при тысячах ассетов.
                    let umlaut_by_path: HashMap<&str, &UmlautAsset> = state.umlaut_assets.iter()
                        .map(|asset| (asset.path.as_str(), asset)).collect();
                    let missing_by_path: HashMap<&str, bool> = state
                        .assets.iter().map(|asset| (asset.path.as_str(), asset.missing)).collect();
                    let mut rows: Vec<InventoryRow<'_>> =
                        if state.show_umlauts {
                            state.umlaut_assets.iter().filter(|asset| {
                                    search.is_empty()
                                        || asset.path.to_lowercase().contains(&search)
                                        || asset.uses.iter().any(|usage| {
                                            usage.tile.to_lowercase().contains(&search)
                                            || usage.source_file.display().to_string().to_lowercase().contains(&search)
                                        })
                                }).map(|asset| {
                                    let missing = missing_by_path.get(asset.path.as_str()).copied().unwrap_or(false);
                                    (
                                        asset.path.as_str(),
                                        asset.uses.as_slice(),
                                        missing,
                                        Some(asset),
                                    )
                                }).collect()
                        } else {
                            state.assets.iter().filter(|asset| {
                                    search.is_empty()
                                        || asset.path.to_lowercase().contains(&search)
                                        || asset.uses.iter().any(|usage| {
                                            usage.tile.to_lowercase().contains(&search)
                                            || usage.source_file.display().to_string().to_lowercase().contains(&search)
                                        })
                                }).map(|asset| {(
                                        asset.path.as_str(),
                                        asset.uses.as_slice(),
                                        asset.missing,
                                        umlaut_by_path.get(asset.path.as_str()).copied(),
                                    )
                                }).collect()
                        };

                    let mut actions = InventoryRowActions::default();
                    let show_tiles = state.show_tiles;
                    // Ширина таблицы считается с запасом на полосу прокрутки, чтобы
                    // шапка и строки оставались в одних и тех же границах.
                    let table_width =
                        (ui.available_width() - ui.spacing().scroll.allocated_width()).max(320.0);
                    let layout = InventoryLayout::new(table_width, show_tiles);
                    let mut sort = state.sort;
                    sort_inventory_rows(&mut rows, sort);
                    let table_data = InventoryTable {
                        rows: &rows,
                        assets_root: &assets_root,
                        layout,
                        show_tiles,
                        repair_busy,
                    };
                    asset_inventory_table(ui, &table_data, &mut sort, &mut actions);
                    state.sort = sort;
                    if let Some(folder) = actions.open_folder {
                        if let Err(error) = open_in_file_explorer(&folder) {
                            state.error = Some(error);
                        }
                    }
                    if let Some(asset) = actions.repair {
                        state.pending_repair = Some(asset);
                    }
                    // Окно подтверждения открываем здесь же: в отдельном окне
                    // инвентаря остаётся фокус клавиатуры, иначе в поля нельзя
                    // вводить символы.
                    if let Some(asset) = state.pending_repair.clone() {
                        let mut fixes = state.character_fixes.clone();
                        let decision = umlaut_repair_window(ui, &asset, &mut fixes);
                        // Введённые символы сохраняем сразу: иначе они
                        // теряются до следующего кадра.
                        state.character_fixes = fixes.clone();
                        match decision {
                            Some(RepairDecision::Cancel) => {
                                state.pending_repair = None;
                            }
                            Some(RepairDecision::Confirm) => {
                                state.pending_repair = None;
                                // Пустое поле означает значение по умолчанию.
                                let mut effective = fixes;
                                for character in &asset.unsupported {
                                    effective
                                        .entry(*character)
                                        .or_insert_with(|| default_replacement(*character));
                                }
                                state.character_fixes = effective.clone();
                                if let Err(error) = action_tx.send(InventoryAction::Repair {
                                    asset,
                                    fixes: effective,
                                }) {
                                    state.error = Some(format!(
                                        "Не удалось запустить транслитерацию: {error}"
                                    ));
                                }
                            }
                            None => {}
                        }
                    }
                });
            },
        );
    }

    pub(crate) fn open_asset_inventory(&mut self) {
        let Some(result) = self.result.as_ref() else {
            return;
        };
        match self.asset_inventory_state.lock() {
            Ok(mut state) => {
                state.assets = result.asset_inventory.clone();
                state.umlaut_assets = result.umlaut_assets.clone();
                state.search.clear();
                state.show_umlauts = false;
                state.show_tiles = true;
                state.error = None;
                state.open = true;
            }
            Err(error) => self.status = format!("Не удалось открыть инвентарь ассетов: {error}"),
        }
    }

    fn start_inventory_scan(&mut self) {
        let Some(result) = self.result.as_ref() else {
            self.status = "Сначала выполните проверку маршрута.".to_owned();
            return;
        };
        if self.inventory_scan.is_some() {
            return;
        }
        let inventory = result.asset_inventory.clone();
        let assets_path = PathBuf::from(self.rw_path.trim()).join("Assets");
        let serz_path = preferred_serz_path(Path::new(self.rw_path.trim()));
        let max_concurrent_serz = self.serz_process_limit;
        let (tx, rx) = mpsc::channel();
        self.inventory_scan = Some(rx);
        self.status = "Проверка ассетов маршрута на умляуты...".to_owned();
        thread::spawn(move || {
            let result =scan_umlaut_assets(&inventory, &assets_path, &serz_path, max_concurrent_serz);
            let _ = tx.send(result);
        });
    }

    fn start_umlaut_repair(&mut self, asset: UmlautAsset, fixes: CharacterFixes) {
        let Some(route) = self.selected_route.and_then(|index| self.routes.get(index)).cloned()
        else {
            return;
        };
        let rw_path = PathBuf::from(self.rw_path.trim());
        let assets_path = rw_path.join("Assets");
        let serz_path = preferred_serz_path(&rw_path);
        let (tx, rx) = mpsc::channel();
        self.umlaut_repair = Some(rx);
        self.asset_replacement_feedback = None;
        self.status = format!("Проверка цепочки ассета {}...", asset.path);
        thread::spawn(move || {
            let path = asset.path.clone();
            let result = repair_umlaut_asset_chain(
                &route.path,
                &assets_path,
                &serz_path,
                &asset.path,
                &asset.uses,
                &fixes,
            )
            .map(|mut report| {
                let mut uses: Vec<_> = asset.uses.iter().map(|usage| AssetReplacementLogUse {
                        tile: usage.tile.clone(),
                        source_file: usage.source_file.display(),
                    }).collect();
                uses.sort_by(|left, right| {
                    left.tile.cmp(&right.tile).then_with(|| left.source_file.cmp(&right.source_file))
                });
                let mut entry = AssetReplacementLogEntry {
                    timestamp_unix_seconds: 0,
                    operation: "Транслитерация цепочки ассетов".to_owned(),
                    route_id: Some(route.id.clone()),
                    route_name: Some(route.name.clone()),
                    route_path: Some(route.path.clone()),
                    original_files: report.files.iter().map(|path| path.display().to_string()).collect(),
                    missing_asset: Some(path.clone()),
                    replacement_asset: Some(report.new_asset_path.clone()),
                    replaced_files: report.changed_files,
                    replaced_instances: report.changed_references,
                    uses,
                    backups: report.backups.clone(),
                };
                match asset_replacement_log_file().ok_or_else(|| {"Windows APPDATA не определена; журнал замен не сохранён.".to_owned()})
                    .and_then(|log_path| {append_asset_replacement_log(&log_path, &mut entry).map(|()| log_path)}) 
                {
                    Ok(log_path) => report.log_file = Some(log_path),
                    Err(error) => report.log_error = Some(error),
                }
                report
            });
            let _ = tx.send((path, result));
        });
    }

    pub(crate) fn start_asset_replacement(
        &mut self,
        missing: MissingAsset,
        replacement: PathBuf,
        transliterate: bool,
        replacement_relative: String,
        fixes: CharacterFixes,
    ) {
        let Some(route) = self.selected_route.and_then(|index| self.routes.get(index)).cloned()
        else {return;};
        let rw_path = PathBuf::from(self.rw_path.trim());
        let assets_path = rw_path.join("Assets");
        let serz_path = preferred_serz_path(&rw_path);
        let (tx, rx) = mpsc::channel();
        self.asset_replacement = Some(rx);
        self.asset_replacement_feedback = None;
        self.status = format!("Подготовка замены ассета {}...", missing.path);
        thread::spawn(move || {
            let missing_path = missing.path.clone();
            // Сначала новый ассет переводится в английские имена: замена
            // должна ссылаться на уже переименованный файл.
            let replacement = if transliterate {
                match repair_umlaut_asset_chain(
                    &route.path,
                    &assets_path,
                    &serz_path,
                    &replacement_relative,
                    &[],
                    &fixes,
                ) {
                    Ok(report) => assets_path.join(report.new_asset_path),
                    Err(error) => {
                        let _ = tx.send((
                            missing_path,
                            Err(format!(
                                "Не удалось переименовать новый ассет под английские имена: {error}"
                            )),
                        ));
                        return;
                    }
                }
            } else {
                replacement
            };
            let result = replace_missing_asset(
                &route.path,
                &assets_path,
                &serz_path,
                &missing.path,
                &replacement,
                &missing.uses,
            )
            .map(|mut report| {
                let mut original_files: Vec<_> = missing
                    .uses
                    .iter()
                    .map(|usage| usage.source_file.display())
                    .collect::<HashSet<_>>()
                    .into_iter()
                    .collect();
                original_files.sort();
                let mut entry = AssetReplacementLogEntry {
                    timestamp_unix_seconds: 0,
                    operation: "Замена отсутствующего ассета".to_owned(),
                    route_id: Some(route.id.clone()),
                    route_name: Some(route.name.clone()),
                    route_path: Some(route.path.clone()),
                    original_files,
                    missing_asset: Some(missing.path.clone()),
                    replacement_asset: Some(report.replacement_asset.clone()),
                    replaced_files: report.replaced_files,
                    replaced_instances: report.replaced_instances,
                    uses: missing
                        .uses
                        .iter()
                        .map(|usage| AssetReplacementLogUse {
                            tile: usage.tile.clone(),
                            source_file: usage.source_file.display(),
                        })
                        .collect(),
                    backups: report.backups.clone(),
                };
                match asset_replacement_log_file()
                    .ok_or_else(|| {
                        "Windows APPDATA не определена; журнал замен не сохранён.".to_owned()
                    })
                    .and_then(|path| append_asset_replacement_log(&path, &mut entry).map(|()| path))
                {
                    Ok(path) => report.log_file = Some(path),
                    Err(error) => report.log_error = Some(error),
                }
                report
            });
            let _ = tx.send((missing_path, result));
        });
    }

    pub(crate) fn asset_only(path: PathBuf) -> Self {
        let mut app = Self {
            startup_scan_started: true,
            asset_editor_only: true,
            ..Self::default()
        };
        app.start_asset_view(path);
        app
    }

    pub(crate) fn asset_only_error(error: String) -> Self {
        Self {
            startup_scan_started: true,
            asset_editor_only: true,
            asset_viewer_open: true,
            status: error,
            ..Self::default()
        }
    }

    fn start_asset_view(&mut self, path: PathBuf) {
        let serz_path = preferred_serz_path(Path::new(self.rw_path.trim()));
        let source_path = path.clone();
        let (tx, rx) = mpsc::channel();
        self.asset_viewer_load = Some(rx);
        self.asset_viewer_open = true;
        self.asset_viewer_document = None;
        self.status = format!("Конвертация для редактирования: {}...", path.display());
        thread::spawn(move || {
            let result = load_asset_as_xml(&serz_path, &source_path);
            let _ = tx.send(result);
        });
    }

    fn start_asset_save(&mut self) {
        let Some(document) = self.asset_viewer_document.as_mut() else {
            return;
        };
        if !document.is_dirty() || self.asset_viewer_save.is_some() {
            return;
        }
        if !document.validate_xml() {
            self.status = document
                .xml_validation
                .as_ref()
                .and_then(|result| result.as_ref().err())
                .cloned()
                .unwrap_or_else(|| "XML не прошёл проверку.".to_owned());
            return;
        }
        let source_path = document.source_path.clone();
        let source_xml = document.xml.clone();
        let serz_path = preferred_serz_path(Path::new(self.rw_path.trim()));
        let (tx, rx) = mpsc::channel();
        self.asset_viewer_save = Some(rx);
        self.status = format!("Сохранение {}...", source_path.display());
        thread::spawn(move || {
            let result = save_asset_xml(&serz_path, &source_path, &source_xml)
                .map(|backup| (source_path, source_xml, backup));
            let _ = tx.send(result);
        });
    }

    fn open_asset_view_dialog(&mut self) {
        if self
            .asset_viewer_document
            .as_ref()
            .is_some_and(EditorDocument::is_dirty)
        {
            self.asset_viewer_open = true;
            self.status =
                "Сначала сохраните изменения текущего файла, прежде чем открывать другой."
                    .to_owned();
            return;
        }
        let selected = rfd::FileDialog::new()
            .set_title("Открыть BIN или GeoPcDx")
            .add_filter("BIN / GeoPcDx", &["bin", "geopcdx"])
            .pick_file();
        let Some(path) = selected else {
            return;
        };
        self.start_asset_view(path);
    }

    fn show_asset_editor(&mut self, ui: &mut egui::Ui) {
        let save_shortcut = ui
            .ctx()
            .input(|input| input.modifiers.command && input.key_pressed(egui::Key::S));
        let find_shortcut = ui
            .ctx()
            .input(|input| input.modifiers.command && input.key_pressed(egui::Key::F));
        let saving = self.asset_viewer_save.is_some();
        let mut save_requested = false;

        if self.asset_editor_only {
            egui::Panel::bottom("editor_status").show(ui, |ui| {
                ui.label(&self.status);
            });
            egui::CentralPanel::default().show(ui, |ui| {
                ui.heading("Редактор BIN/GeoPcDx — XML");
                ui.separator();
                save_requested = render_asset_editor(
                    ui,
                    self.asset_viewer_document.as_mut(),
                    self.asset_viewer_load.is_some(),
                    saving,
                    save_shortcut,
                    find_shortcut,
                );
            });
        } else if self.asset_viewer_open {
            egui::Window::new("Редактор BIN/GeoPcDx — XML")
                .open(&mut self.asset_viewer_open)
                .default_size([1000.0, 760.0])
                .resizable(true)
                .show(ui.ctx(), |ui| {
                    save_requested = render_asset_editor(
                        ui,
                        self.asset_viewer_document.as_mut(),
                        self.asset_viewer_load.is_some(),
                        saving,
                        save_shortcut,
                        find_shortcut,
                    );
                });
        }
        if save_requested || (self.asset_editor_only && save_shortcut) {
            self.start_asset_save();
        }
    }

    pub(crate) fn start_route_scan(&mut self) {
        self.begin_route_scan(false, true);
    }

    fn start_full_route_scan(&mut self) {
        self.begin_route_scan(false, false);
    }

    fn start_full_route_scan_preserving_incomplete(&mut self) {
        self.begin_route_scan(true, false);
    }

    fn begin_route_scan(&mut self, preserve_incomplete: bool, allow_cache: bool) {
        let root = PathBuf::from(self.rw_path.trim());
        let (tx, rx) = mpsc::channel();
        self.route_scan = Some(rx);
        self.routes.clear();
        self.selected_route = None;
        if !preserve_incomplete {
            self.incomplete_folders.clear();
            self.selected_incomplete = None;
        }
        self.result = None;
        self.status = "Сканирование маршрутов...".to_owned();
        let ignored_route_ids = self.ignored_route_ids.clone();
        thread::spawn(move || {
            let result = scan_routes_with_cache(&root, &ignored_route_ids, allow_cache);
            let _ = tx.send(result);
        });
    }

    pub(crate) fn start_check(&mut self) {
        let Some(route) = self
            .selected_route
            .and_then(|index| self.routes.get(index))
            .cloned()
        else {
            return;
        };
        let rw_path = PathBuf::from(self.rw_path.trim());
        let assets_path = rw_path.join("Assets");
        let serz_path = preferred_serz_path(&rw_path);
        let ignore_scenarios = self.ignore_scenarios;
        let max_concurrent_serz = self.serz_process_limit;
        let (tx, rx) = mpsc::channel();
        self.check = Some(rx);
        self.result = None;
        self.missing_assets_popup = None;
        self.missing_assets_shown = false;
        if let Ok(mut state) = self.asset_inventory_state.lock() {
            state.open = false;
            state.assets.clear();
            state.umlaut_assets.clear();
            state.scanning_umlauts = false;
        }
        self.status = format!("Проверка маршрута «{}»...", route.name);
        thread::spawn(move || {
            let progress_tx = tx.clone();
            let result = check_route(
                &route.path,
                &assets_path,
                &serz_path,
                ignore_scenarios,
                max_concurrent_serz,
                |message| {
                    let _ = progress_tx.send(CheckMessage::Progress(message.to_owned()));
                },
            );
            let _ = tx.send(CheckMessage::Finished(result));
        });
    }

    fn start_delete(&mut self, folders: Vec<PathBuf>) {
        self.pending_delete = None;
        self.delete_paths = Some(folders.clone());
        self.delete_feedback = None;
        let routes_path = PathBuf::from(self.rw_path.trim())
            .join("Content")
            .join("Routes");
        let (tx, rx) = mpsc::channel();
        self.delete_rx = Some(rx);
        self.status = format!("Перемещение {} папок в Корзину Windows...", folders.len());
        thread::spawn(move || {
            let result = move_folders_to_recycle_bin(&routes_path, &folders).and_then(|count| {
                invalidate_route_scan_cache()
                    .map(|()| count)
                    .map_err(|error| {
                        format!("Папка перемещена, но не удалось сбросить кэш маршрутов: {error}")
                    })
            });
            let _ = tx.send(result);
        });
    }

    /// Заголовок окна выбранного сценария.
    fn scenario_title(&self) -> String {
        self.selected_scenario
            .and_then(|index| self.scenarios.get(index))
            .map(|scenario| scenario.name.clone())
            .unwrap_or_else(|| "Сценарий".to_owned())
    }

    /// Обновляет список сценариев выбранного маршрута.
    ///
    /// Сценарии читаются с диска, поэтому список обновляется только при смене
    /// маршрута: файл сценариев небольшой, но читать его каждый кадр незачем.
    fn refresh_scenarios(&mut self) {
        let Some(route) = self.selected_route.and_then(|index| self.routes.get(index)) else {
            self.scenarios.clear();
            self.scenarios_route = None;
            self.selected_scenario = None;
            return;
        };
        if self.scenarios_route.as_deref() == Some(route.id.as_str()) {
            return;
        }
        self.scenarios = read_route_scenarios(
            &route.path,
            &preferred_serz_path(Path::new(self.rw_path.trim())),
        );
        self.scenarios_route = Some(route.id.clone());
        self.selected_scenario = None;
        self.selected_consist = None;
        self.scenario_consist_info = None;
        self.scenario_vehicle_files = None;
    }

    /// Все ли файлы техники выбранного состава разрешаются в реальные файлы на диске.
    fn consist_files_exist(
        &self,
        consist: &ScenarioConsist,
        route_path: Option<&Path>,
        rw_path: &Path,
    ) -> bool {
        let Some(route_path) = route_path else {
            return false;
        };
        consist
            .vehicles
            .iter()
            .all(|vehicle| {
                vehicle.blueprint.is_empty()
                    || self
                        .resolve_blueprint_file_cached(
                            rw_path,
                            route_path,
                            &vehicle.blueprint,
                            &vehicle.provider,
                            &vehicle.product,
                        )
                        .is_some()
            })
    }

    /// Разрешает путь блюпринта с кэшированием результата.
    fn resolve_blueprint_file_cached(
        &self,
        rw_path: &Path,
        route_path: &Path,
        blueprint: &str,
        provider: &str,
        product: &str,
    ) -> Option<PathBuf> {
        crate::blueprint::resolve_blueprint_file_cached_free(
            rw_path, route_path, blueprint, provider, product,
        )
    }

    pub(crate) fn is_checking(&self) -> bool {
        self.check.is_some()
    }

    pub(crate) fn receive_background_results(&mut self) {
        loop {
            match self.inventory_action_rx.try_recv() {
                Ok(InventoryAction::ScanUmlauts) => self.start_inventory_scan(),
                Ok(InventoryAction::Repair { asset, fixes }) => {
                    self.character_fixes = fixes.clone();
                    self.start_umlaut_repair(asset, fixes);
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }

        let inventory_scan_result =
            self.inventory_scan
                .as_ref()
                .and_then(|rx| match rx.try_recv() {
                    Ok(result) => Some(result),
                    Err(TryRecvError::Disconnected) => Some(Err(
                        "Не удалось получить результат проверки умляутов.".to_owned(),
                    )),
                    Err(TryRecvError::Empty) => None,
                });
        if let Some(result) = inventory_scan_result {
            self.inventory_scan = None;
            match result {
                Ok((assets, errors)) => {
                    if let Some(check_result) = self.result.as_mut() {
                        check_result.umlaut_assets = assets.clone();
                        for error in &errors {
                            if !check_result.conversion_errors.contains(error) {
                                check_result.conversion_errors.push(error.clone());
                            }
                        }
                        check_result.conversion_errors.sort();
                    }
                    let window_error = match self.asset_inventory_state.lock() {
                        Ok(mut state) => {
                            state.umlaut_assets = assets.clone();
                            state.scanning_umlauts = false;
                            state.show_umlauts = true;
                            state.error = if errors.is_empty() {
                                None
                            } else {
                                Some(errors.join("\n"))
                            };
                            None
                        }
                        Err(error) => Some(error.to_string()),
                    };
                    self.status = if errors.is_empty() {
                        format!("Проверка умляутов завершена. Найдено: {}.", assets.len())
                    } else {
                        format!(
                            "Проверка завершена: найдено ассетов {}, ошибок анализа {}.",
                            assets.len(),
                            errors.len()
                        )
                    };
                    if let Some(error) = window_error {
                        self.status.push_str(&format!(
                            " Не удалось обновить состояние окна инвентаря: {error}"
                        ));
                    }
                }
                Err(error) => {
                    if let Ok(mut state) = self.asset_inventory_state.lock() {
                        state.scanning_umlauts = false;
                        state.error = Some(error.clone());
                    }
                    self.status = error;
                }
            }
        }

        let asset_view_result =
            self.asset_viewer_load
                .as_ref()
                .and_then(|rx| match rx.try_recv() {
                    Ok(result) => Some(result),
                    Err(TryRecvError::Disconnected) => Some(Err(
                        "Не удалось получить результат конвертации файла для просмотра.".to_owned(),
                    )),
                    Err(TryRecvError::Empty) => None,
                });
        if let Some(result) = asset_view_result {
            self.asset_viewer_load = None;
            match result {
                Ok(document) => {
                    self.status = format!("Открыт для редактирования: {}", document.source);
                    self.asset_viewer_document = Some(document);
                }
                Err(error) => self.status = error,
            }
        }

        let asset_save_result =
            self.asset_viewer_save
                .as_ref()
                .and_then(|rx| match rx.try_recv() {
                    Ok(result) => Some(result),
                    Err(TryRecvError::Disconnected) => Some(Err(
                        "Не удалось получить результат сохранения BIN/GeoPcDx.".to_owned(),
                    )),
                    Err(TryRecvError::Empty) => None,
                });
        if let Some(result) = asset_save_result {
            self.asset_viewer_save = None;
            match result {
                Ok((source_path, saved_xml, backup_path)) => {
                    if let Some(document) = self
                        .asset_viewer_document
                        .as_mut()
                        .filter(|document| document.source_path == source_path)
                    {
                        document.saved_xml = saved_xml;
                    }
                    let mut entry = AssetReplacementLogEntry {
                        timestamp_unix_seconds: 0,
                        operation: "Сохранение изменений BIN/GeoPcDx".to_owned(),
                        route_id: None,
                        route_name: None,
                        route_path: None,
                        original_files: vec![source_path.display().to_string()],
                        missing_asset: None,
                        replacement_asset: None,
                        replaced_files: 1,
                        replaced_instances: 0,
                        uses: Vec::new(),
                        backups: backup_path.iter().cloned().collect(),
                    };
                    let log_result = asset_replacement_log_file()
                        .ok_or_else(|| {
                            "Windows APPDATA не определена; журнал замен не сохранён.".to_owned()
                        })
                        .and_then(|path| {
                            append_asset_replacement_log(&path, &mut entry).map(|()| path)
                        });
                    self.status = backup_path.map_or_else(
                        || format!("Сохранено: {}", source_path.display()),
                        |backup| {
                            format!(
                                "Сохранено: {}. Исходник сохранён в {}.",
                                source_path.display(),
                                backup.display()
                            )
                        },
                    );
                    match log_result {
                        Ok(path) => {
                            self.status
                                .push_str(&format!(" Журнал: {}", path.display()));
                        }
                        Err(error) => {
                            self.status
                                .push_str(&format!(" Журнал не записан: {error}"));
                        }
                    }
                }
                Err(error) => self.status = error,
            }
        }

        let asset_replacement_result =
            self.asset_replacement
                .as_ref()
                .and_then(|rx| match rx.try_recv() {
                    Ok(result) => Some(result),
                    Err(TryRecvError::Disconnected) => Some((
                        String::new(),
                        Err("Не удалось получить результат замены ассетов.".to_owned()),
                    )),
                    Err(TryRecvError::Empty) => None,
                });
        if let Some((missing_path, result)) = asset_replacement_result {
            self.asset_replacement = None;
            match result {
                Ok(report) => {
                    let log_message = report.log_file.as_ref().map_or_else(
                        || {
                            report.log_error.as_ref().map_or_else(
                                || "Журнал замен недоступен.".to_owned(),
                                |error| format!("ЖУРНАЛ НЕ ЗАПИСАН: {error}"),
                            )
                        },
                        |path| format!("Журнал: {}", path.display()),
                    );
                    let remaining_assets = self.result.as_mut().map(|check_result| {
                        check_result
                            .missing_assets
                            .retain(|asset| asset.path != missing_path);
                        (
                            check_result.missing_assets.len(),
                            check_result.umlaut_assets.len(),
                        )
                    });
                    if let Some(check_result) = self.result.as_ref() {
                        let paths: Vec<String> =
                            check_result.missing_assets.iter().map(|a| a.path.clone()).collect();
                        if paths.is_empty() {
                            self.missing_assets_popup = None;
                            self.missing_assets_shown = false;
                        } else {
                            self.missing_assets_popup = Some(paths);
                        }
                    }
                    self.missing_assets_shown = false;
                    self.asset_replacement_feedback = Some(Ok(format!(
                        "Заменено размещений: {}; изменено файлов тайлов: {}. {log_message}",
                        report.replaced_instances, report.replaced_files,
                    )));
                    if !report.backups.is_empty() {
                        self.status = format!("Создано резервных копий: {}.", report.backups.len());
                    }
                    if remaining_assets == Some((0, 0)) {
                        self.status = "Все отсутствующие ассеты обработаны. Выполняется итоговая проверка маршрута...".to_owned();
                        self.start_check();
                    } else if let Some((remaining, umlauts)) = remaining_assets {
                        self.status = format!(
                            "Замена завершена. Осталось отсутствующих ассетов: {remaining}; ассетов с умляутами: {umlauts}."
                        );
                    }
                }
                Err(error) => {
                    self.status = error.clone();
                    self.asset_replacement_feedback = Some(Err(error));
                }
            }
        }

        let umlaut_repair_result = self
            .umlaut_repair
            .as_ref()
            .and_then(|rx| match rx.try_recv() {
                Ok(result) => Some(result),
                Err(TryRecvError::Disconnected) => Some((
                    String::new(),
                    Err("Не удалось получить результат транслитерации ассетов.".to_owned()),
                )),
                Err(TryRecvError::Empty) => None,
            });
        if let Some((asset_path, result)) = umlaut_repair_result {
            self.umlaut_repair = None;
            match result {
                Ok(report) => {
                    let log_message = report.log_file.as_ref().map_or_else(
                        || {
                            report.log_error.as_ref().map_or_else(
                                || "Журнал замен недоступен.".to_owned(),
                                |error| format!("ЖУРНАЛ НЕ ЗАПИСАН: {error}"),
                            )
                        },
                        |path| format!("Журнал: {}", path.display()),
                    );
                    let remaining = self.result.as_mut().map(|check_result| {
                        check_result
                            .umlaut_assets
                            .retain(|asset| asset.path != asset_path);
                        check_result
                            .missing_assets
                            .retain(|asset| asset.path != asset_path);
                        if let Some(inventory_asset) = check_result
                            .asset_inventory
                            .iter_mut()
                            .find(|asset| asset.path == asset_path)
                        {
                            inventory_asset.path = report.new_asset_path.clone();
                            inventory_asset.missing = false;
                        }
                        (
                            check_result.missing_assets.len(),
                            check_result.umlaut_assets.len(),
                        )
                    });
                    if let Ok(mut state) = self.asset_inventory_state.lock() {
                        state.umlaut_assets.retain(|asset| asset.path != asset_path);
                        if let Some(inventory_asset) = state
                            .assets
                            .iter_mut()
                            .find(|asset| asset.path == asset_path)
                        {
                            inventory_asset.path = report.new_asset_path.clone();
                            inventory_asset.missing = false;
                        }
                    }
                    self.asset_replacement_feedback = Some(Ok(format!(
                        "Транслитерировано файлов: {}; обновлено ссылок: {}. {log_message}",
                        report.changed_files, report.changed_references
                    )));
                    if remaining == Some((0, 0)) {
                        self.status = "Все найденные проблемы обработаны. Выполняется итоговая проверка маршрута...".to_owned();
                        self.start_check();
                    } else if let Some((missing, umlauts)) = remaining {
                        self.status = format!(
                            "Транслитерация завершена. Осталось отсутствующих ассетов: {missing}; ассетов с умляутами: {umlauts}."
                        );
                    }
                }
                Err(error) => {
                    self.status = error.clone();
                    self.asset_replacement_feedback = Some(Err(error));
                }
            }
        }

        let delete_result = self.delete_rx.as_ref().and_then(|rx| match rx.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Disconnected) => Some(Err(
                "Не удалось получить результат удаления папок.".to_owned(),
            )),
            Err(TryRecvError::Empty) => None,
        });
        if let Some(result) = delete_result {
            self.delete_rx = None;
            match result {
                Ok(count) => {
                    self.delete_feedback = Some(Ok(count));
                    let selected_path = self
                        .selected_incomplete
                        .and_then(|index| self.incomplete_folders.get(index))
                        .map(|folder| folder.path.clone());
                    if let Some(deleted_paths) = self.delete_paths.take() {
                        self.incomplete_folders
                            .retain(|folder| !deleted_paths.contains(&folder.path));
                    }
                    self.selected_incomplete = selected_path
                        .and_then(|path| {
                            self.incomplete_folders
                                .iter()
                                .position(|folder| folder.path == path)
                        })
                        .or_else(|| (!self.incomplete_folders.is_empty()).then_some(0));
                    self.status = format!("Перемещено в Корзину Windows: {count} папок.");
                    self.start_full_route_scan_preserving_incomplete();
                }
                Err(error) => {
                    self.delete_feedback = Some(Err(error.clone()));
                    self.delete_paths = None;
                    self.status = error;
                    self.start_full_route_scan_preserving_incomplete();
                }
            }
        }

        let route_result = self.route_scan.as_ref().and_then(|rx| match rx.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Disconnected) => Some(Err(
                "Не удалось получить результат сканирования маршрутов.".to_owned(),
            )),
            Err(TryRecvError::Empty) => None,
        });
        if let Some(result) = route_result {
            self.route_scan = None;
            match result {
                Ok(outcome) => {
                    let routes = outcome.result;
                    let source = if outcome.from_cache {
                        " (загружено из кэша)"
                    } else {
                        " (полное сканирование)"
                    };
                    self.status = format!(
                        "Найдено маршрутов: {}; неполных папок: {}.",
                        routes.routes.len(),
                        routes.incomplete_folders.len()
                    );
                    self.status.push_str(source);
                    if let Some(notice) = outcome.notice {
                        self.status.push(' ');
                        self.status.push_str(&notice);
                    }
                    self.routes = routes.routes;
                    self.incomplete_folders = routes.incomplete_folders;
                    self.selected_incomplete = (!self.incomplete_folders.is_empty()).then_some(0);
                    self.selected_route = (!self.routes.is_empty()).then_some(0);
                }
                Err(error) => self.status = error,
            }
        }

let check_message = self.check.as_ref().and_then(|rx| match rx.try_recv() {
            Ok(message) => Some(Ok(message)),
            Err(TryRecvError::Disconnected) => Some(Err(
                "Не удалось получить результат проверки маршрута.".to_owned(),
            )),
            Err(TryRecvError::Empty) => None,
        });
        if let Some(message) = check_message {
            match message {
                Ok(CheckMessage::Progress(status)) => self.status = status,
                Ok(CheckMessage::Finished(result)) => {
                    self.check = None;
                    match result {
                        Ok(result) => {
                            let mut summary = Vec::new();
                            if !result.missing_assets.is_empty() {
                                summary.push(format!(
                                    "отсутствует ассетов: {}",
                                    result.missing_assets.len()
                                ));
                                let paths: Vec<String> = result.missing_assets
                                    .iter()
                                    .map(|a| a.path.clone())
                                    .collect();
                                self.missing_assets_popup = Some(paths);
                            }
                            if !result.conversion_errors.is_empty() {
                                summary.push(format!(
                                    "ошибок Serz/XML: {}",
                                    result.conversion_errors.len()
                                ));
                            }
                            if !result.structure_issues.is_empty() {
                                summary.push(format!(
                                    "проблем структуры: {}",
                                    result.structure_issues.len()
                                ));
                            }
                            self.status = if summary.is_empty() {
                                "Проверка завершена: ошибки не найдены.".to_owned()
                            } else {
                                format!("Проверка завершена: {}.", summary.join(", "))
                            };
                            self.result = Some(result);
                        }
                        Err(error) => self.status = error,
                    }
                }
                Err(error) => {
                    self.check = None;
                    self.status = error;
                }
            }
        }
    }
}

fn render_asset_editor(
    ui: &mut egui::Ui,
    document: Option<&mut EditorDocument>,
    loading: bool,
    saving: bool,
    save_shortcut: bool,
    find_shortcut: bool,
) -> bool {
    let mut save_requested = false;
    if let Some(document) = document {
        ui.horizontal(|ui| {
            ui.label(&document.source);
            if document.is_dirty() {
                ui.colored_label(egui::Color32::YELLOW, "Есть несохранённые изменения");
            }
        });
        ui.horizontal(|ui| {
            if ui.button("Проверить целостность XML").clicked() {
                document.validate_xml();
            }
            match &document.xml_validation {
                Some(Ok(())) => {
                    ui.colored_label(egui::Color32::LIGHT_GREEN, "XML корректен");
                }
                Some(Err(error)) => {
                    ui.colored_label(egui::Color32::LIGHT_RED, error);
                }
                None => {
                    ui.label("XML ещё не проверен");
                }
            }
        });
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    document.is_dirty() && !saving,
                    egui::Button::new("Сохранить (Ctrl+S)"),
                )
                .clicked()
            {
                save_requested = document.validate_xml();
            }
            if saving {
                ui.spinner();
                ui.label("Сохранение...");
            }
        });
        if save_shortcut {
            save_requested = document.validate_xml();
        }
        ui.separator();
        ui.horizontal(|ui| {
            ui.label("Найти:");
            let find_response = ui.add(
                egui::TextEdit::singleline(&mut document.find_query)
                    .id(ui.make_persistent_id("editor_find"))
                    .desired_width(160.0),
            );
            if find_shortcut {
                find_response.request_focus();
            }
            if let Err(error) = document.refresh_matches() {
                ui.colored_label(egui::Color32::LIGHT_RED, error);
            }
            if ui.button("Предыдущее").clicked() {
                document.move_match(false);
            }
            if ui.button("Следующее").clicked() {
                document.move_match(true);
            }
            let match_status = if document.match_count() == 0 {
                "Совпадений: 0".to_owned()
            } else if !document.match_selected {
                format!("Совпадений: {}", document.match_count())
            } else {
                format!(
                    "Совпадение {} из {}",
                    document.match_index + 1,
                    document.match_count()
                )
            };
            ui.label(match_status);
            ui.checkbox(&mut document.case_sensitive, "Учитывать регистр");
        });
        ui.horizontal(|ui| {
            ui.label("Заменить на:");
            ui.add(
                egui::TextEdit::singleline(&mut document.replace_text)
                    .id(ui.make_persistent_id("editor_replace"))
                    .desired_width(160.0),
            );
            if ui
                .add_enabled(document.match_count() > 0, egui::Button::new("Заменить"))
                .clicked()
            {
                document.replace_current();
            }
            if ui
                .add_enabled(
                    document.match_count() > 0,
                    egui::Button::new("Заменить всё"),
                )
                .clicked()
            {
                document.replace_all();
            }
        });
        ui.separator();
        let editor_id = ui.make_persistent_id("asset_xml_editor");
        let cursor_char = egui::text_edit::TextEditState::load(ui.ctx(), editor_id)
            .and_then(|state| state.cursor.char_range())
            .map_or(0, |range| range.primary.index.into());
        let matching_tags = document.matching_tag_ranges(cursor_char);
        let syntax = document.syntax().clone();
        let xml = &mut document.xml;
        let mut layouter = |ui: &egui::Ui, text: &dyn egui::TextBuffer, wrap_width: f32| {
            let job = xml_layout_job(
                ui,
                text.as_str(),
                wrap_width,
                &syntax,
                matching_tags.as_ref(),
            );
            ui.painter().layout_job(job)
        };
        let pending_selection = document.pending_selection.take();
        let mut text_changed = false;
        egui::ScrollArea::both()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let output = egui::TextEdit::multiline(xml)
                    .font(egui::TextStyle::Monospace)
                    .code_editor()
                    .desired_width(f32::INFINITY)
                    .desired_rows(30)
                    .id_salt("asset_xml_editor")
                    .layouter(&mut layouter)
                    .show(ui);
                text_changed = output.response.changed();
                if let Some((start, end)) = pending_selection {
                    let mut state = output.state;
                    state
                        .cursor
                        .set_char_range(Some(egui::text::CCursorRange::two(
                            egui::text::CCursor::new(start),
                            egui::text::CCursor::new(end),
                        )));
                    state.store(ui.ctx(), output.response.id);
                    output.response.request_focus();
                }
            });
        if text_changed {
            document.note_text_edited();
            if let Err(error) = document.refresh_matches() {
                ui.colored_label(egui::Color32::LIGHT_RED, error);
            }
        }
    } else if loading {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label("Конвертация BIN/GeoPcDx в XML...");
        });
    } else {
        ui.label("Файл не загружен.");
    }
    save_requested
}

