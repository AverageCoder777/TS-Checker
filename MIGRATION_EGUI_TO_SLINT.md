TS-Checker: план миграции UI с egui/eframe на slint 1.18.1
Файл предназначен для передачи контекста между сессиями. Содержит детальный
план целей и задач для выполнения миграции. Сама миграция НЕ начата — это только
список целей для последующих сессий.

Ключевые решения пользователя:
- Главное приоритет: событийная модель (перерисовка только по событиям),
  absence постоянной перерисовки каждый кадр, рабочая программа.
- Целевая библиотека: slint 1.18.1.
- Изменения в код стоит вносить в дублированные файлы, пока не завершится вся переработка кода. Не нужно изменять основные рабочие файлы, так как переработка будет происходить в несколько сессий и итераций нейросетей.

1. ТЕКУЩЕЕ СОСТОЯНИЕ (что есть)
Язык: Rust, edition 2026.
Сборка: Cargo, один бинарный крейт ts_checker_rs.
Главный фреймворк UI: eframe 0.37 + egui 0.37 + egui_extras 0.37.
Точка входа: src/main.rs — eframe::run_native(...), окно 1440x900, мин. 1100x700.
Состояние приложения: struct CheckerApp в src/app.rs, реализует eframe::App.
Все UI отрисовываются в CheckerApp::ui(...) каждый кадр (immediate mode).
Есть request_repaint_after(100мс) во время фоновых операций (app.rs:1601-1603).

Backend/бизнес-логика (НЕ зависит от UI, не меняется при миграции):
- src/assets.rs — Serz конвертация, разбор ссылок на ассеты, сканирование маршрутов.
- src/reference.rs — парсинг XML ссылок на блюпринты.
- src/blueprint.rs — разрешение путей блюпринтов на диске (кэш в thread_local).
- src/scenario.rs — чтение сценариев маршрута, загрузка составов через Serz.
- src/translit.rs — транслитерация умляутов, работа с CharacterFixes.
- src/routes.rs — обнаружение маршрутов, иконки, кэш сканирования.
- src/analysis_cache.rs — кэш разбора файлов маршрута на диске.
- src/settings.rs — постоянные настройки (APPDATA/TS-Checker).
- src/windows.rs — Windows Shell (Корзина, Проводник).

eframe-зависимости (требуют замены при миграции на slint):
- Cargo.toml: eframe 0.37, egui_extras 0.37.
- src/main.rs: eframe::run_native, egui::ViewportBuilder, NativeOptions.
- src/app.rs: impl eframe::App, fn ui(&mut self, ui: &mut egui::Ui, ...).
- Хранение текстур иконок маршрутов: egui::TextureHandle / egui::TextureId.
- Кастомная отрисовка таблицы инвентаря: egui_extras::TableBuilder, Column.
- Синтаксис XML-редактора: egui::text::LayoutJob, egui::TextFormat.

2. СОСТОЯНИЕ ПРИЛОЖЕНИЯ (CheckerApp, src/app.rs:64-124)
Поля, связанные с UI (нужно будет переписать под slint):
- route_icon_textures: HashMap<PathBuf, egui::TextureHandle> — текстуры иконок.
- attempted_route_icons: HashSet<PathBuf> — уже попытки загрузки иконок.
- asset_inventory_state: Arc<Mutex<AssetInventoryWindowState>> — состояние окна инвентаря.
- inventory_action_tx / inventory_action_rx — канал действий инвентаря.
- route_scan, check, asset_replacement, umlaut_repair, inventory_scan,
  asset_viewer_load, asset_viewer_save, delete_rx — mpsc Receiver для фоновых результатов.
- settings_window_open, asset_viewer_open, asset_editor_only — флаги окон.
- scenario_window_open, incomplete_window_open, ignore_list_window_open — флаги окон.
- selected_route, selected_incomplete, selected_scenario, selected_consist — индексы выделения.
- search_query — строка поиска маршрутов.
- rw_path — путь к RailWorks.
- status — строка статуса в нижней панели.
- character_fixes: CharacterFixes — пользовательские замены символов.
- pending_umlaut_repair, pending_asset_replacement — ожидающие подтверждения действия.
- route_thumbnails_enabled, serz_process_limit, locoswap_path — настройки.
- ignored_route_ids: HashSet<String> — игнор-лист маршрутов.

Поля, НЕ связанные с UI (остаются без изменений):
- routes, incomplete_folders, scenarios, result, ignored_route_ids и т.д. — данные.

3. ФАЙЛЫ С ВЫЗОВАМI EGUI И ЧТО НУЖНО ПЕРЕДЕЛАТЬ

3.1 src/main.rs (точка входа)
  Строка 3:   use eframe::egui;
  Строки 393-412: fn main() -> eframe::Result<()>
    Цель: заменить eframe::run_native на slint run/окно.
    - eframe::run_native(...) → slint::MainWindow / Window::run().
    - egui::ViewportBuilder ... → настройка размера окна в .slint или через slint API.
    - Box::new(move |_creation_context| { ... Ok(Box::new(app)) }) →
      создание главного окна slint с передачей состояния приложения.

3.2 src/app.rs (основное приложение, ~3247 строк)
  Строка 1: //! egui application state, windows, and user interactions.
  Строка 1579: impl eframe::App for CheckerApp
  Строка 1580: fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame)
    Цель: убрать реализацию eframe::App, метод ui() полностью переделать
    под slint. Всё immediate-mode отрисование заменить на slint компоненты.
    - Убрать request_repaint_after(Duration::from_millis(100)) — не нужен в slint
      (событийная модель, перерисовка только при изменении состояния).
    - Все egui::Window::new(...) заменить на slint окна/диалоги.
    - Все egui::Button, Checkbox, Slider, TextEdit, Label, ScrollArea,
      CollapsingHeader, Image, Panel, CentralPanel → slint эквиваленты.
    - egui::Color32::* → slint цвета.
    - egui::RichText → slint текстовое форматирование.

  app::show_asset_editor() (строка 744):
    Цель: перенести XML-редактор на slint TextEdit без подсветки синтаксиса
    (подсветку добавить позже, отдельной задачей).
    - egui::Panel::bottom, egui::CentralPanel, egui::Window → slint layout.
    - egui::TextEdit::multiline с кастомным layouter (xml_layout_job) →
      slint TextEdit (обычный, без подсветки).
    - src/editor.rs:225 xml_layout_job — не используется в erste фазе.

  app::show_asset_inventory_viewport() (строка 209):
    Цель: перенести второе окно инвентаря на slint отдельное окно.
    - egui::ViewportBuilder, ctx.show_viewport_deferred → slint::Window.

  app::route_icon_texture_id() (строка 791):
    Цель: заменить egui::TextureHandle на slint Image/Bitmap.
    - egui::Context::load_texture, egui::ColorImage → slint::Image::load_from_file
      или slint::Bitmap через image crate.
    - HashMap<PathBuf, egui::TextureHandle> → кэш slint изображений.

3.3 src/inventory_ui.rs (таблица инвентаря, 660 строк)
  Цель: перенести таблицу инвентаря на slint TableView/Grid без автоподгонки
  столбцов (не нужна).
  - InventoryRowActions с path_rects / usage_rects: egui::Rect (только в тестах) → убрать.
  - TABLE_ID_SALT, InventorySortColumn, InventorySort — логика сортировки остаётся.
  - InventoryLayout — расчёт ширин столбцов остаётся (без автоподгонки).
  - measure_small_text, paint_cell_text, FittedLabel (impl egui::Widget),
    split_tiles_for_cell, paint_sort_arrow, inventory_header_cell,
    asset_inventory_table — полностью переписать на slint TableView/Grid.
  - Тесты в файле (#[cfg(test)]) с egui::Rect — переписать или убрать.

3.4 src/dialogs.rs (окно подтверждения транслитерации, 99 строк)
  Цель: перенести диалоги на slint.
  - umlaut_repair_window(ui: &egui::Ui, ...) → slint диалог.
  - character_fix_inputs(ui: &mut egui::Ui, ...) → slint TextEdit + Button.

3.5 src/editor.rs (XML редактор, 712 строк)
  Цель: в eerste фазе НЕ трогать логику редактора, только заменить отрисовку
  на slint TextEdit без подсветки.
  - xml_layout_job(ui: &egui::Ui, ...) -> egui::text::LayoutJob — не используется
    в erste фазе (подсветка отложена).
  - render_asset_editor (в app.rs:1411) — переписать на slint TextEdit.

3.6 src/assets.rs (тесты, строки ~2874-3763)
  Цель: переписать или убрать headless egui тесты таблицы инвентаря.
  - Тесты отрисовывают таблицу в headless egui контексте (egui::Context::default(),
    egui::RawInput, CentralPanel) → переписать под slint или убрать, если
    проверяют только UI-отрисовку.

4. СЛОЖНЫЕ МЕСТА И РИСКИ
4.1 XML редактор без подсветки синтаксиса (src/editor.rs, app.rs:1411-1577)
    slint не имеет встроенного код-редактора с подсветкой. В primeira фазе
    использовать обычный slint TextEdit без подсветки. Подсветку добавить
    позже отдельной задачей (CustomComponent или другой подход).

4.2 Таблица инвентаря без автоподгонки столбцов
    slint TableView не поддерживает resize столбцов двойным кликом из коробки.
    Автоподгонка не нужна — использовать авто-ширины.

4.3 Текстур иконок маршрутов (app.rs:791-823)
    egui::TextureHandle из egui::ColorImage → slint::Image или slint::Bitmap.
    Загрузка PNG/JPG через image crate, отдача в slint Image.

4.4 Второе окно инвентаря через ctx.show_viewport_deferred (app.rs:230)
    В slint — отдельное окно (slint::Window).

4.5 egui::Widget трейт (FittedLabel в inventory_ui.rs:160)
    Кастомный виджет с интеракцией → slint CustomComponent или встроенные
    элементы с обработкой кликов.

5. МОДЕЛЬ СОБЫТИЙ (событийная перерисовка, не immediate mode)
egui — immediate mode: весь UI перестраивается каждый кадр, даже без изменений.
slint — retained mode (событийный):
- Перерисовка только при изменении свойств компонентов или явном вызове redraw.
- Нет необходимости в request_repaint / request_repaint_after — достаточно
  обновить модель slint, и slint сам перерисует изменённые области.
- Фоновые потоки (mpsc каналы в app.rs) должны вызывать
  slint::invoke_from_event_loop для обновления состояния UI из потока.
- Все callbacks из фоновых потоков оборачивать в
  slint::invoke_from_event_loop(|| { ... }), иначе паника.

Что нужно сделать при миграции:
1. Убрать все вызовы request_repaint / request_repaint_after.
2. Все обновления состояния из фоновых потоков wrapping в invoke_from_event_loop.
3. Модель данных перенести в slint::Model (VecModel, RcModel и т.п.) или в
   глобальное состояние через slint::GlobalState / общие структуры с
   slint::ComponentHandle.

6. ИЗМЕНЕНИЯ В CARGO.TOML
Убрать:
- eframe = { version = "0.37", ... }
- egui_extras = { version = "0.37", ... }

Добавить:
- slint = "1.18.1"
- slint-build (build-dependency)

Пример build.rs:
```rust
fn main() {
    slint_build::compile("ui/app.slint").unwrap();
}
```

7. СТРУКТУРА .SLINT ФАЙЛОВ (предлагаемая)
ui/app.slint — главное окно:
  - Список маршрутов слева (ListView с иконками)
  - Список сценариев под маршрутами
  - Панель проверки справа (кнопки, прогресс, результаты)
  - Статусная панель внизу

ui/asset_inventory.slint — окно инвентаря ассетов (TableView)
ui/scenario_window.slint — окно сценария (составы, техника)
ui/settings_window.slint — настройки (потоки Serz, путь к LocoSwap, миниатюры)
ui/asset_editor.slint — редактор XML (TextEdit, поиск/замена, без подсветки)
ui/dialogs.slint — диалоги подтверждения (замена ассета, транслитерация, удаление)

8. ПОРЯДОК МИГРАЦИЯ (рекомендуемые цели для сессий)
Цель 1. Подготовка зависимостей и скелета.
  - ✅ Добавить slint = "1.18.1" и slint-build в Cargo.toml.
  - ✅ Убрать eframe, egui_extras из Cargo.toml.
  - ✅ Создать build.rs с slint_build::compile.
  - ✅ Создать ui/ дирректорию и первый .slint файл. (ui/app.slint)

Цель 2. Переписать main.rs.
  - ✅ Убрать eframe::run_native, egui::ViewportBuilder.
  - ✅ Создать главное окно slint.
  - ✅ Проверить: cargo build проходит (пустое окно или заглушка).

Цель 3. Перенести состояние CheckerApp.
  - ✅ Убрать egui::TextureHandle, egui::TextureId из полей CheckerApp.
  - ✅ Убрать impl eframe::App и метод ui().
  - ✅ Сделать состояние доступным для slint окон (через Arc<Mutex> или
    slint::GlobalState / общие структуры).

Цель 4. Переделать главное окно (список маршрутов + панель проверки).
   - ✅ 4.1 Добавить в главное окно список маршрутов в slint ListView (иконки маршрутов загружаются через Image). Маршрут в списке выглядит как иконка + название
   - ✅ 4.2 Добавлено поле поиска маршрутов + фильтрация.
   - ✅ 4.3 Добавлено отдельное окно с Игнор-листом маршрутов и возможностью добавлять маршруты из списка в игнор-лист через кнопку.
   - ✅ 4.4 Отдельное предупреждение про неполные папки: при открытии предупреждения открывается окно со списком неполных маршрутов и возможностью выбрать неполный маршрут для анализа информации внутри него.
   - ✅ 4.5 Добавить панель проверки маршрута на главном окне: кнопки запуска/остановки и кнопку открытия маршрута в проводнике.
   - ✅ 4.6 Добавить прогресс-бар и статус проверки в панель проверки.
   - ✅ 4.7 Снизу окна добавить статусную панель.
   - ✅ 4.8 Убрать request_repaint_after.
   - ✅ 4.9 Прикрутить запуск скана маршрутов
   - ✅ 4.10 Приём результатов скана и обновление списка маршрутов в UI. и обновление списка в slint UI.

Цель 5. Окно инвентаря ассетов.
    - ✅ 5.1 slint TableView для инвентаря (колонки/заголовки). Сначала пользователь выбирает маршрут из списка, а затем у него появляется возможность нажать либо "Проверить маршрут", либо "Открыть маршрут в проводнике". После проверки маршрута также должна появляться кнопка "Показать все объекты", которая как раз открывает окно инвентаря со списком всех объектов маршрута, количества их использований и перечислением всех тайлов, на которых объект встречается.
    - ✅ 5.2 Ширины столбцов (авто, без resize).
    - ✅ 5.3 Сортировка по колонкам колонок (InventorySortColumn/InventorySort).
    - ✅ 5.4 Строка поиска по ячейкам с переходом на вхождение по циклу (как в Notepad++ или других редакторах).
   - ✅ 5.5 Окно должно быть действительно вторым (отдельным) (slint::Window вместо show_viewport_deferred). (AssetInventoryWindow::new())
   - ✅ 5.6 Ссылки на объекты должны быть кликабельны и открывать путь к этим объектам. (кнопка «Открыть выбранный объект» в инвентаре)

Цель 6. Окно сценария.
   - ✅ 6.1 Реализовать список сценариев, который открывается сразу после выбора маршрута под списком маршрутов. (scenario-items в MainWindow, refresh_scenarios при выборе)
   - ✅ 6.2 Реализовать в отдельном окне список составов сценария после двойного клика по сценарию. Проверка на наличие состава проходит 1 раз во время открытия, после чего просто отображается весь подвижной состав сценария и недостающие поезда выделяются красным, а полноценные - зеленым. 
   - ✅ 6.3 Пути к файлам должны быть кликабельны.
   - ✅ 6.4 Добавить кнопку для открытия данного сценария/состава в редакторе LocoSwap для дальнейшей замены.

Цель 7. Окно настроек.
  - ✅ Перенести все настройки со старого интерфейса (потоки Serz, путь к LocoSwap, миниатюры). (SettingsWindow в app.slint + wiring в main.rs)

Цель 8. Диалоги подтверждения действия.
  - ✅ Перенести системы: транслитерация найденных файлов/путей с битыми символами (выводится сначала список таких файлов/путей, затем отдельным окном подтверждения действия предлагается транслитерация с указанием что на что переименовывается), удаление папок с неполными маршрутами (тоже с окном подтверждения действия). Также прикрутить окно подтверждения действия к системе замены ассетов. В окне должно показываться какой ассет меняется на какой.

Цель 9. Редактор XML (без подсветки).
  - ✅ Перенести TextEdit, поиск/замену по файлу и кнопку сохранения изменений. Добавить индикатор наличия несохраненных изменений

Цель 10. Загрузка иконок маршрутов.
  - ✅ Заменить egui::TextureHandle на slint Image/Bitmap. (load_route_icon_image через image crate)

Цель 11. Фоновые потоки и invoke_from_event_loop.
  - ✅ Все mpsc каналы и фоновые результаты обернуть в invoke_from_event_loop. (фоновые потоки шлют через mpsc, опрос на UI-потоке таймером 50 мс — безопасно)

Цель 12. Тесты и финальная проверка.
  - ✅ Переписать или убрать headless egui тесты. Создать в этом файле после этого пункта
    Новые пункты: вещи, оставшиеся в коде egui, но не реализованные в slint: (нет — все вызовы и функции egui удалены окончательно) новый пункт с вещами, которые остались в коде egui, но которые не реализованы в slint. Удалить все вызовы и функции egui окончательно.
  - Полная проверка кода с тестированием производительности и предложениями по оптимизации.

9. ОЦЕНКА СМЫСЛА ПЕРЕХОДА
За:
- Событийная модель (перерисовка только по событиям) — соответствует запросу.
- Нет постоянной перерисовки каждый кадр — меньше ресурсов, нет костылей
  с request_repaint_after.
- Лучший UX для законченного десктопного приложения.

Против:
- Переход — большая переработка UI-слоя (~40% кода).
- Меньшая экосистема и меньше примеров, чем у egui.

Вывод: переход имеет смысл для законченного десктопного приложения с
событийной моделью и рабочей программой. Это основная цель миграции.

КОНЕЦ ФАЙЛА







