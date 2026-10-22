//! Диалоги подтверждения: транслитерация ассета и замена символов.

use super::*;

/// Что выбрал пользователь в окне подтверждения.
pub(crate) enum RepairDecision {
    /// Запустить транслитерацию.
    Confirm,
    /// Отказаться.
    Cancel,
}

pub(crate) fn umlaut_repair_window(
    ui: &egui::Ui,
    asset: &UmlautAsset,
    fixes: &mut CharacterFixes,
) -> Option<RepairDecision> {
    let mut decision = None;
    egui::Window::new("Подтверждение транслитерации ассета")
        .collapsible(false)
        .resizable(true)
        .default_size([560.0, 320.0])
        .show(ui.ctx(), |ui| {
            ui.label(format!("Ассет: {}", asset.path));
            ui.small(format!("Тип: {}", asset_kind_name(asset.kind)));
            ui.label(
                "Будут транслитерированы имена и текст внутри BIN, связанные GeoPcDx и ссылки на TgPcDx.",
            );
            ui.label(
                "Папки Provider/Product автоматически не переименовываются: это затронуло бы другие ассеты. Вложенные папки переименовываются вместе с файлами и ссылками на них.",
            );
            ui.label(
                "Перед установкой проверяются конфликты имён. Для каждого изменённого файла сохраняется исходник с суффиксом .bak1, .bak2 и далее.",
            );
            ui.label("Обнаружения:");
            for evidence in &asset.evidence {
                ui.small(evidence);
            }
            character_fix_inputs(ui, &asset.unsupported, fixes);
            ui.horizontal(|ui| {
                if ui.button("Транслитерировать").clicked() {
                    decision = Some(RepairDecision::Confirm);
                }
                if ui.button("Отмена").clicked() {
                    decision = Some(RepairDecision::Cancel);
                }
            });
        });
    decision
}

/// Поля ввода замен символов, которые игра не умеет загружать.
///
/// Умляуты и французские буквы переводятся по правилам автоматически, а для
/// остальных символов пользователь сам выбирает замену — обычно это обычная
/// английская буква. Поле оставляется пустым: вводимый символ сразу заменяет
/// значение по умолчанию, которое показано подсказкой.
pub(crate) fn character_fix_inputs(
    ui: &mut egui::Ui,
    characters: &[char],
    fixes: &mut CharacterFixes,
) {
    if characters.is_empty() {
        return;
    }
    ui.separator();
    ui.label(
        "Символы, которые игра не умеет загружать. Укажите, чем их заменить: обычно это буква с английской раскладки.",
    );
    for character in characters {
        ui.horizontal(|ui| {
            ui.label(format!("U+{:04X}", *character as u32));
            ui.label("->");
            let mut value = fixes.get(character).cloned().unwrap_or_default();
            let response = ui.add(
                egui::TextEdit::singleline(&mut value)
                    .id(ui.make_persistent_id(("character_fix", *character)))
                    .hint_text(default_replacement(*character))
                    .char_limit(8)
                    .desired_width(120.0)
                    .min_size(egui::vec2(120.0, 24.0)),
            );
            if response.changed() {
                if value.is_empty() {
                    fixes.remove(character);
                } else {
                    fixes.insert(*character, value);
                }
            }
            // Кнопка сразу подставляет значение по умолчанию.
            let default = default_replacement(*character);
            if ui.small_button(default.clone()).clicked() {
                fixes.insert(*character, default);
            }
            ui.small(describe_character(*character));
        });
    }
}

