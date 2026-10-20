use eframe::egui;
use image::GenericImageView;
use rayon::prelude::*;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use tempfile::tempdir;
use walkdir::WalkDir;
use zip::ZipArchive;

const DEFAULT_CONCURRENT_SERZ: usize = 4;
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
    route_files: usize,
    converted_bin_files: usize,
    referenced_assets: usize,
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
    evidence: Vec<String>,
}

#[derive(Clone)]
struct AssetUse {
    source_file: PathBuf,
    tile: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AssetKind {
    Loft,
    Scenery,
    Other,
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
    original_files: Vec<PathBuf>,
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
    source_file: PathBuf,
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

mod app;
mod assets;
mod editor;
mod routes;
mod settings;
mod windows;

pub(crate) use assets::*;
pub(crate) use editor::*;
pub(crate) use routes::*;
pub(crate) use settings::*;
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

fn main() -> eframe::Result<()> {
    let startup_asset = parse_asset_launch_args(std::env::args_os().skip(1));
    eframe::run_native(
        "TS Checker",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([1440.0, 900.0])
                .with_min_inner_size([1100.0, 700.0]),
            ..eframe::NativeOptions::default()
        },
        Box::new(move |_creation_context| {
            let app = match startup_asset {
                Ok(Some(path)) => app::CheckerApp::asset_only(path),
                Ok(None) => app::CheckerApp::default(),
                Err(error) => app::CheckerApp::asset_only_error(error),
            };
            Ok(Box::new(app))
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::{
        decode_xml_text, extract_asset_references, extract_scenario_clues, fallback_serz_path,
        inspect_route_folder, is_skipped_route_scan_directory, is_valid_game_path,
        parse_asset_launch_args, parse_route_name, preferred_serz_path, scan_routes,
        scan_routes_with_ignored, validate_serz_process, ScenarioClueCollection,
    };
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
        fs::create_dir_all(&incomplete.join("Scenery")).unwrap();
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
                zip::write::FileOptions::default(),
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

        let error =
            super::move_folders_to_recycle_bin(&routes_root, &[outside.clone()]).unwrap_err();

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
        let options = zip::write::FileOptions::default();
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
}
