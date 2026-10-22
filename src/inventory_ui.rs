//! Таблица инвентаря ассетов: список используемых объектов маршрута.

use super::*;
pub(crate) type InventoryRow<'a> = (
    &'a str,
    &'a [AssetUse],
    bool,
    Option<&'a UmlautAsset>,
);

/// Действия, запрошенные строками таблицы инвентаря.
#[derive(Default)]
pub(crate) struct InventoryRowActions {
    /// Ассет с умляутами, который нужно транслитерировать.
    pub(crate) repair: Option<UmlautAsset>,
    /// Папка с ассетом, которую нужно открыть в Проводнике.
    pub(crate) open_folder: Option<PathBuf>,
    /// Прямоугольники ячеек с путём ассетов: нужны только в тестах.
    #[cfg(test)]
    pub(crate) path_rects: Vec<egui::Rect>,
    /// Прямоугольники ячеек с числом использований: нужны только в тестах.
    #[cfg(test)]
    pub(crate) usage_rects: Vec<egui::Rect>,
}

/// Высота строки таблицы инвентаря.
pub(crate) const ROW_HEIGHT: f32 = 18.0;

/// Идентификатор таблицы инвентаря: по нему egui_extras хранит ширины столбцов.
pub(crate) const TABLE_ID_SALT: &str = "asset_inventory";

/// Столбец, по которому отсортирована таблица инвентаря.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum InventorySortColumn {
    /// По названию ассета.
    #[default]
    Path,
    /// По числу использований.
    Usage,
}

/// Сортировка таблицы инвентаря.
#[derive(Clone, Copy, Default)]
pub(crate) struct InventorySort {
    /// Столбец, по которому идёт сортировка.
    pub(crate) column: InventorySortColumn,
    /// Сортировка по убыванию.
    pub(crate) descending: bool,
}

impl InventorySort {
    /// Переключает сортировку по столбцу: повторный клик меняет направление.
    fn toggle(&mut self, column: InventorySortColumn) {
        if self.column == column {
            self.descending = !self.descending;
        } else {
            self.column = column;
            self.descending = false;
        }
    }
}

/// Сортирует строки таблицы инвентаря.
pub(crate) fn sort_inventory_rows(rows: &mut [InventoryRow<'_>], sort: InventorySort) {
    match sort.column {
        InventorySortColumn::Path => {
            if sort.descending {
                rows.sort_by_key(|row| std::cmp::Reverse(row.0));
            } else {
                rows.sort_by_key(|row| row.0);
            }
        }
        InventorySortColumn::Usage => {
            if sort.descending {
                rows.sort_by_key(|row| std::cmp::Reverse(row.1.len()));
            } else {
                rows.sort_by_key(|row| row.1.len());
            }
        }
    }
}

/// Ширины столбцов таблицы инвентаря.
///
/// Считаются один раз для шапки и для строк, чтобы подписи и данные стояли
/// в одних и тех же местах.
#[derive(Clone, Copy)]
pub(crate) struct InventoryLayout {
    pub(crate) path_width: f32,
    usage_width: f32,
    tiles_width: f32,
    status_width: f32,
}

impl InventoryLayout {
    /// Раскладывает таблицу по ширине окна. `table_width` берётся с запасом на
    /// полосу прокрутки, чтобы шапка и строки оставались в одних и тех же границах.
    pub(crate) fn new(table_width: f32, show_tiles: bool) -> Self {
        let usage_width = 112.0;
        let status_width = 200.0;
        let tiles_width = if show_tiles {
            (table_width * 0.28).clamp(120.0, 400.0)
        } else {
            0.0
        };
        let path_width = (table_width - usage_width - status_width - tiles_width).max(200.0);
        Self {
            path_width,
            usage_width,
            tiles_width,
            status_width,
        }
    }
}


/// Измеряет ширину текста в стиле мелкого шрифта.
pub(crate) fn measure_small_text(ui: &egui::Ui, font_id: &egui::FontId, text: &str) -> f32 {
    ui.painter()
        .layout_no_wrap(text.to_string(), font_id.clone(), ui.visuals().text_color())
        .size()
        .x
}

/// Рисует текст в ячейке таблицы, выравнивая его по краю ячейки.
pub(crate) fn paint_cell_text(
    ui: &egui::Ui,
    rect: egui::Rect,
    text: &str,
    color: egui::Color32,
    font_id: egui::FontId,
    align: egui::Align,
) {
    let galley = ui.painter().layout_no_wrap(text.to_owned(), font_id, color);
    let x = match align {
        egui::Align::Min => rect.left(),
        egui::Align::Center => rect.center().x - galley.size().x / 2.0,
        egui::Align::Max => rect.right() - galley.size().x,
    };
    let y = rect.center().y - galley.size().y / 2.0;
    ui.painter().galley(egui::pos2(x, y), galley, color);
}

/// Подпись в ячейке таблицы: измеряется по полному тексту, а рисуется обрезанным.
///
/// Так двойной клик по границе столбца подгоняет его ширину под содержимое,
/// как в Excel, и при этом в ячейке остаётся укороченный текст.
pub(crate) struct FittedLabel {
    /// Текст, по которому измеряется ширина столбца.
    pub(crate) full: String,
    /// Текст, который видно в ячейке.
    pub(crate) shown: String,
    pub(crate) color: egui::Color32,
    pub(crate) font_id: egui::FontId,
    /// Мерить ли столбец по полному тексту: нужно в кадре подгонки ширины.
    pub(crate) measure_full: bool,
    /// Как расположить текст в ячейке.
    pub(crate) align: egui::Align,
}
impl egui::Widget for FittedLabel {
    fn ui(self, ui: &mut egui::Ui) -> egui::Response {
        let font_id = self.font_id;
        let color = self.color;
        let full = self.full;
        let shown = self.shown;
        let shown_galley = ui.painter().layout_no_wrap(shown, font_id.clone(), color);
        let shown_size = shown_galley.size();
        let size = if self.measure_full {
            ui.painter().layout_no_wrap(full, font_id, color).size()
        } else {
            shown_size
        };
        // Место занимает измеренный текст: по нему считается ширина столбца.
        let (_rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
        // Текст рисуется по краю ячейки, а место под него выделено по размеру
        // текста: так ячейка не растягивает столбец.
        let cell = ui.max_rect();
        let x = match self.align {
            egui::Align::Min => cell.left(),
            egui::Align::Center => cell.center().x - shown_size.x / 2.0,
            egui::Align::Max => cell.right() - shown_size.x,
        };
        let pos = egui::pos2(x, cell.center().y - shown_size.y / 2.0);
        // Реагируем только на видимый текст, а не на всю измеренную ширину.
        let shown_rect = egui::Rect::from_min_size(pos, shown_size);
        let response = ui.interact(shown_rect, ui.scope_id().with("fitted_label"), egui::Sense::click());
        // Текст рисуется через механизм выделения: его можно выделить мышью и
        // скопировать через Ctrl+C.
        egui::text_selection::LabelSelectionState::label_text_selection(
            ui,
            &response,
            pos,
            shown_galley,
            color,
            egui::Stroke::NONE,
        );
        response
    }
}

/// Делит список тайлов на часть, помещающуюся в ячейку, и часть, уходящую в подсказку.
///
/// Так список тайлов не показывается дважды: в ячейке видно начало, в подсказке — продолжение.
/// Третьим возвращается весь список: по нему измеряется ширина столбца.
pub(crate) fn split_tiles_for_cell(
    ui: &egui::Ui,
    tiles_width: f32,
    tiles: &[(&str, usize)],
) -> (String, String, String) {
    let font_id = ui
        .style()
        .text_styles
        .get(&egui::TextStyle::Small)
        .cloned()
        .unwrap_or_default();
    let mut visible = String::new();
    let mut full = String::new();
    let mut hidden: Vec<String> = Vec::new();
    let mut used = 0.0;
    for (tile, count) in tiles {
        let token = format!("{} ({count})", tile.trim_start_matches("тайл "));
        let separator = if visible.is_empty() {
            0.0
        } else {
            measure_small_text(ui, &font_id, ", ")
        };
        let width = measure_small_text(ui, &font_id, &token) + separator;
        if !full.is_empty() {
            full.push_str(", ");
        }
        full.push_str(&token);
        if visible.is_empty() || used + width <= tiles_width {
            if !visible.is_empty() {
                visible.push_str(", ");
            }
            visible.push_str(&token);
            used += width;
        } else {
            hidden.push(format!("{tile} — {count}"));
        }
    }
    let hint = if hidden.is_empty() {
        String::new()
    } else {
        format!(
            "Ещё {} {}:\n{}",
            hidden.len(),
            plural_tiles(hidden.len()),
            hidden.join("\n")
        )
    };
    (visible, hint, full)
}

/// Склонение слова «тайл» по числу.
pub(crate) fn plural_tiles(count: usize) -> &'static str {
    match count % 10 {
        1 if count % 100 != 11 => "тайл",
        2..=4 if !(12..=14).contains(&(count % 100)) => "тайла",
        _ => "тайлов",
    }
}

/// Рисует треугольник-указатель сортировки.
///
/// В стандартном шрифте egui нет символов-стрелок, поэтому указатель рисуется
/// фигурой, а не текстом.
pub(crate) fn paint_sort_arrow(ui: &egui::Ui, rect: egui::Rect, descending: bool, color: egui::Color32) {
    let size = 4.0;
    let center = egui::pos2(rect.center().x, rect.center().y);
    let points = if descending {
        vec![
            egui::pos2(center.x - size, center.y - size / 2.0),
            egui::pos2(center.x + size, center.y - size / 2.0),
            egui::pos2(center.x, center.y + size),
        ]
    } else {
        vec![
            egui::pos2(center.x - size, center.y + size / 2.0),
            egui::pos2(center.x + size, center.y + size / 2.0),
            egui::pos2(center.x, center.y - size),
        ]
    };
    ui.painter()
        .add(egui::Shape::convex_polygon(points, color, egui::Stroke::NONE));
}

/// Ячейка шапки: подпись столбца по центру.
///
/// Подпись рисуется вручную: обычный виджет занимает всю ширину ячейки, и
/// столбец перестаёт подгоняться под содержимое по двойному клику.
pub(crate) fn inventory_header_cell(ui: &mut egui::Ui, title: &str) -> egui::Response {
    let font_id = egui::FontId::proportional(ui.text_style_height(&egui::TextStyle::Body));
    let color = ui.visuals().strong_text_color();
    let text_width = ui
        .painter()
        .layout_no_wrap(title.to_owned(), font_id.clone(), color)
        .size()
        .x;
    // Ячейка не занимает ширину столбца: ширина измеряется по содержимому.
    let _ = ui.allocate_exact_size(egui::vec2(0.0, ROW_HEIGHT), egui::Sense::hover());
    let cell = ui.max_rect();
    let click_rect = egui::Rect::from_center_size(
        cell.center(),
        egui::vec2(text_width + 12.0, ROW_HEIGHT),
    );
    let response = ui.interact(
        click_rect,
        ui.scope_id().with(("header_cell", title)),
        egui::Sense::click(),
    );
    paint_cell_text(ui, cell, title, color, font_id, egui::Align::Center);
    response
}

pub(crate) struct InventoryTable<'a> {
    /// Строки таблицы.
    pub(crate) rows: &'a [InventoryRow<'a>],
    /// Корень папки с ассетами: нужен, чтобы открыть папку ассета.
    pub(crate) assets_root: &'a Path,
    /// Ширины столбцов.
    pub(crate) layout: InventoryLayout,
    /// Показывать ли столбец с тайлами.
    pub(crate) show_tiles: bool,
    /// Идёт ли сейчас транслитерация: кнопка починки недоступна.
    pub(crate) repair_busy: bool,
}

/// Таблица инвентаря ассетов.
///
/// Строки рисуются только видимые: список ассетов бывает на тысячи строк,
/// и перерисовка всех них каждый кадр съедала бы кадр.
pub(crate) fn asset_inventory_table(
    ui: &mut egui::Ui,
    table_data: &InventoryTable<'_>,
    sort: &mut InventorySort,
    actions: &mut InventoryRowActions,
) {
    let rows = table_data.rows;
    let assets_root = table_data.assets_root;
    let layout = &table_data.layout;
    let show_tiles = table_data.show_tiles;
    let repair_busy = table_data.repair_busy;
    let text_color = ui.visuals().text_color();
    // Идентификатор состояния таблицы нужен, чтобы поймать двойной клик по
    // границе столбца: egui_extras так подгоняет ширину под содержимое.
    let state_id = ui.scope_id().with(egui::IdSalt::new(TABLE_ID_SALT));
    let context = ui.ctx().clone();
    let real_columns = if show_tiles { 4 } else { 3 };
    // Индекс столбца с тайлами: он идёт за ассетом и использованиями.
    let tiles_column_index = 2;
    // Двойной клик по границе столбца ловится по ответу прошлого кадра: в этом
    // кадре столбец измеряется по полному содержимому и подгоняется под него.
    let mut auto_fit_column: Option<usize> = None;
    for index in 0..real_columns {
        let resize_id = state_id.with("resize_column").with(index);
        if let Some(response) = context.read_response(resize_id) {
            if response.double_clicked() {
                auto_fit_column = Some(index);
            }
        }
    }
    let mut path_column = egui_extras::Column::initial(layout.path_width)
        .at_least(120.0)
        .at_most(2000.0)
        .clip(true)
        .resizable(true);
    if auto_fit_column == Some(0) {
        path_column = path_column.auto_size_this_frame(true);
    }
    let mut table = egui_extras::TableBuilder::new(ui)
        .id_salt("asset_inventory")
        .striped(true)
        .resizable(true)
        // Таблица занимает столько места, сколько заняли строки, а не всю окна.
        .auto_shrink([false, true])
        .min_scrolled_height(40.0)
        .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
        // Все столбцы можно тянуть мышкой за границу, а двойной клик по границе
        // подгоняет ширину под содержимое.
        .column(path_column)
        .column(
            egui_extras::Column::initial(layout.usage_width)
                .at_least(60.0)
                .at_most(600.0)
                .resizable(true),
        );
    if show_tiles {
        let mut tiles_column = egui_extras::Column::initial(layout.tiles_width)
            .at_least(100.0)
            .at_most(1200.0)
            .clip(true)
            .resizable(true);
        if auto_fit_column == Some(tiles_column_index) {
            tiles_column = tiles_column.auto_size_this_frame(true);
        }
        table = table.column(tiles_column);
    }
    table = table.column(
        egui_extras::Column::initial(layout.status_width)
            .at_least(100.0)
            .at_most(800.0)
            .clip(true)
            .resizable(true),
    )
    // Пустой столбец-добивка: забирает свободное место, чтобы таблица всегда
    // была во всю ширину окна.
    .column(egui_extras::Column::remainder().at_least(0.0));

    table
        .header(ROW_HEIGHT, |mut header| {
            header.col(|ui| {
                inventory_sort_header(ui, "Ассет", sort, InventorySortColumn::Path).on_hover_text(
                    "Название ассета. Двойной клик открывает папку, где лежит ассет",
                );
            });
            header.col(|ui| {
                inventory_sort_header(ui, "Использований", sort, InventorySortColumn::Usage)
                    .on_hover_text("Сколько раз ассет встречается в файлах маршрута");
            });
            if show_tiles {
                header.col(|ui| {
                    inventory_header_cell(ui, "Тайлы").on_hover_text(
                        "Сколько объектов ассета размещено на каждом тайле",
                    );
                });
            }
            header.col(|ui| {
                inventory_header_cell(ui, "Состояние");
            });
            header.col(|_| {});
        })
        .body(|body| {
            body.rows(ROW_HEIGHT, rows.len(), |mut row| {
                let (path, uses, missing, repair_asset) = &rows[row.index()];
                let color = if *missing {
                    egui::Color32::LIGHT_RED
                } else if repair_asset.is_some() {
                    egui::Color32::YELLOW
                } else {
                    text_color
                };

                // Название ассета: по левому краю ячейки, длинные пути
                // обрезаются слева, чтобы осталось видно имя файла. Ширина
                // столбца меряется по полному пути — по ней работает двойной
                // клик по границе столбца.
                row.col(|ui| {
                    let width = ui.max_rect().width();
                    let (text, truncated) = elide_asset_path(ui, path, width);
                    let response = ui.add(FittedLabel {
                        full: (*path).to_owned(),
                        shown: text,
                        color,
                        font_id: egui::FontId::proportional(
                            ui.text_style_height(&egui::TextStyle::Body),
                        ),
                        measure_full: auto_fit_column == Some(0),
                        align: egui::Align::Min,
                    });
                    if truncated {
                        response.clone().on_hover_text(*path);
                    }
                    if response.double_clicked() {
                        actions.open_folder = asset_folder_to_open(assets_root, path);
                    }
                    #[cfg(test)]
                    actions.path_rects.push(response.rect);
                });

                // Число использований: по центру ячейки.
                row.col(|ui| {
                    let text = format!("×{}", uses.len());
                    let response = ui.add(FittedLabel {
                        full: text.clone(),
                        shown: text,
                        color: text_color,
                        font_id: egui::FontId::proportional(
                            ui.text_style_height(&egui::TextStyle::Body),
                        ),
                        measure_full: auto_fit_column == Some(1),
                        align: egui::Align::Center,
                    });
                    response.clone().on_hover_text(format!(
                        "Использований: {} (см. столбец «Использований»)",
                        uses.len()
                    ));
                    #[cfg(test)]
                    actions.usage_rects.push(response.rect);
                });

                if show_tiles {
                    row.col(|ui| {
                        let mut by_tile: HashMap<&str, usize> = HashMap::new();
                        for usage in *uses {
                            by_tile
                                .entry(&usage.tile)
                                .and_modify(|count| *count += 1)
                                .or_insert(1);
                        }
                        let mut tiles: Vec<_> = by_tile.into_iter().collect();
                        tiles.sort_by(|left, right| left.0.cmp(right.0));
                        let width = ui.max_rect().width();
                        let (summary, tile_hint, full) = split_tiles_for_cell(ui, width, &tiles);
                        let response = ui.add(FittedLabel {
                            full,
                            shown: summary,
                            color: text_color,
                            font_id: egui::FontId::proportional(
                                ui.text_style_height(&egui::TextStyle::Small),
                            ),
                            measure_full: auto_fit_column == Some(tiles_column_index),
                            align: egui::Align::Min,
                        });
                        if !tile_hint.is_empty() {
                            response.on_hover_text(tile_hint);
                        }
                    });
                }

                // Состояние ассета: метка об отсутствии и кнопка починки.
                row.col(|ui| {
                    ui.horizontal(|ui| {
                        if *missing {
                            ui.colored_label(egui::Color32::LIGHT_RED, "ОТСУТСТВУЕТ");
                        }
                        if let Some(asset) = repair_asset {
                            let mut hint =
                                String::from("Транслитерировать цепочку имён с умляутами");
                            for (index, evidence) in asset.evidence.iter().enumerate() {
                                if index == 0 {
                                    hint.push_str("\n\nОбнаружение: ");
                                } else {
                                    hint.push('\n');
                                }
                                hint.push_str(evidence);
                            }
                            if ui
                                .add_enabled(!repair_busy, egui::Button::new("Исправить…"))
                                .on_hover_text(hint)
                                .clicked()
                            {
                                actions.repair = Some((*asset).clone());
                            }
                        }
                    });
                });

                // Столбец-добивка: пустой, только чтобы таблица была во всю
                // ширину окна.
                row.col(|_| {});
            });
        });
    }

pub(crate) fn inventory_sort_header(
    ui: &mut egui::Ui,
    title: &str,
    sort: &mut InventorySort,
    column: InventorySortColumn,
) -> egui::Response {
    let sorted = sort.column == column;
    let response = inventory_header_cell(ui, title);
    if response.clicked() {
        sort.toggle(column);
    }
    if sorted {
        // Указатель ставим сразу за подписью столбца.
        let arrow_rect = egui::Rect::from_center_size(
            egui::pos2(response.rect.right() + 4.0, response.rect.center().y),
            egui::vec2(8.0, 8.0),
        );
        paint_sort_arrow(
            ui,
            arrow_rect,
            sort.descending,
            ui.visuals().strong_text_color(),
        );
    }
    response
}

/// Папка, в которой должен лежать ассет по его пути внутри Assets.
pub(crate) fn asset_directory(assets_path: &std::path::Path, asset_path: &str) -> std::path::PathBuf {
    let normalized = asset_path.replace('/', "\\");
    let relative = std::path::Path::new(&normalized);
    match relative.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => assets_path.join(parent),
        _ => assets_path.to_path_buf(),
    }
}

/// Ближайшая существующая папка по пути: если нужной папки нет, поднимаемся выше.
pub(crate) fn existing_directory_upwards(path: &std::path::Path) -> Option<std::path::PathBuf> {
    let mut current = Some(path);
    while let Some(candidate) = current {
        if candidate.is_dir() {
            return Some(candidate.to_path_buf());
        }
        current = candidate.parent();
    }
    None
}

/// Папка, которую открывают по двойному клику на ассете.
pub(crate) fn asset_folder_to_open(
    assets_root: &std::path::Path,
    asset_path: &str,
) -> Option<std::path::PathBuf> {
    existing_directory_upwards(&asset_directory(assets_root, asset_path))
}

/// Обрезает путь ассета слева, оставляя имя файла: длинные пути не помещаются
/// в столбец, а имя файла в нём важнее папок.
///
/// Возвращает обрезанный текст и признак того, что путь не поместился целиком:
/// только в этом случае показываем подсказку с полным названием ассета.
pub(crate) fn elide_asset_path(ui: &egui::Ui, path: &str, max_width: f32) -> (String, bool) {
    let font_id = egui::FontId::proportional(ui.text_style_height(&egui::TextStyle::Body));
    let width = |text: &str| -> f32 {
        ui.painter()
            .layout_no_wrap(
                text.to_owned(),
                font_id.clone(),
                ui.visuals().text_color(),
            )
            .size()
            .x
    };
    let characters: Vec<char> = path.chars().collect();
    // Ширина считается по факту: раскладка текста в egui кэшируется, поэтому
    // повторные измерения одних и тех же строк почти ничего не стоят.
    if width(path) <= max_width {
        return (path.to_owned(), false);
    }
    let mut best = 0usize;
    let mut low = 1usize;
    let mut high = characters.len();
    while low <= high {
        let middle = (low + high) / 2;
        let candidate: String = std::iter::once('…')
            .chain(characters[characters.len() - middle..].iter().copied())
            .collect();
        if width(&candidate) <= max_width {
            best = middle;
            low = middle + 1;
        } else {
            high = middle.saturating_sub(1);
        }
    }
    if best == 0 {
        // Даже имя файла не помещается: показываем его целиком, текст обрежет
        // сам виджет.
        return (path.rsplit('/').next().unwrap_or(path).to_owned(), true);
    }
    let text: String = std::iter::once('…')
        .chain(characters[characters.len() - best..].iter().copied())
        .collect();
    (text, true)
}
