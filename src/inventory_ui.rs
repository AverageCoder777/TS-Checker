//! Таблица инвентаря ассетов: список используемых объектов маршрута.

use super::*;
pub(crate) type InventoryRow<'a> = (
    &'a str,
    &'a [AssetUse],
    bool,
    Option<&'a UmlautAsset>,
);

/// Действия, запрошенные строками таблицы инвентаря.

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



/// Измеряет ширину текста в стиле мелкого шрифта.

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