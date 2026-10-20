//! egui application state, windows, and user interactions.

use super::*;

enum CheckMessage {
    Progress(String),
    Finished(Result<CheckResult, String>),
}

enum InventoryAction {
    ScanUmlauts,
    Repair(UmlautAsset),
}

#[derive(Default)]
struct AssetInventoryWindowState {
    open: bool,
    search: String,
    show_umlauts: bool,
    show_tiles: bool,
    scanning_umlauts: bool,
    error: Option<String>,
    assets: Vec<ReferencedAsset>,
    umlaut_assets: Vec<UmlautAsset>,
}

pub(crate) fn decode_route_icon(path: &Path) -> image::ImageResult<image::DynamicImage> {
    let reader = image::ImageReader::open(path).map_err(image::ImageError::IoError)?;
    let reader = reader
        .with_guessed_format()
        .map_err(image::ImageError::IoError)?;
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

pub(crate) struct CheckerApp {
    rw_path: String,
    startup_scan_started: bool,
    routes: Vec<Route>,
    selected_route: Option<usize>,
    search_query: String,
    ignore_scenarios: bool,
    result: Option<CheckResult>,
    status: String,
    incomplete_folders: Vec<IncompleteRouteFolder>,
    incomplete_window_open: bool,
    selected_incomplete: Option<usize>,
    ignored_route_ids: HashSet<String>,
    ignore_list_window_open: bool,
    route_scan: Option<Receiver<Result<RouteScanOutcome, String>>>,
    check: Option<Receiver<CheckMessage>>,
    asset_replacement: Option<Receiver<(String, Result<AssetReplacementReport, String>)>>,
    umlaut_repair: Option<Receiver<(String, Result<UmlautRepairReport, String>)>>,
    asset_replacement_feedback: Option<Result<String, String>>,
    pending_umlaut_repair: Option<UmlautAsset>,
    pending_delete: Option<Vec<PathBuf>>,
    delete_paths: Option<Vec<PathBuf>>,
    delete_rx: Option<Receiver<Result<usize, String>>>,
    delete_feedback: Option<Result<usize, String>>,
    settings_window_open: bool,
    asset_viewer_open: bool,
    asset_editor_only: bool,
    asset_viewer_document: Option<EditorDocument>,
    asset_viewer_load: Option<Receiver<Result<EditorDocument, String>>>,
    asset_viewer_save: Option<Receiver<Result<(PathBuf, String, Option<PathBuf>), String>>>,
    route_thumbnails_enabled: bool,
    serz_process_limit: usize,
    route_icon_textures: HashMap<PathBuf, egui::TextureHandle>,
    attempted_route_icons: HashSet<PathBuf>,
    asset_inventory_state: Arc<Mutex<AssetInventoryWindowState>>,
    inventory_action_tx: Sender<InventoryAction>,
    inventory_action_rx: Receiver<InventoryAction>,
    inventory_scan: Option<Receiver<Result<(Vec<UmlautAsset>, Vec<String>), String>>>,
}

impl Default for CheckerApp {
    fn default() -> Self {
        let rw_path = load_last_game_path();
        let (ignored_route_ids, mut status) = match load_ignored_route_ids() {
            Ok(ids) => (
                ids,
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
            status.push_str(" ");
            status.push_str(&warning);
        }
        if let Some(warning) = serz_settings_warning {
            status.push_str(" ");
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
            route_icon_textures: HashMap::new(),
            attempted_route_icons: HashSet::new(),
            asset_inventory_state: Arc::new(Mutex::new(AssetInventoryWindowState::default())),
            inventory_action_tx,
            inventory_action_rx,
            inventory_scan: None,
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
        if !is_open {
            return;
        }

        let state = Arc::clone(&self.asset_inventory_state);
        let action_tx = self.inventory_action_tx.clone();
        let assets_root = PathBuf::from(self.rw_path.trim()).join("Assets");
        let repair_busy = self.check.is_some()
            || self.asset_replacement.is_some()
            || self.inventory_scan.is_some()
            || self.umlaut_repair.is_some()
            || self.pending_umlaut_repair.is_some()
            || self.selected_route.is_none();
        let scan_busy = repair_busy || self.inventory_scan.is_some();
        ctx.show_viewport_deferred(
            egui::ViewportId::from_hash_of("asset_inventory"),
            egui::ViewportBuilder::default()
                .with_title("Инвентарь ассетов маршрута")
                .with_inner_size([900.0, 700.0]),
            move |ctx, _class| {
                let mut state = match state.lock() {
                    Ok(state) => state,
                    Err(error) => {
                        eprintln!("Не удалось получить состояние окна инвентаря: {error}");
                        return;
                    }
                };
                if ctx.input(|input| input.viewport().close_requested()) {
                    state.open = false;
                    return;
                }

                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.heading("Все используемые ассеты маршрута");
                    ui.horizontal(|ui| {
                        ui.label(format!("Всего: {}", state.assets.len()));
                        if ui
                            .add_enabled(
                                !state.scanning_umlauts && !scan_busy,
                                egui::Button::new("Проверить все ассеты на умляуты"),
                            )
                            .clicked()
                        {
                            state.scanning_umlauts = true;
                            state.show_umlauts = true;
                            state.error = None;
                            if let Err(error) = action_tx.send(InventoryAction::ScanUmlauts) {
                                state.scanning_umlauts = false;
                                state.error =
                                    Some(format!("Не удалось запустить проверку: {error}"));
                            } else {
                                ctx.request_repaint();
                            }
                        }
                        ui.selectable_value(&mut state.show_umlauts, false, "Все ассеты");
                        ui.selectable_value(&mut state.show_umlauts, true, "Требуют проверки");
                        if ui
                            .button(if state.show_tiles {
                                "Скрыть тайлы"
                            } else {
                                "Показать тайлы"
                            })
                            .clicked()
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
                        ui.label(format!(
                            "Найдено для проверки: {}",
                            state.umlaut_assets.len()
                        ));
                    }
                    if let Some(error) = &state.error {
                        ui.colored_label(egui::Color32::LIGHT_RED, error);
                    }
                    ui.add(
                        egui::TextEdit::singleline(&mut state.search)
                            .hint_text("Поиск по пути, тайлу или исходному файлу"),
                    );
                    ui.separator();

                    let search = state.search.to_lowercase();
                    let rows: Vec<(&str, &[AssetUse], bool, Option<&UmlautAsset>)> =
                        if state.show_umlauts {
                            state
                                .umlaut_assets
                                .iter()
                                .filter(|asset| {
                                    search.is_empty()
                                        || asset.path.to_lowercase().contains(&search)
                                        || asset.uses.iter().any(|usage| {
                                            usage.tile.to_lowercase().contains(&search)
                                                || usage
                                                    .source_file
                                                    .display()
                                                    .to_string()
                                                    .to_lowercase()
                                                    .contains(&search)
                                        })
                                })
                                .map(|asset| {
                                    let missing = state
                                        .assets
                                        .iter()
                                        .find(|item| item.path == asset.path)
                                        .is_some_and(|item| item.missing);
                                    (
                                        asset.path.as_str(),
                                        asset.uses.as_slice(),
                                        missing,
                                        Some(asset),
                                    )
                                })
                                .collect()
                        } else {
                            state
                                .assets
                                .iter()
                                .filter(|asset| {
                                    search.is_empty()
                                        || asset.path.to_lowercase().contains(&search)
                                        || asset.uses.iter().any(|usage| {
                                            usage.tile.to_lowercase().contains(&search)
                                                || usage
                                                    .source_file
                                                    .display()
                                                    .to_string()
                                                    .to_lowercase()
                                                    .contains(&search)
                                        })
                                })
                                .map(|asset| {
                                    (
                                        asset.path.as_str(),
                                        asset.uses.as_slice(),
                                        asset.missing,
                                        state
                                            .umlaut_assets
                                            .iter()
                                            .find(|item| item.path == asset.path),
                                    )
                                })
                                .collect()
                        };

                    let mut repair_requested = None;
                    egui::ScrollArea::vertical()
                        .id_salt("asset_inventory_rows")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            for (path, uses, missing, repair_asset) in &rows {
                                ui.group(|ui| {
                                    ui.horizontal(|ui| {
                                        let color = if *missing {
                                            egui::Color32::LIGHT_RED
                                        } else if repair_asset.is_some() {
                                            egui::Color32::YELLOW
                                        } else {
                                            ui.visuals().text_color()
                                        };
                                        ui.colored_label(color, *path);
                                        ui.small(
                                            assets_root
                                                .join(path.replace('/', "\\"))
                                                .display()
                                                .to_string(),
                                        );
                                        if *missing {
                                            ui.colored_label(
                                                egui::Color32::LIGHT_RED,
                                                "ОТСУТСТВУЕТ",
                                            );
                                        }
                                        if let Some(asset) = repair_asset.as_ref() {
                                            if ui
                                                .add_enabled(
                                                    !repair_busy,
                                                    egui::Button::new(
                                                        "Транслитерировать цепочку...",
                                                    ),
                                                )
                                                .clicked()
                                            {
                                                repair_requested = Some((*asset).clone());
                                            }
                                        }
                                    });
                                    ui.small(format!("Использований: {}", uses.len()));
                                    let mut by_tile: HashMap<&str, usize> = HashMap::new();
                                    for usage in *uses {
                                        by_tile
                                            .entry(&usage.tile)
                                            .and_modify(|count| *count += 1)
                                            .or_insert(1);
                                    }
                                    if state.show_tiles {
                                        let mut tiles: Vec<_> = by_tile.into_iter().collect();
                                        tiles.sort_by(|left, right| left.0.cmp(right.0));
                                        for (tile, count) in tiles {
                                            ui.label(format!("• {tile} — {count} объектов"));
                                        }
                                    } else {
                                        ui.small(format!(
                                            "Используется на {} тайлах",
                                            by_tile.len()
                                        ));
                                    }
                                    if let Some(asset) = repair_asset {
                                        for evidence in &asset.evidence {
                                            ui.small(format!("Обнаружение: {evidence}"));
                                        }
                                    }
                                });
                            }
                        });
                    drop(rows);
                    if let Some(asset) = repair_requested {
                        if let Err(error) = action_tx.send(InventoryAction::Repair(asset)) {
                            state.error =
                                Some(format!("Не удалось запустить транслитерацию: {error}"));
                        } else {
                            ctx.request_repaint();
                        }
                    }
                });
            },
        );
    }

    fn open_asset_inventory(&mut self) {
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
            let result =
                scan_umlaut_assets(&inventory, &assets_path, &serz_path, max_concurrent_serz);
            let _ = tx.send(result);
        });
    }

    fn start_umlaut_repair(&mut self, asset: UmlautAsset) {
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
            )
            .map(|mut report| {
                let mut uses: Vec<_> = asset
                    .uses
                    .iter()
                    .map(|usage| AssetReplacementLogUse {
                        tile: usage.tile.clone(),
                        source_file: usage.source_file.clone(),
                    })
                    .collect();
                uses.sort_by(|left, right| {
                    left.tile
                        .cmp(&right.tile)
                        .then_with(|| left.source_file.cmp(&right.source_file))
                });
                let mut entry = AssetReplacementLogEntry {
                    timestamp_unix_seconds: 0,
                    operation: "Транслитерация цепочки ассетов".to_owned(),
                    route_id: Some(route.id.clone()),
                    route_name: Some(route.name.clone()),
                    route_path: Some(route.path.clone()),
                    original_files: report.files.clone(),
                    missing_asset: Some(path.clone()),
                    replacement_asset: Some(report.new_asset_path.clone()),
                    replaced_files: report.changed_files,
                    replaced_instances: report.changed_references,
                    uses,
                    backups: report.backups.clone(),
                };
                match asset_replacement_log_file()
                    .ok_or_else(|| {
                        "Windows APPDATA не определена; журнал замен не сохранён.".to_owned()
                    })
                    .and_then(|log_path| {
                        append_asset_replacement_log(&log_path, &mut entry).map(|()| log_path)
                    }) {
                    Ok(log_path) => report.log_file = Some(log_path),
                    Err(error) => report.log_error = Some(error),
                }
                report
            });
            let _ = tx.send((path, result));
        });
    }

    fn start_asset_replacement(&mut self, missing: MissingAsset, replacement: PathBuf) {
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
        let (tx, rx) = mpsc::channel();
        self.asset_replacement = Some(rx);
        self.asset_replacement_feedback = None;
        self.status = format!("Подготовка замены ассета {}...", missing.path);
        thread::spawn(move || {
            let missing_path = missing.path.clone();
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
                    .map(|usage| usage.source_file.clone())
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
                            source_file: usage.source_file.clone(),
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
        let mut app = Self::default();
        app.startup_scan_started = true;
        app.asset_editor_only = true;
        app.start_asset_view(path);
        app
    }

    pub(crate) fn asset_only_error(error: String) -> Self {
        let mut app = Self::default();
        app.startup_scan_started = true;
        app.asset_editor_only = true;
        app.asset_viewer_open = true;
        app.status = error;
        app
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

    fn show_asset_editor(&mut self, ctx: &egui::Context) {
        let save_shortcut =
            ctx.input(|input| input.modifiers.command && input.key_pressed(egui::Key::S));
        let find_shortcut =
            ctx.input(|input| input.modifiers.command && input.key_pressed(egui::Key::F));
        let saving = self.asset_viewer_save.is_some();
        let mut save_requested = false;

        if self.asset_editor_only {
            egui::TopBottomPanel::bottom("editor_status").show(ctx, |ui| {
                ui.label(&self.status);
            });
            egui::CentralPanel::default().show(ctx, |ui| {
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
                .show(ctx, |ui| {
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

    fn route_icon_texture_id(
        &mut self,
        ctx: &egui::Context,
        icon_path: &Path,
    ) -> Option<egui::TextureId> {
        if !self.attempted_route_icons.contains(icon_path) {
            self.attempted_route_icons.insert(icon_path.to_path_buf());
            if let Ok(image) = decode_route_icon(icon_path) {
                let dimensions = image.dimensions();
                if dimensions.0 > 0
                    && dimensions.1 > 0
                    && dimensions.0 <= 4096
                    && dimensions.1 <= 4096
                {
                    let rgba = image.to_rgba8();
                    let color_image = egui::ColorImage::from_rgba_unmultiplied(
                        [dimensions.0 as usize, dimensions.1 as usize],
                        rgba.as_raw(),
                    );
                    let texture = ctx.load_texture(
                        icon_path.display().to_string(),
                        color_image,
                        egui::TextureOptions::LINEAR,
                    );
                    self.route_icon_textures
                        .insert(icon_path.to_path_buf(), texture);
                }
            }
        }
        self.route_icon_textures
            .get(icon_path)
            .map(egui::TextureHandle::id)
    }

    fn start_route_scan(&mut self) {
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

    fn start_check(&mut self) {
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

    fn receive_background_results(&mut self) {
        loop {
            match self.inventory_action_rx.try_recv() {
                Ok(InventoryAction::ScanUmlauts) => self.start_inventory_scan(),
                Ok(InventoryAction::Repair(asset)) => {
                    self.pending_umlaut_repair = Some(asset);
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
                        original_files: vec![source_path.clone()],
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
                        self.status.push_str(" ");
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
            let find_response = ui.text_edit_singleline(&mut document.find_query);
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
            ui.text_edit_singleline(&mut document.replace_text);
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
            .map_or(0, |range| range.primary.index);
        let matching_tags = document.matching_tag_ranges(cursor_char);
        let syntax = document.syntax().clone();
        let xml = &mut document.xml;
        let mut layouter = |ui: &egui::Ui, text: &str, wrap_width: f32| {
            let job = xml_layout_job(ui, text, wrap_width, &syntax, matching_tags.as_ref());
            ui.fonts(|fonts| fonts.layout_job(job))
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

impl eframe::App for CheckerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if !self.startup_scan_started {
            self.startup_scan_started = true;
            if is_valid_game_path(Path::new(self.rw_path.trim())) {
                self.start_route_scan();
            } else {
                self.status =
                    "Сохранённый путь к игре недействителен. Выберите папку RailWorks.".to_owned();
            }
        }
        self.receive_background_results();
        if self.route_scan.is_some()
            || self.check.is_some()
            || self.inventory_scan.is_some()
            || self.asset_replacement.is_some()
            || self.umlaut_repair.is_some()
            || self.pending_umlaut_repair.is_some()
            || self.delete_rx.is_some()
            || self.asset_viewer_load.is_some()
            || self.asset_viewer_save.is_some()
        {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        if self.asset_editor_only {
            self.show_asset_editor(ctx);
            return;
        }
        let route_icon_paths: Vec<_> = if self.route_thumbnails_enabled {
            self.routes
                .iter()
                .map(|route| route.icon_path.clone())
                .collect()
        } else {
            Vec::new()
        };
        let route_icon_texture_ids: Vec<_> = route_icon_paths
            .iter()
            .map(|icon_path| {
                icon_path
                    .as_deref()
                    .and_then(|path| self.route_icon_texture_id(ctx, path))
            })
            .collect();

        let mut delete_request = None;
        let mut delete_all_requested = false;
        let mut open_folder_request = None;
        let mut ignore_route_request = None;
        let mut remove_ignored_route = None;
        let mut replace_asset_request = None;
        let mut repair_umlaut_request = None;
        let mut open_inventory_request = false;
        egui::TopBottomPanel::bottom("status_bar").show(ctx, |ui| {
            ui.label(&self.status);
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("TS Checker");
            ui.label("Проверка маршрутов RailWorks и ссылок на ассеты");
            ui.add_space(8.0);

            ui.horizontal(|ui| {
                let busy = self.route_scan.is_some()
                    || self.check.is_some()
                    || self.inventory_scan.is_some()
                    || self.asset_replacement.is_some()
                    || self.umlaut_repair.is_some()
                    || self.pending_umlaut_repair.is_some()
                    || self.delete_rx.is_some();
                ui.label("Папка RailWorks:");
                ui.add(
                    egui::Label::new(&self.rw_path)
                        .truncate()
                        .wrap_mode(egui::TextWrapMode::Truncate),
                );
                if ui
                    .add_enabled(!busy, egui::Button::new("Выбрать папку..."))
                    .clicked()
                {
                    let current_path = PathBuf::from(self.rw_path.trim());
                    let mut dialog = rfd::FileDialog::new().set_title("Выберите папку RailWorks");
                    if current_path.is_dir() {
                        dialog = dialog.set_directory(current_path);
                    }
                    if let Some(path) = dialog.pick_folder() {
                        self.rw_path = path.to_string_lossy().into_owned();
                        self.routes.clear();
                        self.incomplete_folders.clear();
                        self.selected_route = None;
                        self.result = None;
                        self.selected_incomplete = None;
                        self.status = match save_last_game_path(&self.rw_path) {
                            Ok(()) => "Путь сохранён. Обновите список маршрутов.".to_owned(),
                            Err(error) => {
                                format!("Путь выбран, но не удалось сохранить его: {error}")
                            }
                        };
                    }
                }
                ui.add_enabled_ui(!busy, |ui| {
                    if ui.button("Обновить список").clicked() {
                        self.start_full_route_scan();
                    }
                });
                if ui.button("Настройки").clicked() {
                    self.settings_window_open = true;
                }
                if ui
                    .add_enabled(
                        self.asset_viewer_load.is_none() && self.asset_viewer_save.is_none(),
                        egui::Button::new("Редактор BIN/GeoPcDx..."),
                    )
                    .clicked()
                {
                    self.open_asset_view_dialog();
                }
            });

            ui.separator();
            let content_size = ui.available_size();
            let font_id = egui::TextStyle::Button.resolve(ui.style());
            let longest_route_label_width = ui.fonts(|fonts| {
                self.routes
                    .iter()
                    .map(|route| {
                        let label = format!("{}  ({})", route.name, route.id);
                        fonts
                            .layout_no_wrap(label, font_id.clone(), egui::Color32::WHITE)
                            .size()
                            .x
                    })
                    .fold(0.0_f32, f32::max)
            });
            let has_route_thumbnails =
                self.route_thumbnails_enabled && route_icon_texture_ids.iter().any(Option::is_some);
            let route_content_width = longest_route_label_width
                + if has_route_thumbnails {
                    256.0 + ui.spacing().item_spacing.x
                } else {
                    0.0
                }
                + 32.0;
            let (routes_width, check_width) = route_panel_widths(
                content_size.x,
                ui.spacing().item_spacing.x,
                route_content_width,
            );
            ui.horizontal_top(|ui| {
                ui.allocate_ui_with_layout(
                    egui::vec2(routes_width, content_size.y),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.set_width(routes_width);
                        ui.group(|ui| {
                            ui.heading("Маршруты");
                            ui.horizontal(|ui| {
                                ui.label(format!(
                                    "В игнор-листе: {}",
                                    self.ignored_route_ids.len()
                                ));
                                if ui.button("Редактировать игнор-лист...").clicked()
                                {
                                    self.ignore_list_window_open = true;
                                }
                            });
                            if !self.incomplete_folders.is_empty() {
                                ui.colored_label(
                                    egui::Color32::YELLOW,
                                    format!(
                                        "Неполные папки маршрутов: {}",
                                        self.incomplete_folders.len()
                                    ),
                                );
                                if ui.button("Открыть отчёт о неполных папках...").clicked()
                                {
                                    self.incomplete_window_open = true;
                                    self.selected_incomplete = Some(0);
                                }
                            }

                            ui.horizontal(|ui| {
                                ui.label("Поиск:");
                                ui.text_edit_singleline(&mut self.search_query);
                            });

                            if self.routes.is_empty() {
                                if self.incomplete_folders.is_empty() {
                                    ui.label("Маршруты не загружены. Нажмите «Обновить список».");
                                } else {
                                    ui.label("Полноценных маршрутов в этой папке не найдено.");
                                }
                            } else {
                                let available_height = ui.available_height();
                                let query = self.search_query.trim().to_lowercase();
                                let matching_routes = self
                                    .routes
                                    .iter()
                                    .filter(|route| {
                                        query.is_empty()
                                            || route.name.to_lowercase().contains(&query)
                                            || route.id.to_lowercase().contains(&query)
                                    })
                                    .count();
                                if matching_routes == 0 {
                                    ui.label("Маршруты по запросу не найдены.");
                                }
                                egui::ScrollArea::vertical()
                                    .id_salt("route_list")
                                    .auto_shrink([false, false])
                                    .max_height(available_height)
                                    .show(ui, |ui| {
                                        for (index, route) in self.routes.iter().enumerate() {
                                            if !query.is_empty()
                                                && !route.name.to_lowercase().contains(&query)
                                                && !route.id.to_lowercase().contains(&query)
                                            {
                                                continue;
                                            }
                                            let label = format!("{}  ({})", route.name, route.id);
                                            ui.horizontal(|ui| {
                                                if let Some(texture_id) = route_icon_texture_ids
                                                    .get(index)
                                                    .copied()
                                                    .flatten()
                                                {
                                                    ui.add(
                                            egui::Image::new((
                                                texture_id,
                                                egui::vec2(256.0, 128.0),
                                            ))
                                            .fit_to_exact_size(egui::vec2(256.0, 128.0)),
                                        );
                                                }
                                                ui.add_enabled_ui(
                                                    self.asset_replacement.is_none()
                                                        && self.check.is_none()
                                                        && self.inventory_scan.is_none()
                                                        && self.umlaut_repair.is_none()
                                                        && self.pending_umlaut_repair.is_none(),
                                                    |ui| {
                                                        if ui
                                                            .selectable_label(
                                                                self.selected_route
                                                                    == Some(index),
                                                                label,
                                                            )
                                                            .clicked()
                                                        {
                                                            self.selected_route = Some(index);
                                                            self.result = None;
                                                            if let Ok(mut state) =
                                                                self.asset_inventory_state.lock()
                                                            {
                                                                state.open = false;
                                                            } else {
                                                                eprintln!(
                                                                    "Не удалось закрыть инвентарь при смене маршрута."
                                                                );
                                                            }
                                                        }
                                                    },
                                                );
                                            });
                                        }
                                    });
                            }
                        });
                    },
                );
                ui.allocate_ui_with_layout(
                    egui::vec2(check_width, content_size.y),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.set_width(check_width);
                        ui.group(|ui| {
                            egui::ScrollArea::vertical()
                                .id_salt("route_check_panel")
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    ui.heading("Проверка и результаты");
                                    if let Some(route) =
                                        self.selected_route.and_then(|index| self.routes.get(index))
                                    {
                                        ui.label(format!("Выбран: {} ({})", route.name, route.id));
                                    } else {
                                        ui.label("Маршрут не выбран.");
                                    }

                                    ui.add_enabled_ui(
                                        self.asset_replacement.is_none()
                                            && self.inventory_scan.is_none()
                                            && self.umlaut_repair.is_none()
                                            && self.pending_umlaut_repair.is_none(),
                                        |ui| {
                                            ui.checkbox(
                                                &mut self.ignore_scenarios,
                                                "Игнорировать ассеты сценариев",
                                            );
                                        },
                                    );
                                    let checking = self.check.is_some();
                                    let can_check = self.selected_route.is_some()
                                        && !checking
                                        && self.inventory_scan.is_none()
                                        && self.asset_replacement.is_none()
                                        && self.umlaut_repair.is_none()
                                        && self.pending_umlaut_repair.is_none();
                                    if ui
                                        .add_enabled(
                                            can_check,
                                            egui::Button::new("Проверить выбранный маршрут"),
                                        )
                                        .clicked()
                                    {
                                        self.start_check();
                                    }
                                    let selected_route_path = self
                                        .selected_route
                                        .and_then(|index| self.routes.get(index))
                                        .map(|route| route.path.clone());
                                    if ui
                                        .add_enabled(
                                            selected_route_path.is_some(),
                                            egui::Button::new(
                                                "Открыть папку маршрута в Проводнике",
                                            ),
                                        )
                                        .clicked()
                                    {
                                        open_folder_request = selected_route_path;
                                    }
                                    if checking {
                                        ui.horizontal(|ui| {
                                            ui.spinner();
                                            ui.label(&self.status);
                                        });
                                    }
                                    if self.asset_replacement.is_some()
                                        || self.umlaut_repair.is_some()
                                    {
                                        ui.horizontal(|ui| {
                                            ui.spinner();
                                            ui.label(&self.status);
                                        });
                                    }
                                    if let Some(feedback) = &self.asset_replacement_feedback {
                                        match feedback {
                                            Ok(message) => {
                                                ui.colored_label(
                                                    egui::Color32::LIGHT_GREEN,
                                                    message,
                                                );
                                            }
                                            Err(error) => {
                                                ui.colored_label(egui::Color32::LIGHT_RED, error);
                                            }
                                        }
                                    }

                                    ui.separator();
                                    if let Some(result) = &self.result {
                                        ui.label(format!(
                        "Файлов маршрута: {}  |  Конвертировано BIN: {}  |  Уникальных ссылок: {}",
                        result.route_files,
                        result.converted_bin_files,
                        result.referenced_assets
                    ));
                                        if ui.button("Открыть список всех ассетов маршрута...").clicked() {
                                            open_inventory_request = true;
                                        }
                                        for issue in &result.structure_issues {
                                            ui.colored_label(
                                                egui::Color32::YELLOW,
                                                format!("Структура: {issue}"),
                                            );
                                        }
                                        if !result.conversion_errors.is_empty() {
                                            ui.collapsing(
                                                format!(
                                                    "Ошибки конвертации/XML ({})",
                                                    result.conversion_errors.len()
                                                ),
                                                |ui| {
                                                    egui::ScrollArea::vertical()
                                                        .id_salt("conversion_errors")
                                                        .max_height(120.0)
                                                        .show(ui, |ui| {
                                                            for error in &result.conversion_errors {
                                                                ui.colored_label(
                                                                    egui::Color32::LIGHT_RED,
                                                                    error,
                                                                );
                                                            }
                                                        });
                                                },
                                            );
                                        }
                                        if result.missing_assets.is_empty() {
                                            ui.colored_label(
                                                egui::Color32::LIGHT_GREEN,
                                                "Отсутствующих ассетов нет.",
                                            );
                                        } else {
                                            let missing_assets = result.missing_assets.clone();
                                            ui.colored_label(
                                                egui::Color32::LIGHT_RED,
                                                format!(
                                                    "Отсутствует ассетов: {}",
                                                    missing_assets.len()
                                                ),
                                            );
                                            let replacement_busy = checking
                                                || self.inventory_scan.is_some()
                                                || self.asset_replacement.is_some()
                                                || self.umlaut_repair.is_some()
                                                || self.pending_umlaut_repair.is_some()
                                                || self.selected_route.is_none();
                                            if self.asset_replacement.is_some() {
                                                ui.label(
                                                    "Не закрывайте программу, пока выполняется замена объектов.",
                                                );
                                            }
                                            egui::ScrollArea::vertical()
                                                .id_salt("missing_assets")
                                                .auto_shrink([false, false])
                                                .show(ui, |ui| {
                                                    for (kind, heading) in [
                                                        (AssetKind::Loft, "Loft (Networks)"),
                                                        (AssetKind::Scenery, "Scenery"),
                                                        (AssetKind::Other, "Другие типы"),
                                                    ] {
                                                        let assets: Vec<_> = missing_assets
                                                            .iter()
                                                            .filter(|asset| {
                                                                asset_kind(&asset.path) == kind
                                                            })
                                                            .cloned()
                                                            .collect();
                                                        if assets.is_empty() {
                                                            continue;
                                                        }
                                                        ui.collapsing(
                                                            format!(
                                                                "{heading} ({})",
                                                                assets.len()
                                                            ),
                                                            |ui| {
                                                                for asset in &assets {
                                                                    ui.horizontal(|ui| {
                                                                        ui.colored_label(
                                                                            egui::Color32::LIGHT_RED,
                                                                            &asset.path,
                                                                        );
                                                                        if ui
                                                                            .add_enabled(
                                                                                !replacement_busy,
                                                                                egui::Button::new("Заменить во всём маршруте..."),
                                                                            )
                                                                            .clicked()
                                                                        {
                                                                            replace_asset_request =
                                                                                Some(asset.clone());
                                                                        }
                                                                    });
                                                                    let mut uses_by_tile: HashMap<
                                                                        String,
                                                                        Vec<String>,
                                                                    > = HashMap::new();
                                                                    for usage in &asset.uses {
                                                                        let route_relative =
                                                                            self.selected_route
                                                                                .and_then(|index| self.routes.get(index))
                                                                                .and_then(|route| usage.source_file.strip_prefix(&route.path).ok())
                                                                                .map(|path| path.display().to_string())
                                                                                .unwrap_or_else(|| usage.source_file.display().to_string());
                                                                        uses_by_tile
                                                                            .entry(usage.tile.clone())
                                                                            .or_default()
                                                                            .push(route_relative);
                                                                    }
                                                                    let mut tiles: Vec<_> =
                                                                        uses_by_tile.into_iter().collect();
                                                                    tiles.sort_by(|left, right| left.0.cmp(&right.0));
                                                                    ui.indent(
                                                                        ("asset_uses", &asset.path),
                                                                        |ui| {
                                                                            ui.label(format!(
                                                                                "Использований: {}",
                                                                                asset.uses.len()
                                                                            ));
                                                                            for (tile, files) in tiles {
                                                                                ui.label(format!("• {tile}"));
                                                                                for file in files {
                                                                                    ui.small(file);
                                                                                }
                                                                            }
                                                                        },
                                                                    );
                                                                }
                                                            },
                                                        );
                                                    }
                                                });
                                        }

                                        if result.umlaut_assets.is_empty() {
                                            ui.colored_label(
                                                egui::Color32::LIGHT_GREEN,
                                                "Ассетов с умляутами или подозрительным U+FFDF не обнаружено.",
                                            );
                                        } else {
                                            let assets = result.umlaut_assets.clone();
                                            ui.colored_label(
                                                egui::Color32::YELLOW,
                                                format!(
                                                    "Ассеты с умляутами или подозрительным U+FFDF — требуют проверки: {}",
                                                    assets.len()
                                                ),
                                            );
                                            egui::ScrollArea::vertical()
                                                .id_salt("umlaut_assets")
                                                .auto_shrink([false, false])
                                                .show(ui, |ui| {
                                                    for asset in assets {
                                                        ui.horizontal(|ui| {
                                                            ui.colored_label(
                                                                egui::Color32::YELLOW,
                                                                &asset.path,
                                                            );
                                                            let busy = checking
                                                                || self.inventory_scan.is_some()
                                                                || self.asset_replacement.is_some()
                                                                || self.umlaut_repair.is_some()
                                                                || self.pending_umlaut_repair.is_some()
                                                                || self.selected_route.is_none();
                                                            if ui
                                                                .add_enabled(
                                                                    !busy,
                                                                    egui::Button::new(
                                                                        "Транслитерировать цепочку...",
                                                                    ),
                                                                )
                                                                .clicked()
                                                            {
                                                                repair_umlaut_request =
                                                                    Some(asset.clone());
                                                            }
                                                        });
                                                        for evidence in &asset.evidence {
                                                            ui.small(format!("• {evidence}"));
                                                        }
                                                        let mut uses_by_tile: HashMap<
                                                            String,
                                                            Vec<String>,
                                                        > = HashMap::new();
                                                        for usage in &asset.uses {
                                                            let path = self
                                                                .selected_route
                                                                .and_then(|index| self.routes.get(index))
                                                                .and_then(|route| usage.source_file.strip_prefix(&route.path).ok())
                                                                .map(|path| path.display().to_string())
                                                                .unwrap_or_else(|| usage.source_file.display().to_string());
                                                            uses_by_tile
                                                                .entry(usage.tile.clone())
                                                                .or_default()
                                                                .push(path);
                                                        }
                                                        let mut tiles: Vec<_> =
                                                            uses_by_tile.into_iter().collect();
                                                        tiles.sort_by(|left, right| {
                                                            left.0.cmp(&right.0)
                                                        });
                                                        ui.indent(
                                                            ("umlaut_asset_uses", &asset.path),
                                                            |ui| {
                                                                for (tile, files) in tiles {
                                                                    ui.label(format!("• {tile}"));
                                                                    for file in files {
                                                                        ui.small(file);
                                                                    }
                                                                }
                                                            },
                                                        );
                                                    }
                                                });
                                        }
                                    }
                                });
                        });
                    },
                );
            });
        });

        if open_inventory_request {
            self.open_asset_inventory();
        }
        if let Some(missing) = replace_asset_request {
            let assets_path = PathBuf::from(self.rw_path.trim()).join("Assets");
            let selected = rfd::FileDialog::new()
                .set_title("Выберите BIN нового объекта")
                .add_filter("RailWorks BIN", &["bin"])
                .set_directory(&assets_path)
                .pick_file();
            if let Some(replacement) = selected {
                self.start_asset_replacement(missing, replacement);
            }
        }
        if let Some(asset) = repair_umlaut_request {
            self.pending_umlaut_repair = Some(asset);
        }
        if let Some(asset) = self.pending_umlaut_repair.clone() {
            let mut confirm = false;
            let mut cancel = false;
            egui::Window::new("Подтверждение транслитерации ассета")
                .collapsible(false)
                .resizable(true)
                .show(ctx, |ui| {
                    ui.label(format!("Ассет: {}", asset.path));
                    ui.label(
                        "Будут транслитерированы имена и текст внутри BIN, связанные GeoPcDx и ссылки на TgPcDx. Если затронут TgPcDx, проверятся GeoPcDx во всём продукте.",
                    );
                    ui.label(
                        "Папки Provider/Product и вложенные папки автоматически не переименовываются; если умляут есть в их именах, операция остановится без изменений.",
                    );
                    ui.label(
                        "Перед установкой проверяются конфликты имён. Для каждого изменённого файла сохраняется исходник с суффиксом .bak1, .bak2 и далее.",
                    );
                    ui.label("Обнаружения:");
                    for evidence in &asset.evidence {
                        ui.small(evidence);
                    }
                    ui.horizontal(|ui| {
                        if ui.button("Транслитерировать").clicked() {
                            confirm = true;
                        }
                        if ui.button("Отмена").clicked() {
                            cancel = true;
                        }
                    });
                });
            if cancel {
                self.pending_umlaut_repair = None;
            } else if confirm {
                self.pending_umlaut_repair = None;
                self.start_umlaut_repair(asset);
            }
        }

        if self.settings_window_open {
            egui::Window::new("Настройки")
                .open(&mut self.settings_window_open)
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.style_mut()
                        .text_styles
                        .insert(egui::TextStyle::Body, egui::FontId::proportional(18.0));
                    ui.style_mut()
                        .text_styles
                        .insert(egui::TextStyle::Small, egui::FontId::proportional(14.0));
                    ui.style_mut()
                        .text_styles
                        .insert(egui::TextStyle::Button, egui::FontId::proportional(16.0));
                    if ui
                        .checkbox(
                            &mut self.route_thumbnails_enabled,
                            "Показывать миниатюры маршрутов",
                        )
                        .changed()
                    {
                        let enabled = self.route_thumbnails_enabled;
                        match save_route_thumbnail_setting(enabled) {
                            Ok(()) => {
                                self.status = if enabled {
                                    "Миниатюры маршрутов включены.".to_owned()
                                } else {
                                    "Миниатюры маршрутов отключены.".to_owned()
                                };
                            }
                            Err(error) => {
                                self.route_thumbnails_enabled = !enabled;
                                self.status = error;
                            }
                        }
                    }
                    ui.small(
                        "Иконки RouteInformation будут показываться в списке маршрутов и отчёте о неполных папках.",
                    );
                    ui.separator();
                    let previous_limit = self.serz_process_limit;
                    ui.add(
                        egui::Slider::new(
                            &mut self.serz_process_limit,
                            1..=maximum_serz_processes(),
                        )
                        .text("Параллельные Serz-процессы"),
                    );
                    if self.serz_process_limit != previous_limit {
                        let limit = self.serz_process_limit;
                        match save_serz_process_limit(limit) {
                            Ok(()) => {
                                self.status = format!(
                                    "Лимит параллельных Serz-процессов: {limit}."
                                );
                            }
                            Err(error) => {
                                self.serz_process_limit = previous_limit;
                                self.status = error;
                            }
                        }
                    }
                    ui.small(format!(
                        "Доступно логических процессоров: {}. Это максимальное число одновременно запущенных Serz, не число потоков внутри каждого процесса.",
                        maximum_serz_processes()
                    ));
                });
        }

        self.show_asset_editor(ctx);

        if self.ignore_list_window_open {
            egui::Window::new("Игнор-лист маршрутов")
                .open(&mut self.ignore_list_window_open)
                .default_size([520.0, 360.0])
                .resizable(true)
                .show(ctx, |ui| {
                    ui.label(
                        "Эти папки не будут отмечаться дефектными при сканировании. Удалите запись из списка, чтобы снова проверять папку.",
                    );
                    ui.separator();
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        if self.ignored_route_ids.is_empty() {
                            ui.label("Игнор-лист пуст.");
                        }
                        let mut ids: Vec<_> = self.ignored_route_ids.iter().cloned().collect();
                        ids.sort();
                        for id in ids {
                            ui.horizontal(|ui| {
                                ui.label(&id);
                                if ui.button("Убрать").clicked() {
                                    remove_ignored_route = Some(id.clone());
                                }
                            });
                        }
                    });
                });
        }

        if self.incomplete_window_open {
            let reports = self.incomplete_folders.clone();
            let icon_texture_ids: Vec<_> = reports
                .iter()
                .map(|folder| {
                    if self.route_thumbnails_enabled {
                        folder
                            .icon_path
                            .as_deref()
                            .and_then(|icon_path| self.route_icon_texture_id(ctx, icon_path))
                    } else {
                        None
                    }
                })
                .collect();
            let screen_size = ctx.input(|input| input.screen_rect().size());
            let max_window_size = egui::vec2(
                (screen_size.x - 24.0).max(320.0),
                (screen_size.y - 24.0).max(240.0),
            );
            egui::Window::new(format!(
                "Отчёт о неполных маршрутах ({})",
                reports.len()
            ))
            .open(&mut self.incomplete_window_open)
            .default_size([
                (screen_size.x * 0.85).min(1100.0),
                (screen_size.y * 0.85).min(760.0),
            ])
            .max_size(max_window_size)
            .resizable(true)
            .show(ctx, |ui| {
                ui.label(
                    "Сортировка по числу файлов",
                );
                if let Some(feedback) = &self.delete_feedback {
                    match feedback {
                        Ok(count) => {
                            ui.colored_label(
                                egui::Color32::LIGHT_GREEN,
                                format!("Перемещено в Корзину Windows: {count} папок."),
                            );
                        }
                        Err(error) => {
                            ui.colored_label(egui::Color32::LIGHT_RED, error);
                        }
                    }
                }
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            !reports.is_empty()
                                && self.delete_rx.is_none()
                                && self.route_scan.is_none(),
                            egui::Button::new(format!(
                                "Удалить все {}",
                                reports.len()
                            )),
                        )
                        .clicked()
                    {
                        delete_all_requested = true;
                    }
                });
                ui.separator();

                ui.columns(2, |columns| {
                    columns[0].vertical(|ui| {
                        ui.heading("Папки");
                        let list_height = ui.available_height();
                        egui::ScrollArea::vertical()
                            .id_salt("incomplete_folder_picker")
                            .auto_shrink([false, false])
                            .max_height(list_height)
                            .show(ui, |ui| {
                                for (index, folder) in reports.iter().enumerate() {
                                    let selected = self.selected_incomplete == Some(index);
                                    ui.horizontal(|ui| {
                                        if let Some(texture_id) =
                                            icon_texture_ids.get(index).copied().flatten()
                                        {
                                            ui.add(
                                                egui::Image::new((
                                                    texture_id,
                                                    egui::vec2(256.0, 128.0),
                                                ))
                                                .fit_to_exact_size(egui::vec2(256.0, 128.0)),
                                            );
                                        }
                                        if ui
                                            .selectable_label(
                                                selected,
                                                format!(
                                                    "{} — {} файл(ов)",
                                                    folder.id, folder.file_count
                                                ),
                                            )
                                            .clicked()
                                        {
                                            self.selected_incomplete = Some(index);
                                        }
                                    });
                                }
                            });
                    });
                    columns[1].vertical(|ui| {
                        ui.heading("Отчёт");
                        if let Some(folder) = self
                            .selected_incomplete
                            .and_then(|index| reports.get(index))
                        {
                            ui.add(
                                egui::Label::new(folder.path.display().to_string())
                                    .wrap_mode(egui::TextWrapMode::Wrap),
                            );
                            ui.horizontal_wrapped(|ui| {
                                if ui.button("Открыть в Проводнике").clicked() {
                                    open_folder_request = Some(folder.path.clone());
                                }
                                if !self.ignored_route_ids.contains(&folder.id)
                                    && ui.button("В игнор").clicked()
                                {
                                    ignore_route_request =
                                        Some((folder.id.clone(), folder.path.clone()));
                                }
                                if ui
                                    .add_enabled(
                                        self.delete_rx.is_none() && self.route_scan.is_none(),
                                        egui::Button::new(
                                            "Удалить в корзину",
                                        ),
                                    )
                                    .clicked()
                                {
                                    delete_request = Some(folder.path.clone());
                                }
                            });
                        } else {
                            ui.label("Выберите папку слева.");
                        }
                        ui.separator();
                        let report_height = ui.available_height();
                        egui::ScrollArea::vertical()
                            .id_salt("incomplete_folder_details")
                            .auto_shrink([false, false])
                            .max_height(report_height)
                            .show(ui, |ui| {
                                if let Some(folder) = self
                                    .selected_incomplete
                                    .and_then(|index| reports.get(index))
                                {
                                    if let Some(name) = &folder.suggested_name {
                                        ui.label(format!(
                                            "Возможное название маршрута: {name}"
                                        ));
                                    }
                                    for problem in &folder.problems {
                                        ui.colored_label(egui::Color32::YELLOW, problem);
                                    }
                                    if !folder.scenario_clues.is_empty() {
                                        ui.separator();
                                        ui.heading("Подсказки из сценариев");
                                        ui.label(
                                            "Названия сценариев и упомянутые места могут помочь опознать маршрут; это подсказки, а не гарантированный список станций.",
                                        );
                                        for clue in &folder.scenario_clues {
                                            ui.add(
                                                egui::Label::new(clue)
                                                    .wrap_mode(egui::TextWrapMode::Wrap),
                                            );
                                        }
                                    }
                                    ui.separator();
                                    ui.label("Состав папки и архивов:");
                                    if folder.inventory.is_empty() {
                                        ui.label("Папка пуста.");
                                    } else {
                                        for entry in &folder.inventory {
                                            ui.add(
                                                egui::Label::new(entry)
                                                    .wrap_mode(egui::TextWrapMode::Wrap),
                                            );
                                        }
                                    }
                                } else {
                                    ui.label("Выберите папку слева.");
                                }
                            });
                    });
                });
            });
        }

        if let Some((id, path)) = ignore_route_request {
            self.ignored_route_ids.insert(id.clone());
            match save_ignored_route_ids(&self.ignored_route_ids) {
                Ok(()) => {
                    self.incomplete_folders.retain(|folder| folder.id != id);
                    self.selected_incomplete = (!self.incomplete_folders.is_empty()).then_some(0);
                    match (read_route_name(&path, &id), find_route_icon(&path)) {
                        (Ok(name), Ok(icon_path)) => {
                            self.routes.push(Route {
                                id: id.clone(),
                                name,
                                icon_path,
                                path,
                            });
                            self.routes.sort_by(|left, right| {
                                left.name
                                    .to_lowercase()
                                    .cmp(&right.name.to_lowercase())
                                    .then_with(|| left.id.cmp(&right.id))
                            });
                            self.status = format!("Маршрут {id} добавлен в игнор-лист.");
                        }
                        (Err(error), _) | (_, Err(error)) => self.status = error,
                    }
                }
                Err(error) => {
                    self.ignored_route_ids.remove(&id);
                    self.status = error;
                }
            }
        }

        if let Some(id) = remove_ignored_route {
            self.ignored_route_ids.remove(&id);
            match save_ignored_route_ids(&self.ignored_route_ids) {
                Ok(()) => {
                    self.status = format!("Маршрут {id} удалён из игнор-листа.");
                    self.start_route_scan();
                }
                Err(error) => {
                    self.ignored_route_ids.insert(id);
                    self.status = error;
                }
            }
        }

        if let Some(path) = open_folder_request {
            match open_in_file_explorer(&path) {
                Ok(()) => self.status = format!("Открыта папка: {}", path.display()),
                Err(error) => self.status = error,
            }
        }

        if let Some(folder) = delete_request {
            self.pending_delete = Some(vec![folder]);
        } else if delete_all_requested {
            self.pending_delete = Some(
                self.incomplete_folders
                    .iter()
                    .map(|folder| folder.path.clone())
                    .collect(),
            );
        }

        if let Some(folders) = self.pending_delete.clone() {
            let mut confirm = false;
            let mut cancel = false;
            egui::Window::new("Подтверждение удаления")
                .collapsible(false)
                .resizable(true)
                .show(ctx, |ui| {
                    ui.label(format!(
                        "Переместить в корзину все {}?",
                        folders.len()
                    ));
                    ui.label("Перед удалением будет проверено, что каждая папка является непосредственной папкой внутри Content\\Routes.");
                    egui::ScrollArea::vertical()
                        .max_height(160.0)
                        .show(ui, |ui| {
                            for folder in &folders {
                                ui.label(folder.display().to_string());
                            }
                        });
                    ui.horizontal(|ui| {
                        if ui.button("Переместить в Корзину").clicked() {
                            confirm = true;
                        }
                        if ui.button("Отмена").clicked() {
                            cancel = true;
                        }
                    });
                });
            if cancel {
                self.pending_delete = None;
            } else if confirm {
                self.start_delete(folders);
            }
        }
        self.show_asset_inventory_viewport(ctx);
    }
}
