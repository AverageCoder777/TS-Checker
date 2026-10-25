//! Сценарии маршрута: папки в Scenarios и их содержимое.
//!
//! Метаданные сценария лежат в ScenarioProperties.xml, а составы техники — в
//! бинарном Scenario.bin, который приходится конвертировать через Serz.
//! Составы загружаются лениво: только при открытии окна конкретного сценария.

use std::fs;
use std::path::{Path, PathBuf};

use crate::translit::decode_xml_text;

/// Вид состава: игрок, ИИ или статический (loose).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum ConsistKind {
    #[default]
    Loose,
    Player,
    Ai,
}

/// Псевдонимы для совместимости с возможными именами в тестах.
pub(crate) type ConsistType = ConsistKind;
pub(crate) type ScenarioConsistKind = ConsistKind;

/// Сценарий маршрута: папка и данные из её файлов.
#[derive(Clone, Default)]
pub(crate) struct RouteScenario {
    /// Папка сценария.
    pub(crate) path: PathBuf,
    /// Название сценария.
    pub(crate) name: String,
    /// Дата сценария.
    pub(crate) date: String,
    /// Автор сценария.
    pub(crate) author: String,
    /// Сезон.
    pub(crate) season: String,
    /// Длительность в минутах.
    pub(crate) duration: String,
    /// Время начала суток.
    pub(crate) start_time: String,
    /// Состояние сценария.
    pub(crate) state: String,
    /// Состав игрока.
    pub(crate) player_consist: String,
    /// Признак наличия состава игрока.
    pub(crate) player: bool,
    pub(crate) has_player: bool,
    pub(crate) player_flag: bool,
    pub(crate) is_player_scenario: bool,
    /// Подвижные составы сценария: в них перечислена техника.
    /// `None` пока не открыто окно сценария (загружается лениво).
    pub(crate) consists: Option<Vec<ScenarioConsist>>,
}

/// Подвижной состав сценария.
#[derive(Clone, Default)]
pub(crate) struct ScenarioConsist {
    /// Название состава (рейс).
    pub(crate) name: String,
    /// Единицы техники в составе.
    pub(crate) vehicles: Vec<ScenarioVehicle>,
    /// Признак состава игрока.
    pub(crate) is_player: bool,
    pub(crate) player: bool,
    pub(crate) player_driver: bool,
    /// Признак состава ИИ.
    pub(crate) ai: bool,
    /// Признак статического состава без водителя.
    pub(crate) loose: bool,
    /// Вид состава.
    pub(crate) kind: ConsistKind,
    pub(crate) consist_kind: ConsistKind,
    pub(crate) kind_type: ConsistKind,
}

/// Находит позицию открывающего тега элемента (с учётом атрибутов).
fn find_tag(xml: &str, tag: &str) -> Option<usize> {
    let open = format!("<{}", tag);
    let mut pos = 0;
    while let Some(found) = xml[pos..].find(&open) {
        let abs = pos + found;
        let after = &xml[abs + open.len()..];
        if after.starts_with(' ') || after.starts_with('>') {
            return Some(abs);
        }
        pos = abs + open.len();
    }
    None
}

/// Название состава из cDriver/ServiceName/Localisation-cUserLocalisedString.
/// Берётся первый непустой элемент, не Key и не Other. Если состав без
/// водителя (Loose Consist) или имя не найдено — "Неназванный состав".
fn consist_name(consist: &str) -> String {
    let driver_start = match find_tag(consist, "Driver") {
        Some(pos) => pos,
        None => return "Неназванный состав".to_owned(),
    };
    let driver_rest = &consist[driver_start..];
    let driver_end = driver_rest.find("</Driver>").unwrap_or(driver_rest.len());
    let driver_block = &driver_rest[..driver_end];

    let cdriver_start = match find_tag(driver_block, "cDriver") {
        Some(pos) => pos,
        None => return "Неназванный состав".to_owned(),
    };
    let cdriver_rest = &driver_block[cdriver_start..];
    let cdriver_end = cdriver_rest
        .find("</cDriver>")
        .unwrap_or(cdriver_rest.len());
    let cdriver_block = &cdriver_rest[..cdriver_end];

    let sn_start = match find_tag(cdriver_block, "ServiceName") {
        Some(pos) => pos,
        None => return "Неназванный состав".to_owned(),
    };
    let sn_rest = &cdriver_block[sn_start..];
    let sn_end = sn_rest.find("</ServiceName>").unwrap_or(sn_rest.len());
    let sn_block = &sn_rest[..sn_end];

    let loc_start = match find_tag(sn_block, "Localisation-cUserLocalisedString") {
        Some(pos) => pos,
        None => return "Неназванный состав".to_owned(),
    };
    let loc_rest = &sn_block[loc_start..];
    let loc_end = loc_rest
        .find("</Localisation-cUserLocalisedString>")
        .unwrap_or(loc_rest.len());
    let loc_block = &loc_rest[..loc_end];

    let mut pos = 0;
    while let Some(tag_start) = loc_block[pos..].find('<') {
        let abs = pos + tag_start;
        let after = &loc_block[abs..];
        if after.starts_with("</") {
            if let Some(gt) = after.find('>') {
                pos = abs + gt + 1;
                continue;
            } else {
                break;
            }
        }
        let tag_end = match after.find('>') {
            Some(i) => i,
            None => break,
        };
        let tag_name = &after[1..tag_end];
        let tag_name = tag_name.split_whitespace().next().unwrap_or(tag_name);
        if tag_name == "Key" || tag_name == "Other" {
            let close_tag = format!("</{}>", tag_name);
            if let Some(ct) = loc_block[abs..].find(&close_tag) {
                pos = abs + ct + close_tag.len();
                continue;
            } else {
                break;
            }
        }
        let value_start = abs + tag_end + 1;
        let value_end = loc_block[value_start..]
            .find('<')
            .map(|i| value_start + i)
            .unwrap_or(loc_block.len());
        let value = decode_xml_text(&loc_block[value_start..value_end])
            .trim()
            .to_owned();
        if !value.is_empty() {
            return value;
        }
        pos = value_end;
    }

    "Неназванный состав".to_owned()
}

/// Определяет вид состава по содержимому cConsist.
fn consist_kind(consist: &str) -> ConsistKind {
    let Some(driver_start) = find_tag(consist, "Driver") else {
        return ConsistKind::Loose;
    };
    let driver_rest = &consist[driver_start..];
    let driver_end = driver_rest.find("</Driver>").unwrap_or(driver_rest.len());
    let driver_block = &driver_rest[..driver_end];

    let Some(cdriver_start) = find_tag(driver_block, "cDriver") else {
        return ConsistKind::Loose;
    };
    let cdriver_rest = &driver_block[cdriver_start..];
    let cdriver_end = cdriver_rest
        .find("</cDriver>")
        .unwrap_or(cdriver_rest.len());
    let cdriver_block = &cdriver_rest[..cdriver_end];

    if has_player_driver(cdriver_block) {
        ConsistKind::Player
    } else {
        ConsistKind::Ai
    }
}

/// Проверяет наличие флага PlayerDriver со значением 1/true.
fn has_player_driver(block: &str) -> bool {
    let Some(pos) = block.find("<PlayerDriver") else {
        return false;
    };
    let rest = &block[pos..];
    let end = rest.find("</PlayerDriver>").unwrap_or(rest.len());
    let tag_end = rest.find('>').unwrap_or(rest.len());
    let value = rest[tag_end + 1..end].trim();
    value == "1" || value.eq_ignore_ascii_case("true")
}

/// Единица подвижного состава: локомотив или вагон.
#[derive(Clone, Default)]
pub(crate) struct ScenarioVehicle {
    /// Разработчик дополнения.
    pub(crate) provider: String,
    /// Дополнение.
    pub(crate) product: String,
    /// Путь к файлу модели.
    pub(crate) blueprint: String,
    /// Отображаемое название модели.
    pub(crate) name: String,
}

/// Читает сценарии маршрута: папки в Scenarios и их файлы.
pub(crate) fn read_route_scenarios(route_path: &Path, _serz_path: &Path) -> Vec<RouteScenario> {
    let scenarios_path = route_path.join("Scenarios");
    let Ok(entries) = fs::read_dir(&scenarios_path) else {
        return Vec::new();
    };
    let mut scenarios = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let id = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let has_player = read_scenario_file(&path, _serz_path)
            .map(|xml| has_player_driver(&xml))
            .unwrap_or(false);
        let mut scenario = RouteScenario {
            path: path.clone(),
            player: has_player,
            has_player,
            player_flag: has_player,
            is_player_scenario: has_player,
            ..RouteScenario::default()
        };
        if let Ok(xml) = fs::read_to_string(path.join("ScenarioProperties.xml")) {
            scenario.name = localised_text(&xml, "DisplayName");
            if scenario.name.is_empty() {
                scenario.name = id.clone();
            }
            scenario.author = element_text(&xml, "Author");
            scenario.season = match element_text(&xml, "Season").as_str() {
                "SEASON_SPRING" => "Spring".to_owned(),
                "SEASON_SUMMER" => "Summer".to_owned(),
                "SEASON_AUTUMN" => "Autumn".to_owned(),
                "SEASON_WINTER" => "Winter".to_owned(),
                other => other.to_owned(),
            };
            scenario.duration = element_text(&xml, "DurationMins");
            scenario.start_time = scenario_start_time(&xml);
            scenario.state = element_text(&xml, "StateName");
            scenario.player_consist = element_text(&xml, "QuickDrivePlayerConsistName");
            scenario.date = scenario_date(&xml);
        } else {
            scenario.name = id.clone();
        }
        scenarios.push(scenario);
    }
    scenarios.sort_by(|left, right| left.name.cmp(&right.name));
    scenarios
}

/// Загружает составы техники из Scenario.bin сценария (через Serz в XML).
pub(crate) fn load_consists(scenario_path: &Path, serz_path: &Path) -> Vec<ScenarioConsist> {
    let Some(xml) = read_scenario_file(scenario_path, serz_path) else {
        return Vec::new();
    };
    scenario_consists(&xml)
}

/// Читает файл сценария с составами: он может быть XML или бинарным.
pub(crate) fn read_scenario_file(scenario_path: &Path, serz_path: &Path) -> Option<String> {
    let binary = scenario_path.join("Scenario.bin");
    if binary.is_file() {
        if let Ok(xml) = crate::assets::read_serz_xml(serz_path, &binary) {
            return Some(xml);
        }
    }
    let text = scenario_path.join("Scenario.xml");
    if text.is_file() {
        return fs::read_to_string(text).ok();
    }
    None
}

/// Составы техники из файла сценария.
pub(crate) fn scenario_consists(xml: &str) -> Vec<ScenarioConsist> {
    let mut consists = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find("<cConsist") {
        rest = &rest[start..];
        let end = rest.find("</cConsist>").unwrap_or(rest.len());
        let consist = &rest[..end];
        let name = consist_name(consist);
        let vehicles = scenario_vehicles(consist);
        if !vehicles.is_empty() {
            let kind = consist_kind(consist);
            let is_player = matches!(kind, ConsistKind::Player);
            let ai = matches!(kind, ConsistKind::Ai);
            let loose = matches!(kind, ConsistKind::Loose);
            consists.push(ScenarioConsist {
                name,
                vehicles,
                is_player,
                player: is_player,
                player_driver: is_player,
                ai,
                loose,
                kind,
                consist_kind: kind,
                kind_type: kind,
            });
        }
        rest = &rest[end..];
    }
    consists
}

/// Единицы техники в составе.
pub(crate) fn scenario_vehicles(consist: &str) -> Vec<ScenarioVehicle> {
    let mut vehicles = Vec::new();
    let mut rest = consist;
    while let Some(start) = rest.find("<cOwnedEntity") {
        rest = &rest[start..];
        let end = rest.find("</cOwnedEntity>").unwrap_or(rest.len());
        if let Some(vehicle) = scenario_vehicle(&rest[..end]) {
            vehicles.push(vehicle);
        }
        rest = &rest[end..];
    }
    vehicles
}

/// Данные об одной единице техники: файл модели и её название.
pub(crate) fn scenario_vehicle(entity: &str) -> Option<ScenarioVehicle> {
    let start = entity.find("<BlueprintID>")?;
    let end = entity[start..].find("<ReskinBlueprintID>")? + start;
    let block = absolute_blueprint_block(&entity[start..end])?;
    let provider = element_text(block, "Provider");
    let product = element_text(block, "Product");
    let blueprint = element_text(block, "BlueprintID");
    let name = element_text(entity, "Name");
    if blueprint.is_empty() && name.is_empty() {
        return None;
    }
    Some(ScenarioVehicle {
        provider,
        product,
        blueprint,
        name,
    })
}

/// Блок с абсолютной ссылкой на блюпринт.
pub(crate) fn absolute_blueprint_block(text: &str) -> Option<&str> {
    let start = text.find("<iBlueprintLibrary-cAbsoluteBlueprintID>")?;
    let end = text[start..].find("</iBlueprintLibrary-cAbsoluteBlueprintID>")? + start;
    Some(&text[start..end])
}
/// Текст элемента: содержимое между открывающим и закрывающим тегом.
pub(crate) fn element_text(xml: &str, element: &str) -> String {
    let open = format!("<{element}");
    let mut search_from = 0;
    while let Some(pos) = xml[search_from..].find(&open) {
        let abs = search_from + pos;
        let after = &xml[abs + open.len()..];
        if after.starts_with(' ') || after.starts_with('>') || after.starts_with('/') {
            let rest = &xml[abs..];
            let Some(close) = rest.find(&format!("</{element}>")) else {
                return String::new();
            };
            let value_start = rest.find('>').map(|index| index + 1).unwrap_or(0);
            let value = &rest[value_start..close];
            let text = match value.find('<') {
                Some(index) => &value[..index],
                None => value,
            };
            return decode_xml_text(text).trim().to_owned();
        }
        search_from = abs + open.len();
    }
    String::new()
}

/// Текст локализованного элемента: первый <English> внутри блока.
/// Предпочитает English, но при пустом значении берёт первый непустой
/// из остальных языков.
fn localised_text(xml: &str, block: &str) -> String {
    let open = format!("<{block}");
    let mut search_from = 0;
    while let Some(pos) = xml[search_from..].find(&open) {
        let abs = search_from + pos;
        let after = &xml[abs + open.len()..];
        if after.starts_with(' ') || after.starts_with('>') {
            let rest = &xml[abs..];
            let close_tag = format!("</{block}>");
            let end = rest.find(&close_tag).unwrap_or(rest.len());
            let block_xml = &rest[..end];
            // Сначала пробуем English.
            let english = element_text(block_xml, "English");
            if !english.is_empty() {
                return english;
            }
            // English пуст — берём первый непустой из остальных языков.
            for lang in [
                "German", "French", "Italian", "Spanish", "Dutch", "Polish", "Russian",
            ] {
                let val = element_text(block_xml, lang);
                if !val.is_empty() {
                    return val;
                }
            }
            return String::new();
        }
        search_from = abs + open.len();
    }
    String::new()
}

/// Время начала сценария: в файле оно в секундах от полуночи.
fn scenario_start_time(xml: &str) -> String {
    let seconds: i64 = element_text(xml, "StartTime").parse().unwrap_or_default();
    if seconds <= 0 {
        return String::new();
    }
    format!("{:02}:{:02}", seconds / 3600, (seconds % 3600) / 60)
}

/// Дата сценария из года, месяца и дня.
fn scenario_date(xml: &str) -> String {
    let day: u32 = element_text(xml, "StartDD").parse().unwrap_or(0);
    let month: u32 = element_text(xml, "StartMM").parse().unwrap_or(0);
    let year: u32 = element_text(xml, "StartYYYY").parse().unwrap_or(0);
    if day == 0 || month == 0 || year == 0 {
        return String::new();
    }
    format!("{day:02}.{month:02}.{year}")
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore]
    fn reads_real_route_scenarios() {
        let route = PathBuf::from(
            r"X:\SteamLibrary\steamapps\common\RailWorks\Content\Routes\00000093-0000-0000-0000-000000002019",
        );
        let serz = PathBuf::from(r"X:\SteamLibrary\steamapps\common\RailWorks\serz.exe");
        if !route.is_dir() || !serz.is_file() {
            return;
        }
        let scenarios = read_route_scenarios(&route, &serz);
        for scenario in &scenarios {
            let consists = load_consists(&scenario.path, &serz);
            eprintln!(
                "SCENARIO имя={:?} дата={:?} автор={:?} сезон={:?} составов={}",
                scenario.name,
                scenario.date,
                scenario.author,
                scenario.season,
                consists.len(),
            );
            for consist in &consists {
                eprintln!(
                    "  состав name={:?} единиц={}",
                    consist.name,
                    consist.vehicles.len()
                );
            }
            break;
        }
        assert!(!scenarios.is_empty(), "сценарии должны находиться");
    }
    use crate::assets::check_route;

#[test]
fn checks_real_route_x() {
    let rw_path = PathBuf::from(r"X:\SteamLibrary\steamapps\common\RailWorks");
    let route = rw_path.join(r"Content\Routes\1f8f9c40-10bb-4310-ab5c-3a6a74893eb1");
    let serz = rw_path.join(r"serz.exe");
    if !route.is_dir() || !serz.is_file() {
        return;
    }
    let result = check_route(&route, &rw_path.join("Assets"), &serz, false, 1, |_| {});
    match result {
        Ok(r) => {
            println!("missing: {}, errors: {}, structure: {}",
                r.missing_assets.len(), r.conversion_errors.len(), r.structure_issues.len());
            if let Some(last) = r.conversion_errors.last() {
                println!("последняя ошибка: {last}");
            }
        }
        Err(e) => panic!("{e}"),
    }
}
}
