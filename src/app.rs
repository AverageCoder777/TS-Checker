//! egui application state, windows, and user interactions.

use super::*;
use std::collections::HashSet;

enum CheckMessage {
    Progress(String),
    Finished(Result<CheckResult, String>),
}

pub(crate) enum InventoryAction {
    ScanUmlauts,
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
    pub(crate) sort: InventorySort,
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
    pub(crate) scenarios: Vec<RouteScenario>,
    /// Маршрут, для которого прочитаны сценарии.
    scenarios_route: Option<String>,
    /// Выбранный сценарий в списке.
    pub(crate) selected_scenario: Option<usize>,
    /// Выбранный состав внутри открытого окна сценария.
    pub(crate) selected_consist: Option<usize>,
    /// Открыто ли окно выбранного сценария.
    pub(crate) scenario_window_open: bool,
    /// Кэш разрешённых путей блюпринтов техники, чтобы не обходить Assets
    /// повторно при каждом кадре отрисовки окна сценария.
    /// Результат проверки наличия файлов для составов открытого сценария,
    /// считается один раз, чтобы не пересчитывать при каждом кадре egui.
    pub(crate) scenario_consist_info: Option<Vec<(String, bool)>>,
    /// Кэш наличия файлов для техники: [состав][вагон] = файл_на_месте.
    pub(crate) scenario_vehicle_files: Option<Vec<Vec<bool>>>,
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
    /// Замены символов, предложенные пользователем для сломанных имён.
    character_fixes: CharacterFixes,
    pending_delete: Option<Vec<PathBuf>>,
    delete_paths: Option<Vec<PathBuf>>,
    delete_rx: Option<Receiver<Result<usize, String>>>,
    delete_feedback: Option<Result<usize, String>>,
    settings_window_open: bool,
    pub(crate) asset_viewer_open: bool,
    asset_editor_only: bool,
    pub(crate) asset_viewer_document: Option<EditorDocument>,
    pub(crate) editor_sync_pending: bool,
    pub(crate) asset_viewer_load: Option<Receiver<Result<EditorDocument, String>>>,
    pub(crate) asset_viewer_save: Option<Receiver<Result<SavedAsset, String>>>,
    pub(crate) route_thumbnails_enabled: bool,
    pub(crate) serz_process_limit: usize,
    pub(crate) locoswap_path: String,
    attempted_route_icons: HashSet<PathBuf>,
    pub(crate) asset_inventory_state: Arc<Mutex<AssetInventoryWindowState>>,
    pub(crate) inventory_action_tx: Sender<InventoryAction>,
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
            character_fixes: CharacterFixes::new(),
            pending_delete: None,
            delete_paths: None,
            delete_rx: None,
            delete_feedback: None,
            settings_window_open: false,
            asset_viewer_open: false,
            asset_editor_only: false,
            asset_viewer_document: None,
            editor_sync_pending: false,
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

    pub(crate) fn start_asset_view(&mut self, path: PathBuf) {
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

    pub(crate) fn start_asset_save(&mut self) {
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

    pub(crate) fn start_route_scan(&mut self) {
        self.begin_route_scan(false, true);
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

/// Обновляет список сценариев выбранного маршрута.
    ///
    /// Сценарии читаются с диска, поэтому список обновляется только при смене
    /// маршрута: файл сценариев небольшой, но читать его каждый кадр незачем.
    pub(crate) fn refresh_scenarios(&mut self) {
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

    pub(crate) fn is_checking(&self) -> bool {
        self.check.is_some()
    }

    pub(crate) fn receive_background_results(&mut self) {
        loop {
            match self.inventory_action_rx.try_recv() {
                Ok(InventoryAction::ScanUmlauts) => self.start_inventory_scan(),
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
                    self.editor_sync_pending = true;
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
