//! Serz conversion and asset-reference checks.

use super::*;

pub(crate) fn is_skipped_route_scan_directory(name: &str, ignore_scenarios: bool) -> bool {
    ignore_scenarios && name.eq_ignore_ascii_case("Scenarios")
}

fn is_allowed_route_directory(name: &str, ignore_scenarios: bool) -> bool {
    [
        "Terrain",
        "Scenery",
        "Scenarios",
        "RouteInfo",
        "RouteInformation",
        "Networks",
        "MixMap",
    ]
    .iter()
    .any(|allowed| name.eq_ignore_ascii_case(allowed))
        && !is_skipped_route_scan_directory(name, ignore_scenarios)
}

/// Префикс рабочих каталогов программы, которые создаются рядом с файлами маршрута
/// и ассетов. Такие каталоги не должны попадать в проверку маршрута, а после аварийного
/// завершения их легко узнать и удалить вручную.
pub(crate) const TEMP_WORK_DIRECTORY_PREFIX: &str = ".ts-checker-tmp-";

pub(crate) fn is_temporary_work_directory(name: &str) -> bool {
    name.starts_with(TEMP_WORK_DIRECTORY_PREFIX)
}

/// Создаёт рабочий каталог рядом с целевым файлом: переименование в пределах одного
/// тома обязательно для `install_converted_asset`, поэтому системный `%TEMP%` не подходит.
pub(crate) fn create_temporary_work_directory(parent: &Path) -> Result<tempfile::TempDir, String> {
    tempfile::Builder::new()
        .prefix(TEMP_WORK_DIRECTORY_PREFIX)
        .tempdir_in(parent)
        .map_err(|error| {
            format!(
                "Не удалось создать временные файлы рядом с {}: {error}",
                parent.display()
            )
        })
}

pub(crate) fn collect_route_files(
    route_path: &Path,
    ignore_scenarios: bool,
) -> Result<Vec<PathBuf>, String> {
    WalkDir::new(route_path)
        .into_iter()
        .filter_entry(|entry| {
            if is_temporary_work_directory(&entry.file_name().to_string_lossy()) {
                return false;
            }
            entry.depth() == 0
                || entry.depth() != 1
                || !entry.file_type().is_dir()
                || is_allowed_route_directory(
                    &entry.file_name().to_string_lossy(),
                    ignore_scenarios,
                )
        })
        .map(|entry| {
            entry.map_err(|error| format!("Ошибка при обходе {}: {error}", route_path.display()))
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|entries| {
            entries
                .into_iter()
                .filter(|entry| {
                    if !entry.file_type().is_file() {
                        return false;
                    }
                    let Some(relative) = entry.path().strip_prefix(route_path).ok() else {
                        return false;
                    };
                    let mut components = relative.components();
                    let Some(first) = components.next() else {
                        return false;
                    };
                    let in_route_root = components.next().is_none();
                    let in_allowed_directory = first
                        .as_os_str()
                        .to_str()
                        .is_some_and(|directory| {
                            is_allowed_route_directory(directory, ignore_scenarios)
                        });
                    (in_route_root || in_allowed_directory)
                        && entry.path().extension().is_some_and(|extension| {
                            extension.eq_ignore_ascii_case("bin")
                                || extension.eq_ignore_ascii_case("xml")
                        })
                })
                .map(|entry| entry.into_path())
                .collect()
        })
}

pub(crate) fn unexpected_route_directories(route_path: &Path) -> Result<Vec<String>, String> {
    let entries = fs::read_dir(route_path)
        .map_err(|error| format!("Не удалось прочитать {}: {error}", route_path.display()))?;
    let mut unexpected = Vec::new();
    for entry in entries {
        let entry =
            entry.map_err(|error| format!("Ошибка чтения {}: {error}", route_path.display()))?;
        if entry
            .file_type()
            .map_err(|error| format!("Не удалось прочитать {}: {error}", entry.path().display()))?
            .is_dir()
        {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !is_allowed_route_directory(&name, false) {
                unexpected.push(name);
            }
        }
    }
    unexpected.sort_by_key(|name| name.to_lowercase());
    Ok(unexpected)
}

/// Находит рабочие каталоги программы, оставшиеся в маршруте после аварийного завершения.
pub(crate) fn leftover_temporary_work_directories(route_path: &Path) -> Result<Vec<String>, String> {
    let mut leftovers = Vec::new();
    for entry in WalkDir::new(route_path).min_depth(1).max_depth(3) {
        let entry = entry
            .map_err(|error| format!("Ошибка при обходе {}: {error}", route_path.display()))?;
        if entry.file_type().is_dir()
            && is_temporary_work_directory(&entry.file_name().to_string_lossy())
        {
            leftovers.push(entry.path().display().to_string());
        }
    }
    leftovers.sort();
    Ok(leftovers)
}

pub(crate) fn preferred_serz_path(rw_path: &Path) -> PathBuf {
    let serz64 = rw_path.join("serz64.exe");
    if serz64.is_file() {
        serz64
    } else {
        rw_path.join("serz.exe")
    }
}

pub(crate) fn fallback_serz_path(serz_path: &Path) -> Option<PathBuf> {
    if !serz_path
        .file_name()
        .is_some_and(|name| name.eq_ignore_ascii_case("serz64.exe"))
    {
        return None;
    }
    let fallback = serz_path.with_file_name("serz.exe");
    fallback.is_file().then_some(fallback)
}

struct FileAnalysis {
    converted_bin: bool,
    file_path: PathBuf,
    references: Vec<AssetReference>,
    error: Option<String>,
}

#[derive(Clone)]
struct AssetReference {
    path: String,
    provider_range: std::ops::Range<usize>,
    product_range: std::ops::Range<usize>,
    blueprint_range: std::ops::Range<usize>,
}

fn transliterate_umlauts(value: &str) -> String {
    value
        .replace('ä', "ae")
        .replace('Ä', "Ae")
        .replace('ö', "oe")
        .replace('Ö', "Oe")
        .replace('ü', "ue")
        .replace('Ü', "Ue")
        .replace('ß', "ss")
        .replace('ẞ', "SS")
        .replace('\u{FFDF}', "ss")
}

fn has_umlauts(value: &str) -> bool {
    value.chars().any(|character| {
        matches!(
            character,
            'ä' | 'Ä' | 'ö' | 'Ö' | 'ü' | 'Ü' | 'ß' | 'ẞ' | '\u{FFDF}'
        )
    })
}

fn resolve_asset_path(assets_path: &Path, relative_asset: &str) -> Result<Option<PathBuf>, String> {
    let exact = assets_path.join(relative_asset.replace('/', "\\"));
    if exact.is_file() {
        return Ok(Some(exact));
    }
    if !relative_asset.contains('\u{FFDF}') {
        return Ok(None);
    }

    let umlaut_variant =
        assets_path.join(relative_asset.replace('/', "\\").replace('\u{FFDF}', "ß"));
    let transliterated_variant =
        assets_path.join(relative_asset.replace('/', "\\").replace('\u{FFDF}', "ss"));
    let mut candidates: Vec<_> = [umlaut_variant, transliterated_variant]
        .into_iter()
        .filter(|candidate| candidate.is_file())
        .collect();
    candidates.sort();
    candidates.dedup();
    match candidates.len() {
        0 => Ok(None),
        1 => Ok(candidates.pop()),
        _ => Err(format!(
            "Ссылка содержит испорченный символ U+FFDF и совпадает с несколькими файлами: {}",
            candidates
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

fn has_umlauts_in_xml(xml: &str) -> bool {
    has_umlauts(&decode_xml_text(xml))
}

fn transliterate_xml_umlauts(xml: &str) -> String {
    let mut output = String::with_capacity(xml.len());
    let mut remaining = xml;
    while let Some(ampersand) = remaining.find('&') {
        output.push_str(&transliterate_umlauts(&remaining[..ampersand]));
        remaining = &remaining[ampersand..];
        let Some(semicolon) = remaining.find(';') else {
            output.push_str(&transliterate_umlauts(remaining));
            return output;
        };
        let entity_end = semicolon + 1;
        let decoded = decode_xml_text(&remaining[..entity_end]);
        if decoded.chars().count() == 1 && has_umlauts(&decoded) {
            output.push_str(&transliterate_umlauts(&decoded));
        } else {
            output.push_str(&remaining[..entity_end]);
        }
        remaining = &remaining[entity_end..];
    }
    output.push_str(&transliterate_umlauts(remaining));
    output
}

fn read_serz_xml(serz_path: &Path, asset_path: &Path) -> Result<String, String> {
    let temp = tempdir().map_err(|error| format!("Не удалось создать временную папку: {error}"))?;
    let xml_path = temp.path().join("asset.xml");
    let conversion = convert_bin_to_xml(serz_path, asset_path, &xml_path).or_else(|error| {
        let Some(fallback) = fallback_serz_path(serz_path) else {
            return Err(error);
        };
        convert_bin_to_xml(&fallback, asset_path, &xml_path).map_err(|fallback_error| {
            format!(
                "{error}; повторная попытка через {} не удалась: {fallback_error}",
                fallback.display()
            )
        })
    });
    conversion?;
    fs::read_to_string(&xml_path)
        .map_err(|error| format!("Не удалось прочитать XML {}: {error}", xml_path.display()))
}

fn geometry_reference_regex() -> &'static Regex {
    static REGEX: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r#"(?is)>([^<>]*?\.(?:GeoPcDx|TgPcDx))\s*</[^>]+>"#)
            .expect("Шаблон поиска ссылок на геометрию должен быть корректным")
    })
}

fn referenced_geometry_paths(xml: &str) -> Vec<(String, std::ops::Range<usize>)> {
    geometry_reference_regex()
        .captures_iter(xml)
        .filter_map(|captures| {
            let matched = captures.get(1)?;
            let path = decode_xml_text(matched.as_str().trim());
            let extension = Path::new(&path).extension()?;
            if !extension.eq_ignore_ascii_case("GeoPcDx")
                && !extension.eq_ignore_ascii_case("TgPcDx")
            {
                return None;
            }
            Some((path, capture_value_range(&matched)))
        })
        .collect()
}

fn resolve_product_reference(
    product_root: &Path,
    source_file: &Path,
    reference: &str,
) -> Option<PathBuf> {
    let normalized = reference.replace('\\', "/");
    let relative = Path::new(&normalized);
    let candidates = [
        product_root.join(relative),
        source_file.parent()?.join(relative),
    ];
    candidates.into_iter().find(|candidate| candidate.is_file())
}

fn asset_chain_umlaut_evidence(
    assets_path: &Path,
    serz_path: &Path,
    root_asset: &Path,
) -> Result<Vec<String>, String> {
    let relative = root_asset
        .strip_prefix(assets_path)
        .map_err(|_| format!("Ассет находится вне Assets: {}", root_asset.display()))?;
    let mut relative_components = relative.components();
    let provider = relative_components
        .next()
        .ok_or_else(|| "Некорректный путь ассета.".to_owned())?;
    let product = relative_components
        .next()
        .ok_or_else(|| "Некорректный путь ассета.".to_owned())?;
    let product_root = assets_path.join(provider).join(product);
    let mut queue = vec![root_asset.to_path_buf()];
    let mut visited = HashSet::new();
    let mut evidence = Vec::new();

    while let Some(asset_path) = queue.pop() {
        let canonical = match asset_path.canonicalize() {
            Ok(path) => path,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(format!(
                    "Не удалось проверить {}: {error}",
                    asset_path.display()
                ))
            }
        };
        if !canonical.starts_with(&product_root) || !visited.insert(canonical.clone()) {
            continue;
        }
        let extension = canonical
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("");
        if !extension.eq_ignore_ascii_case("bin") && !extension.eq_ignore_ascii_case("GeoPcDx") {
            continue;
        }
        let xml = read_serz_xml(serz_path, &canonical)?;
        if has_umlauts_in_xml(&xml) {
            evidence.push(format!("Внутри файла: {}", canonical.display()));
        }
        for (reference, _) in referenced_geometry_paths(&xml) {
            if has_umlauts(&reference) {
                evidence.push(format!(
                    "Ссылка из {}: {reference}",
                    canonical.file_name().unwrap_or_default().to_string_lossy()
                ));
            }
            if Path::new(&reference)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("GeoPcDx"))
            {
                if let Some(path) = resolve_product_reference(&product_root, &canonical, &reference)
                {
                    queue.push(path);
                }
            }
        }
    }

    evidence.sort();
    evidence.dedup();
    Ok(evidence)
}

fn inspect_umlaut_asset(
    relative_asset: &str,
    uses: &[AssetUse],
    assets_path: &Path,
    serz_path: &Path,
) -> (Option<UmlautAsset>, Option<String>) {
    let mut evidence = Vec::new();
    if has_umlauts(relative_asset) {
        evidence.push(format!("Умляут в пути ассета: {relative_asset}"));
    }
    let mut scan_error = None;
    let full_asset_path = match resolve_asset_path(assets_path, relative_asset) {
        Ok(Some(path)) => Some(path),
        Ok(None) => None,
        Err(error) => {
            evidence.push(error.clone());
            scan_error = Some(error);
            None
        }
    };
    if let Some(asset_path) = full_asset_path.filter(|path| {
        path.extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("bin"))
    }) {
        match asset_chain_umlaut_evidence(assets_path, serz_path, &asset_path) {
            Ok(found) => evidence.extend(found),
            Err(error) => {
                evidence.push(format!(
                    "Цепочка зависимостей не проверена полностью: {error}"
                ));
                scan_error = Some(error);
            }
        }
    }
    let asset = (!evidence.is_empty()).then(|| UmlautAsset {
        path: relative_asset.to_owned(),
        uses: uses.to_vec(),
        evidence,
    });
    (asset, scan_error)
}

pub(crate) fn scan_umlaut_assets(
    inventory: &[ReferencedAsset],
    assets_path: &Path,
    serz_path: &Path,
    max_concurrent_serz: usize,
) -> Result<(Vec<UmlautAsset>, Vec<String>), String> {
    let worker_count = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(2)
        .min(max_concurrent_serz.max(1))
        .min(inventory.len().max(1));
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(worker_count)
        .build()
        .map_err(|error| format!("Не удалось создать пул проверки умляутов: {error}"))?;
    let scan: Vec<_> = pool.install(|| {
        inventory
            .par_iter()
            .map(|asset| inspect_umlaut_asset(&asset.path, &asset.uses, assets_path, serz_path))
            .collect()
    });
    let mut umlaut_assets = Vec::new();
    let mut errors = Vec::new();
    for (asset, error) in scan {
        if let Some(asset) = asset {
            umlaut_assets.push(asset);
        }
        if let Some(error) = error {
            errors.push(error);
        }
    }
    umlaut_assets.sort_by(|left, right| left.path.cmp(&right.path));
    errors.sort();
    Ok((umlaut_assets, errors))
}

fn analyze_route_file(
    file_path: &Path,
    index: usize,
    temp_dir: &Path,
    serz_path: &Path,
) -> FileAnalysis {
    let is_binary = file_path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("bin"));
    let xml_data = if is_binary {
        let xml_path = temp_dir.join(format!("{index:08}.xml"));
        let conversion_result = convert_bin_to_xml(serz_path, file_path, &xml_path).or_else(
            |primary_error| {
                let Some(fallback) = fallback_serz_path(serz_path) else {
                    return Err(primary_error);
                };
                convert_bin_to_xml(&fallback, file_path, &xml_path).map_err(|fallback_error| {
                    format!(
                        "{primary_error}; повторная попытка через {} тоже не удалась: {fallback_error}",
                        fallback.display()
                    )
                })
            },
        );
        if let Err(error) = conversion_result {
            return FileAnalysis {
                converted_bin: false,
                file_path: file_path.to_path_buf(),
                references: Vec::new(),
                error: Some(format!("{}: {error}", file_path.display())),
            };
        }
        match fs::read(xml_path) {
            Ok(data) => data,
            Err(error) => {
                return FileAnalysis {
                    converted_bin: true,
                    file_path: file_path.to_path_buf(),
                    references: Vec::new(),
                    error: Some(format!(
                        "{}: не удалось прочитать результат Serz: {error}",
                        file_path.display()
                    )),
                };
            }
        }
    } else {
        match fs::read(file_path) {
            Ok(data) => data,
            Err(error) => {
                return FileAnalysis {
                    converted_bin: false,
                    file_path: file_path.to_path_buf(),
                    references: Vec::new(),
                    error: Some(format!(
                        "{}: не удалось прочитать XML: {error}",
                        file_path.display()
                    )),
                };
            }
        }
    };

    let xml_text = match String::from_utf8(xml_data) {
        Ok(text) => text,
        Err(error) => String::from_utf8_lossy(error.as_bytes()).into_owned(),
    };

    FileAnalysis {
        converted_bin: is_binary,
        file_path: file_path.to_path_buf(),
        references: extract_asset_references_with_locations(&xml_text),
        error: None,
    }
}

pub(crate) fn check_route(
    route_path: &Path,
    assets_path: &Path,
    serz_path: &Path,
    ignore_scenarios: bool,
    max_concurrent_serz: usize,
    mut report_progress: impl FnMut(&str),
) -> Result<CheckResult, String> {
    if !serz_path.is_file() {
        return Err(format!(
            "Не найден serz.exe: {}. Выберите корневую папку RailWorks.",
            serz_path.display()
        ));
    }

    report_progress("Сканирование файлов маршрута...");
    let route_files = collect_route_files(route_path, ignore_scenarios)?;

    let mut structure_issues = Vec::new();
    if !route_path.join("RouteProperties.xml").is_file() {
        structure_issues.push("не найден RouteProperties.xml".to_owned());
    }
    if !assets_path.is_dir() {
        structure_issues.push("не найдена папка Assets".to_owned());
    }
    if route_files.is_empty() {
        structure_issues.push("не найдено файлов .bin или .xml для проверки".to_owned());
    }
    let unexpected_directories = unexpected_route_directories(route_path)?;
    if !unexpected_directories.is_empty() {
        structure_issues.push(format!(
            "неизвестные папки маршрута не проверялись: {}",
            unexpected_directories.join(", ")
        ));
    }
    let leftover_work_directories = leftover_temporary_work_directories(route_path)?;
    if !leftover_work_directories.is_empty() {
        structure_issues.push(format!(
            "в маршруте остались рабочие каталоги программы (можно удалить): {}",
            leftover_work_directories.join(", ")
        ));
    }

    let temp_dir = tempdir()
        .map_err(|error| format!("Не удалось создать временную папку для Serz: {error}"))?;
    let binary_count = route_files
        .iter()
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("bin"))
        })
        .count();
    let worker_count = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(2)
        .min(max_concurrent_serz.max(1))
        .min(binary_count.max(1));
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(worker_count)
        .build()
        .map_err(|error| format!("Не удалось создать пул проверки файлов: {error}"))?;

    let mut analyses = Vec::with_capacity(route_files.len());
    for (chunk_index, chunk) in route_files.chunks(32).enumerate() {
        let start_index = chunk_index * 32;
        report_progress(&format!(
            "Анализ файлов: {} из {}; параллельных Serz-процессов: {}...",
            start_index + 1,
            route_files.len(),
            worker_count.min(binary_count.max(1))
        ));
        let chunk_analyses: Vec<FileAnalysis> = pool.install(|| {
            chunk
                .par_iter()
                .enumerate()
                .map(|(offset, file_path)| {
                    analyze_route_file(file_path, start_index + offset, temp_dir.path(), serz_path)
                })
                .collect()
        });
        analyses.extend(chunk_analyses);
    }
    let mut asset_uses: HashMap<String, Vec<AssetUse>> = HashMap::new();
    let mut conversion_errors = Vec::new();
    let mut converted_bin_files = 0;
    for analysis in analyses {
        converted_bin_files += usize::from(analysis.converted_bin);
        let tile = route_tile_label(route_path, &analysis.file_path);
        for reference in analysis.references {
            asset_uses
                .entry(reference.path)
                .or_default()
                .push(AssetUse {
                    source_file: analysis.file_path.clone(),
                    tile: tile.clone(),
                });
        }
        if let Some(error) = analysis.error {
            conversion_errors.push(error);
        }
    }
    conversion_errors.sort();

    report_progress(&format!(
        "Уникальных ссылок: {}. Проверка файлов и архивов .ap...",
        asset_uses.len(),
    ));
    let mut ap_cache: HashMap<PathBuf, HashSet<String>> = HashMap::new();
    for relative_asset in asset_uses.keys() {
        let parts: Vec<&str> = relative_asset.split('/').collect();
        if parts.len() < 3 {
            continue;
        }
        let product_dir = assets_path.join(parts[0]).join(parts[1]);
        if ap_cache.contains_key(&product_dir) {
            continue;
        }

        let mut files = HashSet::new();
        match fs::read_dir(&product_dir) {
            Ok(entries) => {
                for entry in entries {
                    let archive_path = entry
                        .map_err(|error| {
                            format!("Ошибка при чтении {}: {error}", product_dir.display())
                        })?
                        .path();
                    if !archive_path
                        .extension()
                        .is_some_and(|extension| extension.eq_ignore_ascii_case("ap"))
                    {
                        continue;
                    }
                    let archive_file = fs::File::open(&archive_path).map_err(|error| {
                        format!("Не удалось открыть {}: {error}", archive_path.display())
                    })?;
                    let mut archive = match ZipArchive::new(archive_file) {
                        Ok(archive) => archive,
                        Err(error) => {
                            structure_issues.push(format!(
                                "Не удалось прочитать архив ассетов {}: {error}",
                                archive_path.display()
                            ));
                            continue;
                        }
                    };
                    for index in 0..archive.len() {
                        match archive.by_index(index) {
                            Ok(file) => {
                                files.insert(file.name().replace('\\', "/").to_lowercase());
                            }
                            Err(error) => structure_issues.push(format!(
                                "Не удалось прочитать запись {index} в архиве ассетов {}: {error}",
                                archive_path.display()
                            )),
                        }
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "Не удалось прочитать {}: {error}",
                    product_dir.display()
                ));
            }
        }
        ap_cache.insert(product_dir, files);
    }

    report_progress("Проверка имён и строк ассетов на умляуты...");
    let inventory_seed: Vec<_> = asset_uses
        .iter()
        .map(|(path, uses)| ReferencedAsset {
            path: path.clone(),
            uses: uses.clone(),
            missing: false,
        })
        .collect();
    let (umlaut_assets, umlaut_errors) =
        scan_umlaut_assets(&inventory_seed, assets_path, serz_path, max_concurrent_serz)?;
    conversion_errors.extend(umlaut_errors);
    conversion_errors.sort();

    let referenced_assets_count = asset_uses.len();
    let mut asset_inventory = Vec::with_capacity(asset_uses.len());
    let mut missing_assets = Vec::new();
    for (relative_asset, uses) in asset_uses {
        let full_asset_path = match resolve_asset_path(assets_path, &relative_asset) {
            Ok(path) => path,
            Err(error) => {
                structure_issues.push(format!(
                    "Не удалось однозначно сопоставить ассет {relative_asset}: {error}"
                ));
                None
            }
        };
        let parts: Vec<&str> = relative_asset.split('/').collect();
        let in_archive = if parts.len() >= 3 {
            let product_dir = assets_path.join(parts[0]).join(parts[1]);
            let inner_path = parts[2..].join("/").to_lowercase();
            let variants = [
                inner_path.clone(),
                inner_path.replace('\u{FFDF}', "ss"),
                inner_path.replace('\u{FFDF}', "ß"),
            ];
            ap_cache
                .get(&product_dir)
                .is_some_and(|files| variants.iter().any(|variant| files.contains(variant)))
        } else {
            false
        };
        let missing = full_asset_path.is_none() && !in_archive;
        if missing {
            missing_assets.push(MissingAsset {
                path: relative_asset.clone(),
                uses: uses.clone(),
            });
        }
        asset_inventory.push(ReferencedAsset {
            path: relative_asset,
            uses,
            missing,
        });
    }
    missing_assets.sort_by(|left, right| left.path.cmp(&right.path));
    asset_inventory.sort_by(|left, right| left.path.cmp(&right.path));

    Ok(CheckResult {
        route_files: route_files.len(),
        converted_bin_files,
        referenced_assets: referenced_assets_count,
        missing_assets,
        asset_inventory,
        umlaut_assets,
        conversion_errors,
        structure_issues,
    })
}

pub(crate) fn replace_missing_asset(
    route_path: &Path,
    assets_path: &Path,
    serz_path: &Path,
    old_asset: &str,
    replacement_path: &Path,
    uses: &[AssetUse],
) -> Result<AssetReplacementReport, String> {
    let replacement_path = replacement_path
        .canonicalize()
        .map_err(|error| format!("Не удалось проверить новый ассет: {error}"))?;
    let assets_root = assets_path
        .canonicalize()
        .map_err(|error| format!("Не удалось проверить папку Assets: {error}"))?;
    let replacement_relative = replacement_path
        .strip_prefix(&assets_root)
        .map_err(|_| "Новый объект должен находиться внутри папки Assets.".to_owned())?;
    if !replacement_path.is_file()
        || !replacement_path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("bin"))
    {
        return Err("Для замены выберите существующий файл BIN ассета.".to_owned());
    }

    let replacement_parts: Vec<String> = replacement_relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect();
    if replacement_parts.len() < 3
        || replacement_parts
            .iter()
            .any(|component| component == "." || component == "..")
    {
        return Err("Путь нового ассета должен иметь вид Provider/Product/....bin.".to_owned());
    }
    let replacement_provider = replacement_parts[0].clone();
    let replacement_product = replacement_parts[1].clone();
    let mut replacement_blueprint = PathBuf::new();
    for component in &replacement_parts[2..] {
        replacement_blueprint.push(component);
    }
    let replacement_blueprint = replacement_blueprint
        .with_extension("xml")
        .to_string_lossy()
        .replace('/', "\\");
    let new_asset = replacement_relative.to_string_lossy().replace('\\', "/");
    let old_kind = asset_kind(old_asset);
    let new_kind = asset_kind(&new_asset);
    if (old_kind == AssetKind::Loft) != (new_kind == AssetKind::Loft)
        || (old_kind == AssetKind::Scenery) != (new_kind == AssetKind::Scenery)
    {
        return Err(match old_kind {
            AssetKind::Loft => "Для loft-объекта выберите замену из папки Networks.".to_owned(),
            AssetKind::Scenery => {
                "Для scenery-объекта выберите замену из папки Scenery.".to_owned()
            }
            AssetKind::Other => {
                "Тип выбранного ассета не совпадает с типом отсутствующего.".to_owned()
            }
        });
    }
    if !serz_path.is_file() {
        return Err(format!("Не найден Serz: {}", serz_path.display()));
    }
    if uses.is_empty() {
        return Err("Для отсутствующего ассета не найдены места использования.".to_owned());
    }

    let canonical_route = route_path
        .canonicalize()
        .map_err(|error| format!("Не удалось проверить папку маршрута: {error}"))?;
    let mut source_files = HashSet::new();
    for usage in uses {
        let canonical_source = usage.source_file.canonicalize().map_err(|error| {
            format!(
                "Не удалось проверить файл маршрута {}: {error}",
                usage.source_file.display()
            )
        })?;
        if !canonical_source.starts_with(&canonical_route) || !canonical_source.is_file() {
            return Err(format!(
                "Файл для замены находится вне маршрута или недоступен: {}",
                usage.source_file.display()
            ));
        }
        source_files.insert(canonical_source);
    }

    let mut staged = Vec::new();
    let mut replaced_instances = 0;
    for source_file in source_files {
        let is_binary = source_file
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("bin"));
        if !is_binary
            && !source_file
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("xml"))
        {
            return Err(format!(
                "Неизвестный формат файла маршрута: {}",
                source_file.display()
            ));
        }
        let source_parent = source_file.parent().unwrap_or(route_path);
        let temp = create_temporary_work_directory(source_parent)?;
        let xml_path = temp.path().join("source.xml");
        if is_binary {
            let conversion =
                convert_bin_to_xml(serz_path, &source_file, &xml_path).or_else(|error| {
                    let Some(fallback) = fallback_serz_path(serz_path) else {
                        return Err(error);
                    };
                    convert_bin_to_xml(&fallback, &source_file, &xml_path).map_err(
                        |fallback_error| {
                            format!(
                                "{error}; повторная попытка через Serz тоже не удалась: {fallback_error}"
                            )
                        },
                    )
                });
            conversion.map_err(|error| {
                format!(
                    "Не удалось прочитать {} перед заменой: {error}",
                    source_file.display()
                )
            })?;
        } else {
            fs::copy(&source_file, &xml_path).map_err(|error| {
                format!(
                    "Не удалось подготовить {} перед заменой: {error}",
                    source_file.display()
                )
            })?;
        }

        let mut xml = fs::read_to_string(&xml_path).map_err(|error| {
            format!(
                "Не удалось прочитать XML для {}: {error}",
                source_file.display()
            )
        })?;
        let references = extract_asset_references_with_locations(&xml);
        let edits: Vec<_> = references
            .into_iter()
            .filter(|reference| reference.path.eq_ignore_ascii_case(old_asset))
            .flat_map(|reference| {
                [
                    (
                        reference.provider_range,
                        escape_xml_text(&replacement_provider),
                    ),
                    (
                        reference.product_range,
                        escape_xml_text(&replacement_product),
                    ),
                    (
                        reference.blueprint_range,
                        escape_xml_text(&replacement_blueprint),
                    ),
                ]
            })
            .collect();
        let instances = edits.len() / 3;
        if instances == 0 {
            continue;
        }
        let mut edits = edits;
        edits.sort_by_key(|(range, _)| std::cmp::Reverse(range.start));
        for (range, value) in edits {
            xml.replace_range(range, &value);
        }
        validate_xml_document(&xml).map_err(|error| {
            format!(
                "Замена в {} создала некорректный XML: {error}",
                source_file.display()
            )
        })?;
        fs::write(&xml_path, xml).map_err(|error| {
            format!(
                "Не удалось записать временный XML для {}: {error}",
                source_file.display()
            )
        })?;

        let converted_path = if is_binary {
            let converted_path = temp.path().join("replacement.bin");
            let conversion = convert_xml_to_bin(serz_path, &xml_path, &converted_path).or_else(
                |error| {
                    let Some(fallback) = fallback_serz_path(serz_path) else {
                        return Err(error);
                    };
                    convert_xml_to_bin(&fallback, &xml_path, &converted_path).map_err(
                        |fallback_error| {
                            format!(
                                "{error}; повторная попытка через Serz тоже не удалась: {fallback_error}"
                            )
                        },
                    )
                },
            );
            conversion.map_err(|error| {
                format!(
                    "Не удалось подготовить изменённый BIN {}: {error}",
                    source_file.display()
                )
            })?;
            converted_path
        } else {
            xml_path
        };
        replaced_instances += instances;
        staged.push((source_file, temp, converted_path));
    }

    if staged.is_empty() {
        return Err(format!(
            "В исходниках маршрута не найдено использований {}. Обновите проверку и повторите.",
            old_asset
        ));
    }

    let mut backups = Vec::new();
    let mut replaced_files = 0;
    for (source_file, _temp, converted_path) in staged {
        match install_converted_asset(&source_file, &converted_path) {
            Ok(backup) => {
                replaced_files += 1;
                backups.push(backup);
            }
            Err(error) => {
                return Err(format!(
                    "Заменено размещений: {replaced_instances}; успешно изменено файлов: {replaced_files}. Ошибка в {}: {error}. Созданные резервные копии: {}",
                    source_file.display(),
                    backups
                        .iter()
                        .map(|path: &PathBuf| path.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }
    }
    Ok(AssetReplacementReport {
        replaced_files,
        replaced_instances,
        backups,
        replacement_asset: new_asset,
        log_file: None,
        log_error: None,
    })
}

struct StagedUmlautFile {
    source: PathBuf,
    target: PathBuf,
    staged: PathBuf,
    _temp: tempfile::TempDir,
}

fn transliterated_asset_path(assets_path: &Path, source: &Path) -> Result<PathBuf, String> {
    let relative = source
        .strip_prefix(assets_path)
        .map_err(|_| format!("Ассет находится вне папки Assets: {}", source.display()))?;
    let mut components = relative.components();
    let provider = components
        .next()
        .ok_or_else(|| "Некорректный путь ассета.".to_owned())?
        .as_os_str()
        .to_string_lossy()
        .into_owned();
    let product = components
        .next()
        .ok_or_else(|| "Некорректный путь ассета.".to_owned())?
        .as_os_str()
        .to_string_lossy()
        .into_owned();
    let nested: Vec<_> = components
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect();
    if has_umlauts(&provider) || has_umlauts(&product) {
        return Err(format!(
            "Умляут найден в имени Provider/Product для {}. Автоматическая транслитерация папок Provider/Product остановлена, чтобы не затронуть другие ассеты.",
            source.display()
        ));
    }
    if nested.len() > 1
        && nested[..nested.len() - 1]
            .iter()
            .any(|part| has_umlauts(part))
    {
        return Err(format!(
            "Умляут найден в имени вложенной папки для {}. Переименование целых папок пока не выполняется, чтобы не повредить соседние ссылки.",
            source.display()
        ));
    }
    let translated = transliterate_umlauts(&relative.to_string_lossy());
    Ok(assets_path.join(translated))
}

fn translated_reference(
    target_file: &Path,
    target_source: &Path,
    product_root: &Path,
    relative_to_product: bool,
) -> Result<String, String> {
    let base = if relative_to_product {
        product_root
    } else {
        target_source.parent().ok_or_else(|| {
            format!(
                "Некорректный путь к исходному ассету: {}",
                target_source.display()
            )
        })?
    };
    let relative = target_file.strip_prefix(base).map_err(|_| {
        format!(
            "Путь {} вышел за пределы {}",
            target_file.display(),
            base.display()
        )
    })?;
    Ok(relative.to_string_lossy().replace('/', "\\"))
}

fn stage_serz_xml(
    source: &Path,
    target: &Path,
    xml: &str,
    serz_path: &Path,
) -> Result<StagedUmlautFile, String> {
    let parent = source
        .parent()
        .ok_or_else(|| format!("Некорректный путь к ассету: {}", source.display()))?;
    let temp = create_temporary_work_directory(parent)?;
    let xml_path = temp.path().join("transliterated.xml");
    fs::write(&xml_path, xml)
        .map_err(|error| format!("Не удалось записать временный XML: {error}"))?;
    let extension = source
        .extension()
        .ok_or_else(|| format!("Не удалось определить тип файла: {}", source.display()))?;
    let mut staged = temp.path().join("transliterated");
    staged.set_extension(extension);
    let conversion = convert_xml_to_bin(serz_path, &xml_path, &staged).or_else(|error| {
        let Some(fallback) = fallback_serz_path(serz_path) else {
            return Err(error);
        };
        convert_xml_to_bin(&fallback, &xml_path, &staged).map_err(|fallback_error| {
            format!(
                "{error}; повторная попытка через {} не удалась: {fallback_error}",
                fallback.display()
            )
        })
    });
    conversion.map_err(|error| {
        format!(
            "Не удалось подготовить изменённый файл {}: {error}",
            source.display()
        )
    })?;
    Ok(StagedUmlautFile {
        source: source.to_path_buf(),
        target: target.to_path_buf(),
        staged,
        _temp: temp,
    })
}

fn stage_renamed_file(source: &Path, target: &Path) -> Result<StagedUmlautFile, String> {
    let parent = source
        .parent()
        .ok_or_else(|| format!("Некорректный путь к ассету: {}", source.display()))?;
    let temp = create_temporary_work_directory(parent)?;
    let staged = temp.path().join(
        source
            .file_name()
            .ok_or_else(|| format!("Некорректный путь к ассету: {}", source.display()))?,
    );
    fs::copy(source, &staged).map_err(|error| {
        format!(
            "Не удалось подготовить переименование {}: {error}",
            source.display()
        )
    })?;
    Ok(StagedUmlautFile {
        source: source.to_path_buf(),
        target: target.to_path_buf(),
        staged,
        _temp: temp,
    })
}

pub(crate) fn repair_umlaut_asset_chain(
    route_path: &Path,
    assets_path: &Path,
    serz_path: &Path,
    asset_relative_path: &str,
    uses: &[AssetUse],
) -> Result<UmlautRepairReport, String> {
    if !serz_path.is_file() {
        return Err(format!("Не найден Serz: {}", serz_path.display()));
    }
    if uses.is_empty() {
        return Err("Для ассета не найдены места использования в маршруте.".to_owned());
    }
    let assets_root = assets_path
        .canonicalize()
        .map_err(|error| format!("Не удалось проверить папку Assets: {error}"))?;
    let route_root = route_path
        .canonicalize()
        .map_err(|error| format!("Не удалось проверить папку маршрута: {error}"))?;
    let source_asset = resolve_asset_path(&assets_root, asset_relative_path)?.ok_or_else(|| {
        format!(
            "Не удалось найти BIN для ссылки {}. Проверены исходный путь и варианты с ß/ss.",
            asset_relative_path
        )
    })?;
    let canonical_asset = source_asset.canonicalize().map_err(|error| {
        format!(
            "Не удалось открыть ассет {}: {error}",
            source_asset.display()
        )
    })?;
    if !canonical_asset.starts_with(&assets_root) || !canonical_asset.is_file() {
        return Err(format!(
            "Ассет находится вне Assets или недоступен: {}",
            source_asset.display()
        ));
    }
    let new_asset_path = transliterated_asset_path(&assets_root, &canonical_asset)?;
    let new_relative_asset = new_asset_path
        .strip_prefix(&assets_root)
        .map_err(|_| "Новый путь ассета выходит за пределы Assets.".to_owned())?
        .to_string_lossy()
        .replace('\\', "/");
    if new_relative_asset.split('/').count() < 3 {
        return Err("Путь ассета должен иметь структуру Provider/Product/файл.".to_owned());
    }
    if new_relative_asset == asset_relative_path {
        let evidence = asset_chain_umlaut_evidence(&assets_root, serz_path, &canonical_asset)?;
        if evidence.is_empty() {
            return Err("В файлах ассета не обнаружены поддерживаемые умляуты.".to_owned());
        }
    }

    let relative = canonical_asset
        .strip_prefix(&assets_root)
        .map_err(|_| "Ассет находится вне папки Assets.".to_owned())?;
    let mut components = relative.components();
    let provider = components
        .next()
        .ok_or_else(|| "Некорректный путь ассета.".to_owned())?;
    let product = components
        .next()
        .ok_or_else(|| "Некорректный путь ассета.".to_owned())?;
    let product_root = assets_root.join(provider).join(product);
    let mut queue = vec![canonical_asset.clone()];
    let mut visited = HashSet::new();
    let mut staged_by_source: HashMap<PathBuf, StagedUmlautFile> = HashMap::new();
    let mut renamed_tg_files: HashMap<PathBuf, PathBuf> = HashMap::new();
    let mut scan_all_geometry = false;
    let mut changed_references = 0;

    while let Some(source) = queue.pop() {
        let canonical = source.canonicalize().map_err(|error| {
            format!(
                "Не удалось открыть зависимость {}: {error}",
                source.display()
            )
        })?;
        if !canonical.starts_with(&product_root) || !visited.insert(canonical.clone()) {
            continue;
        }
        let extension = canonical
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("");
        if !extension.eq_ignore_ascii_case("bin") && !extension.eq_ignore_ascii_case("GeoPcDx") {
            continue;
        }
        let xml = read_serz_xml(serz_path, &canonical)?;
        let target_file = transliterated_asset_path(&assets_root, &canonical)?;
        let mut edits = Vec::new();
        for (reference, range) in referenced_geometry_paths(&xml) {
            let extension = Path::new(&reference)
                .extension()
                .and_then(|value| value.to_str())
                .unwrap_or("");
            let Some(dependency) = resolve_product_reference(&product_root, &canonical, &reference)
            else {
                if has_umlauts(&reference) {
                    return Err(format!(
                        "Ссылка с умляутом не разрешается в файл: {} → {reference}",
                        canonical.display()
                    ));
                }
                continue;
            };
            let canonical_dependency = dependency.canonicalize().map_err(|error| {
                format!(
                    "Не удалось проверить зависимость {}: {error}",
                    dependency.display()
                )
            })?;
            if !canonical_dependency.starts_with(&product_root) {
                return Err(format!(
                    "Ссылка выходит за пределы продукта {}: {}",
                    product_root.display(),
                    dependency.display()
                ));
            }
            let translated_dependency =
                transliterated_asset_path(&assets_root, &canonical_dependency)?;
            if extension.eq_ignore_ascii_case("GeoPcDx") {
                queue.push(canonical_dependency.clone());
            } else if extension.eq_ignore_ascii_case("TgPcDx") && has_umlauts(&reference) {
                scan_all_geometry = true;
                renamed_tg_files
                    .insert(canonical_dependency.clone(), translated_dependency.clone());
            }
            if transliterate_umlauts(&reference) != reference
                && translated_dependency == canonical_dependency
            {
                changed_references += 1;
            }
            if translated_dependency != canonical_dependency {
                let relative_to_product =
                    product_root.join(reference.replace('\\', "/")) == canonical_dependency;
                let replacement = translated_reference(
                    &translated_dependency,
                    &target_file,
                    &product_root,
                    relative_to_product,
                )?;
                edits.push((range, escape_xml_text(&replacement)));
                changed_references += 1;
            }
        }
        let mut rewritten = xml.clone();
        edits.sort_by_key(|(range, _)| std::cmp::Reverse(range.start));
        for (range, value) in edits {
            rewritten.replace_range(range, &value);
        }
        rewritten = transliterate_xml_umlauts(&rewritten);
        if rewritten != xml || target_file != canonical {
            validate_xml_document(&rewritten).map_err(|error| {
                format!(
                    "Некорректный XML после обработки {}: {error}",
                    canonical.display()
                )
            })?;
            let staged = stage_serz_xml(&canonical, &target_file, &rewritten, serz_path)?;
            staged_by_source.insert(canonical, staged);
        }
    }

    if scan_all_geometry {
        for entry in WalkDir::new(&product_root).follow_links(false) {
            let entry = entry.map_err(|error| {
                format!("Ошибка обхода продукта {}: {error}", product_root.display())
            })?;
            if entry.file_type().is_file()
                && entry
                    .path()
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("GeoPcDx"))
            {
                queue.push(entry.into_path());
            }
        }
        while let Some(source) = queue.pop() {
            let canonical = source.canonicalize().map_err(|error| {
                format!("Не удалось открыть геометрию {}: {error}", source.display())
            })?;
            if !canonical.starts_with(&product_root) || !visited.insert(canonical.clone()) {
                continue;
            }
            let xml = read_serz_xml(serz_path, &canonical)?;
            for (reference, _) in referenced_geometry_paths(&xml) {
                if transliterate_umlauts(&reference) != reference {
                    changed_references += 1;
                }
                let extension = Path::new(&reference)
                    .extension()
                    .and_then(|value| value.to_str())
                    .unwrap_or("");
                let Some(dependency) =
                    resolve_product_reference(&product_root, &canonical, &reference)
                else {
                    if has_umlauts(&reference) {
                        return Err(format!(
                            "Ссылка с умляутом не разрешается в файл: {} → {reference}",
                            canonical.display()
                        ));
                    }
                    continue;
                };
                let dependency = dependency.canonicalize().map_err(|error| {
                    format!(
                        "Не удалось проверить зависимость {}: {error}",
                        dependency.display()
                    )
                })?;
                if !dependency.starts_with(&product_root) {
                    return Err(format!(
                        "Ссылка выходит за пределы продукта {}: {}",
                        product_root.display(),
                        dependency.display()
                    ));
                }
                if extension.eq_ignore_ascii_case("GeoPcDx") {
                    queue.push(dependency.clone());
                } else if extension.eq_ignore_ascii_case("TgPcDx") && has_umlauts(&reference) {
                    let target = transliterated_asset_path(&assets_root, &dependency)?;
                    renamed_tg_files.insert(dependency, target);
                }
            }
            let target = transliterated_asset_path(&assets_root, &canonical)?;
            let rewritten = transliterate_xml_umlauts(&xml);
            if rewritten != xml || target != canonical {
                validate_xml_document(&rewritten).map_err(|error| {
                    format!(
                        "Некорректный XML после обработки {}: {error}",
                        canonical.display()
                    )
                })?;
                let staged = stage_serz_xml(&canonical, &target, &rewritten, serz_path)?;
                staged_by_source.insert(canonical, staged);
            }
        }
    }

    for (source, target) in &renamed_tg_files {
        if !staged_by_source.contains_key(source) {
            staged_by_source.insert(source.clone(), stage_renamed_file(source, target)?);
        }
    }

    let new_parts: Vec<_> = new_relative_asset.split('/').collect();
    let replacement_blueprint = Path::new(&new_parts[2..].join("\\"))
        .with_extension("xml")
        .to_string_lossy()
        .into_owned();
    let mut route_sources: HashSet<PathBuf> = HashSet::new();
    if new_relative_asset != asset_relative_path {
        for usage in uses {
            let source = usage.source_file.canonicalize().map_err(|error| {
                format!(
                    "Не удалось открыть файл маршрута {}: {error}",
                    usage.source_file.display()
                )
            })?;
            if !source.starts_with(&route_root) {
                return Err(format!(
                    "Файл использования находится вне маршрута: {}",
                    source.display()
                ));
            }
            route_sources.insert(source);
        }
    }
    for source in route_sources {
        let is_binary = source
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("bin"));
        if !is_binary
            && !source
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("xml"))
        {
            return Err(format!(
                "Неподдерживаемый файл маршрута: {}",
                source.display()
            ));
        }
        let temp = create_temporary_work_directory(source.parent().unwrap_or(&route_root))?;
        let xml_path = temp.path().join("route.xml");
        if is_binary {
            let conversion = convert_bin_to_xml(serz_path, &source, &xml_path).or_else(|error| {
                let Some(fallback) = fallback_serz_path(serz_path) else {
                    return Err(error);
                };
                convert_bin_to_xml(&fallback, &source, &xml_path).map_err(|fallback_error| {
                    format!(
                        "{error}; повторная попытка через {} не удалась: {fallback_error}",
                        fallback.display()
                    )
                })
            });
            conversion?;
        } else {
            fs::copy(&source, &xml_path)
                .map_err(|error| format!("Не удалось подготовить {}: {error}", source.display()))?;
        }
        let mut xml = fs::read_to_string(&xml_path)
            .map_err(|error| format!("Не удалось прочитать {}: {error}", xml_path.display()))?;
        let references = extract_asset_references_with_locations(&xml);
        let mut edits = Vec::new();
        for reference in references
            .into_iter()
            .filter(|reference| reference.path.eq_ignore_ascii_case(asset_relative_path))
        {
            changed_references += 1;
            edits.extend([
                (reference.provider_range, new_parts[0].to_owned()),
                (reference.product_range, new_parts[1].to_owned()),
                (reference.blueprint_range, replacement_blueprint.clone()),
            ]);
        }
        if edits.is_empty() {
            return Err(format!(
                "Ссылка {} не найдена в файле маршрута {}.",
                asset_relative_path,
                source.display()
            ));
        }
        edits.sort_by_key(|(range, _)| std::cmp::Reverse(range.start));
        for (range, value) in edits {
            xml.replace_range(range, &escape_xml_text(&value));
        }
        validate_xml_document(&xml)
            .map_err(|error| format!("Некорректный XML маршрута {}: {error}", source.display()))?;
        fs::write(&xml_path, xml)
            .map_err(|error| format!("Не удалось записать подготовленный маршрут: {error}"))?;
        let staged_path = if is_binary {
            let mut staged = temp.path().join("route-new");
            staged.set_extension(source.extension().unwrap());
            let conversion = convert_xml_to_bin(serz_path, &xml_path, &staged).or_else(|error| {
                let Some(fallback) = fallback_serz_path(serz_path) else {
                    return Err(error);
                };
                convert_xml_to_bin(&fallback, &xml_path, &staged).map_err(|fallback_error| {
                    format!(
                        "{error}; повторная попытка через {} не удалась: {fallback_error}",
                        fallback.display()
                    )
                })
            });
            conversion?;
            staged
        } else {
            xml_path
        };
        staged_by_source.insert(
            source.clone(),
            StagedUmlautFile {
                source: source.clone(),
                target: source,
                staged: staged_path,
                _temp: temp,
            },
        );
    }

    if staged_by_source.is_empty() {
        return Err("Не найдено файлов для транслитерации.".to_owned());
    }
    let mut targets = HashSet::new();
    for staged in staged_by_source.values() {
        if !targets.insert(staged.target.clone()) {
            return Err(format!(
                "Несколько файлов претендуют на путь {}.",
                staged.target.display()
            ));
        }
        if staged.target.exists() && staged.target != staged.source {
            return Err(format!(
                "После транслитерации возникнет конфликт имён: {} уже существует.",
                staged.target.display()
            ));
        }
    }

    let mut prepared: Vec<_> = staged_by_source.into_values().collect();
    prepared.sort_by(|left, right| left.source.cmp(&right.source));
    for staged in &prepared {
        if let Some(parent) = staged.target.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                format!("Не удалось создать папку {}: {error}", parent.display())
            })?;
        }
    }
    let mut committed: Vec<(PathBuf, PathBuf, PathBuf)> = Vec::new();
    for staged in &prepared {
        let backup = match next_backup_path(&staged.source) {
            Ok(path) => path,
            Err(error) => {
                let rollback_errors = rollback_umlaut_changes(&committed);
                let rollback_detail = if rollback_errors.is_empty() {
                    String::new()
                } else {
                    format!(" Ошибки отката: {}", rollback_errors.join("; "))
                };
                return Err(format!("{error}.{rollback_detail}"));
            }
        };
        if let Err(error) = fs::rename(&staged.source, &backup) {
            let rollback_errors = rollback_umlaut_changes(&committed);
            let rollback_detail = if rollback_errors.is_empty() {
                String::new()
            } else {
                format!(" Ошибки отката: {}", rollback_errors.join("; "))
            };
            return Err(format!(
                "Не удалось создать резервную копию {}: {error}.{rollback_detail}",
                staged.source.display()
            ));
        }
        if let Err(error) = fs::rename(&staged.staged, &staged.target) {
            let restore = fs::rename(&backup, &staged.source);
            let rollback_errors = rollback_umlaut_changes(&committed);
            let rollback_detail = if rollback_errors.is_empty() {
                String::new()
            } else {
                format!(
                    " Дополнительные ошибки отката: {}",
                    rollback_errors.join("; ")
                )
            };
            return Err(match restore {
                Ok(()) => format!("Не удалось установить {}: {error}. Файл восстановлен.{rollback_detail}", staged.target.display()),
                Err(restore_error) => format!("Не удалось установить {}: {error}; восстановление {} тоже не удалось: {restore_error}.{rollback_detail}", staged.target.display(), staged.source.display()),
            });
        }
        committed.push((staged.source.clone(), staged.target.clone(), backup));
    }
    Ok(UmlautRepairReport {
        changed_files: committed.len(),
        changed_references,
        files: committed
            .iter()
            .flat_map(|(source, target, _)| [source.clone(), target.clone()])
            .collect(),
        backups: committed.into_iter().map(|(_, _, backup)| backup).collect(),
        new_asset_path: new_relative_asset,
        log_file: None,
        log_error: None,
    })
}

fn rollback_umlaut_changes(committed: &[(PathBuf, PathBuf, PathBuf)]) -> Vec<String> {
    let mut errors = Vec::new();
    for (source, target, backup) in committed.iter().rev() {
        if target.exists() {
            if let Err(error) = fs::remove_file(target) {
                errors.push(format!("не удалось удалить {}: {error}", target.display()));
                continue;
            }
        }
        if let Err(error) = fs::rename(backup, source) {
            errors.push(format!(
                "не удалось восстановить {} из {}: {error}",
                source.display(),
                backup.display()
            ));
        }
    }
    errors
}

fn escape_xml_text(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

pub(crate) fn convert_bin_to_xml(
    serz_path: &Path,
    bin_path: &Path,
    xml_path: &Path,
) -> Result<(), String> {
    if xml_path.exists() {
        fs::remove_file(xml_path).map_err(|error| {
            format!(
                "не удалось удалить предыдущий временный XML {}: {error}",
                xml_path.display()
            )
        })?;
    }
    let output = Command::new(serz_path)
        .current_dir(
            serz_path
                .parent()
                .ok_or_else(|| format!("Некорректный путь к Serz: {}", serz_path.display()))?,
        )
        .arg(legacy_windows_path(bin_path))
        .arg(format!("/xml:{}", legacy_windows_path(xml_path).display()))
        .output()
        .map_err(|error| format!("не удалось запустить serz.exe: {error}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    validate_serz_process(output.status.success(), &stdout, &stderr)
        .map_err(|error| format!("{error} (код выхода {})", output.status))?;
    let metadata = fs::metadata(xml_path).map_err(|error| {
        format!(
            "Serz завершился успешно, но выходной XML {} недоступен: {error}",
            xml_path.display()
        )
    })?;
    if !metadata.is_file() || metadata.len() == 0 {
        return Err(format!(
            "Serz завершился успешно, но не создал непустой XML-файл: {}",
            xml_path.display()
        ));
    }
    if !has_complete_xml_tail(xml_path)? {
        return Err(format!(
            "Serz создал незавершённый XML для {}. Файл использует формат, который не поддерживает конвертация в XML.",
            bin_path.display()
        ));
    }
    Ok(())
}

/// Проверяет, что XML-файл закрыт корневым элементом: Serz возвращает обрывок разметки
/// для некоторых устаревших форматов файлов маршрута.
fn has_complete_xml_tail(xml_path: &Path) -> Result<bool, String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = fs::File::open(xml_path).map_err(|error| {
        format!(
            "не удалось прочитать {} после конвертации: {error}",
            xml_path.display()
        )
    })?;
    let length = file
        .seek(SeekFrom::End(0))
        .map_err(|error| format!("не удалось определить размер {}: {error}", xml_path.display()))?;
    let read_size = length.min(64);
    file.seek(SeekFrom::End(-(read_size as i64)))
        .map_err(|error| format!("не удалось прочитать {}: {error}", xml_path.display()))?;
    let mut tail = vec![0u8; read_size as usize];
    file.read_exact(&mut tail)
        .map_err(|error| format!("не удалось прочитать {}: {error}", xml_path.display()))?;
    Ok(tail
        .iter()
        .rev()
        .find(|byte| !byte.is_ascii_whitespace())
        .is_some_and(|byte| *byte == b'>'))
}

pub(crate) fn convert_xml_to_bin(
    serz_path: &Path,
    xml_path: &Path,
    bin_path: &Path,
) -> Result<(), String> {
    if bin_path.exists() {
        fs::remove_file(bin_path).map_err(|error| {
            format!(
                "не удалось удалить предыдущий временный бинарный файл {}: {error}",
                bin_path.display()
            )
        })?;
    }
    let output = Command::new(serz_path)
        .current_dir(
            serz_path
                .parent()
                .ok_or_else(|| format!("Некорректный путь к Serz: {}", serz_path.display()))?,
        )
        .arg(legacy_windows_path(xml_path))
        .arg(format!("/bin:{}", legacy_windows_path(bin_path).display()))
        .output()
        .map_err(|error| format!("не удалось запустить serz.exe: {error}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    validate_serz_process(output.status.success(), &stdout, &stderr)
        .map_err(|error| format!("{error} (код выхода {})", output.status))?;
    let metadata = fs::metadata(bin_path).map_err(|error| {
        format!(
            "Serz завершился успешно, но выходной бинарный файл {} недоступен: {error}",
            bin_path.display()
        )
    })?;
    if !metadata.is_file() || metadata.len() == 0 {
        return Err(format!(
            "Serz завершился успешно, но не создал непустой бинарный файл: {}",
            bin_path.display()
        ));
    }
    Ok(())
}

pub(crate) fn validate_serz_process(
    status_success: bool,
    stdout: &str,
    stderr: &str,
) -> Result<(), String> {
    if !status_success {
        let detail = if !stderr.trim().is_empty() {
            stderr.trim().to_owned()
        } else if stdout.trim().is_empty() {
            "Serz завершился с ошибкой без диагностического сообщения.".to_owned()
        } else {
            stdout.trim().to_owned()
        };
        return Err(detail);
    }
    if !stderr.trim().is_empty() {
        return Err(format!("Serz сообщил об ошибке: {}", stderr.trim()));
    }
    Ok(())
}

pub(crate) fn legacy_windows_path(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};

        const VERBATIM_PREFIX: [u16; 4] = [b'\\' as u16, b'\\' as u16, b'?' as u16, b'\\' as u16];
        const UNC_PREFIX: [u16; 4] = [b'U' as u16, b'N' as u16, b'C' as u16, b'\\' as u16];

        let path_wide: Vec<u16> = path.as_os_str().encode_wide().collect();
        if let Some(path_wide) = path_wide.strip_prefix(&VERBATIM_PREFIX) {
            if let Some(unc_path) = path_wide.strip_prefix(&UNC_PREFIX) {
                let mut legacy_unc_path = vec![b'\\' as u16, b'\\' as u16];
                legacy_unc_path.extend_from_slice(unc_path);
                return PathBuf::from(std::ffi::OsString::from_wide(&legacy_unc_path));
            }
            return PathBuf::from(std::ffi::OsString::from_wide(path_wide));
        }
    }
    path.to_path_buf()
}

fn capture_value_range(capture: &regex::Match<'_>) -> std::ops::Range<usize> {
    let value = capture.as_str();
    let leading = value.len() - value.trim_start().len();
    let trimmed = value.trim();
    capture.start() + leading..capture.start() + leading + trimmed.len()
}

fn absolute_blueprint_regex() -> &'static Regex {
    static REGEX: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(
            r"(?is)<iBlueprintLibrary-cAbsoluteBlueprintID>(.*?)</iBlueprintLibrary-cAbsoluteBlueprintID>",
        )
        .expect("Шаблон поиска блока абсолютного блюпринта должен быть корректным")
    })
}

fn provider_regex() -> &'static Regex {
    static REGEX: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"(?is)<Provider[^>]*>([^<]*)</Provider>")
            .expect("Шаблон поиска Provider должен быть корректным")
    })
}

fn product_regex() -> &'static Regex {
    static REGEX: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"(?is)<Product[^>]*>([^<]*)</Product>")
            .expect("Шаблон поиска Product должен быть корректным")
    })
}

fn blueprint_id_regex() -> &'static Regex {
    static REGEX: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"(?is)<BlueprintID[^>]*>([^<]*)</BlueprintID>")
            .expect("Шаблон поиска BlueprintID должен быть корректным")
    })
}

fn bare_blueprint_regex() -> &'static Regex {
    static REGEX: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(
            r"(?is)<Provider[^>]*>([^<]*)</Provider>[^<]*<Product[^>]*>([^<]*)</Product>[^<]*<BlueprintID[^>]*>([^<]*)</BlueprintID>",
        )
        .expect("Шаблон поиска голой ссылки на блюпринт должен быть корректным")
    })
}

fn trimmed_range(capture: &regex::Match<'_>, offset: usize) -> std::ops::Range<usize> {
    let value = capture.as_str();
    let leading = value.len() - value.trim_start().len();
    let trimmed = value.trim();
    let start = capture.start() + offset + leading;
    start..start + trimmed.len()
}

fn asset_relative_path(provider: &str, product: &str, blueprint: &str) -> Option<String> {
    let blueprint_path = Path::new(blueprint);
    let extension = blueprint_path.extension()?;
    let file_name = if extension.eq_ignore_ascii_case("xml") {
        blueprint_path.with_extension("bin")
    } else if extension.eq_ignore_ascii_case("bin") {
        blueprint_path.to_path_buf()
    } else {
        return None;
    };
    let relative_path = Path::new(provider).join(product).join(file_name);
    let components: Vec<_> = relative_path.components().collect();
    if components.is_empty()
        || components
            .iter()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return None;
    }
    Some(relative_path.to_string_lossy().replace('\\', "/"))
}

fn build_asset_reference(
    provider: &str,
    product: &str,
    blueprint: &str,
    provider_range: std::ops::Range<usize>,
    product_range: std::ops::Range<usize>,
    blueprint_range: std::ops::Range<usize>,
) -> Option<AssetReference> {
    let provider = decode_xml_text(provider.trim());
    let product = decode_xml_text(product.trim());
    let blueprint = decode_xml_text(blueprint.trim());
    if provider.is_empty() || product.is_empty() || blueprint.is_empty() {
        return None;
    }
    let path = asset_relative_path(&provider, &product, &blueprint)?;
    Some(AssetReference {
        path,
        provider_range,
        product_range,
        blueprint_range,
    })
}

/// Собирает все ссылки на ассеты маршрута.
///
/// Основной источник — блоки `iBlueprintLibrary-cAbsoluteBlueprintID`, внутри которых
/// Provider, Product и путь к блюпринту принадлежат одной ссылке. Такой разбор не даёт
/// пустым заготовкам `ReskinBlueprintID` «съесть» ссылку следующего за ними объекта.
fn extract_asset_references_with_locations(xml: &str) -> Vec<AssetReference> {
    let mut references: Vec<(usize, AssetReference)> = Vec::new();
    let mut covered: Vec<std::ops::Range<usize>> = Vec::new();

    for block in absolute_blueprint_regex().captures_iter(xml) {
        let Some(whole) = block.get(0) else {
            continue;
        };
        let Some(inner) = block.get(1) else {
            continue;
        };
        covered.push(whole.start()..whole.end());
        let block_text = inner.as_str();
        let Some(provider_match) = provider_regex().captures(block_text) else {
            continue;
        };
        let Some(product_match) = product_regex().captures(block_text) else {
            continue;
        };
        let Some(blueprint_match) = blueprint_id_regex().captures(block_text) else {
            continue;
        };
        let Some(provider_capture) = provider_match.get(1) else {
            continue;
        };
        let Some(product_capture) = product_match.get(1) else {
            continue;
        };
        let Some(blueprint_capture) = blueprint_match.get(1) else {
            continue;
        };
        let offset = inner.start();
        let Some(reference) = build_asset_reference(
            provider_capture.as_str(),
            product_capture.as_str(),
            blueprint_capture.as_str(),
            trimmed_range(&provider_capture, offset),
            trimmed_range(&product_capture, offset),
            trimmed_range(&blueprint_capture, offset),
        ) else {
            continue;
        };
        references.push((whole.start(), reference));
    }

    // Запасной вариант для файлов без обёртки `iBlueprintLibrary-cAbsoluteBlueprintID`.
    let mut covered_index = 0usize;
    for captures in bare_blueprint_regex().captures_iter(xml) {
        let Some(whole) = captures.get(0) else {
            continue;
        };
        while covered_index < covered.len() && covered[covered_index].end <= whole.start() {
            covered_index += 1;
        }
        if covered_index < covered.len() && covered[covered_index].contains(&whole.start()) {
            continue;
        }
        let Some(provider_capture) = captures.get(1) else {
            continue;
        };
        let Some(product_capture) = captures.get(2) else {
            continue;
        };
        let Some(blueprint_capture) = captures.get(3) else {
            continue;
        };
        let Some(reference) = build_asset_reference(
            provider_capture.as_str(),
            product_capture.as_str(),
            blueprint_capture.as_str(),
            trimmed_range(&provider_capture, 0),
            trimmed_range(&product_capture, 0),
            trimmed_range(&blueprint_capture, 0),
        ) else {
            continue;
        };
        references.push((whole.start(), reference));
    }

    references.sort_by_key(|(position, _)| *position);
    references
        .into_iter()
        .map(|(_, reference)| reference)
        .collect()
}

#[cfg(test)]
pub(crate) fn extract_asset_references(xml: &str) -> HashSet<String> {
    extract_asset_references_with_locations(xml)
        .into_iter()
        .map(|reference| reference.path)
        .collect()
}

fn route_tile_label(route_path: &Path, source_file: &Path) -> String {
    let relative = source_file.strip_prefix(route_path).unwrap_or(source_file);
    let stem = source_file
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default();
    let bytes = stem.as_bytes();
    if bytes.len() >= 14 {
        for start in 0..=bytes.len() - 14 {
            let tile = &bytes[start..start + 14];
            if (tile[0] == b'+' || tile[0] == b'-')
                && tile[1..7].iter().all(u8::is_ascii_digit)
                && (tile[7] == b'+' || tile[7] == b'-')
                && tile[8..14].iter().all(u8::is_ascii_digit)
            {
                return format!(
                    "тайл X={} Z={}",
                    String::from_utf8_lossy(&tile[..7]),
                    String::from_utf8_lossy(&tile[7..])
                );
            }
        }
    }
    relative.display().to_string()
}

pub(crate) fn asset_kind(path: &str) -> AssetKind {
    let components: Vec<_> = path.split('/').collect();
    if components
        .iter()
        .any(|component| component.eq_ignore_ascii_case("Networks"))
    {
        AssetKind::Loft
    } else if components
        .iter()
        .any(|component| component.eq_ignore_ascii_case("Scenery"))
    {
        AssetKind::Scenery
    } else {
        AssetKind::Other
    }
}

#[cfg(test)]
mod tests {
    use super::{
        asset_kind, collect_route_files, extract_asset_references_with_locations, has_umlauts,
        has_umlauts_in_xml, inspect_umlaut_asset, leftover_temporary_work_directories,
        referenced_geometry_paths, resolve_asset_path, route_tile_label, transliterate_umlauts,
        transliterate_xml_umlauts, transliterated_asset_path, AssetKind, AssetUse, UmlautAsset,
        TEMP_WORK_DIRECTORY_PREFIX,
    };
    use crate::app::{asset_inventory_rows, InventoryLayout};
    use eframe::egui;
    use std::path::Path;
    use std::path::PathBuf;

    /// Готовит строку таблицы инвентаря с заданным числом тайлов.
    fn inventory_row(tile_count: usize) -> (Vec<AssetUse>, UmlautAsset) {
        let uses: Vec<AssetUse> = (0..tile_count)
            .map(|index| AssetUse {
                source_file: PathBuf::from(format!("route/rail/{index}.bin")),
                tile: format!("тайл {}", index + 1),
            })
            .collect();
        let umlaut = UmlautAsset {
            path: "Scenery/Prüfung.xml".to_string(),
            uses: uses.clone(),
            evidence: vec!["имя содержит умляут".to_string()],
        };
        (uses, umlaut)
    }

    /// Рисует таблицу инвентаря в headless-контексте egui и возвращает её высоту.
    fn render_inventory_height(tile_count: usize) -> f32 {
        let (uses, umlaut) = inventory_row(tile_count);
        let rows: Vec<crate::app::InventoryRow<'_>> = vec![(
            "Scenery/Prüfung.xml",
            uses.as_slice(),
            true,
            Some(&umlaut),
        )];

        let context = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(900.0, 700.0),
            )),
            ..Default::default()
        };
        let layout = InventoryLayout::new(880.0, 8.0, true);
        let mut height = 0.0;
        let _ = context.run(input, |context| {
            egui::CentralPanel::default().show(context, |ui| {
                let mut repair_requested = None;
                asset_inventory_rows(
                    ui,
                    &rows,
                    Path::new("C:\\RailWorks\\Content\\Assets"),
                    &layout,
                    true,
                    false,
                    &mut repair_requested,
                );
                height = ui.min_rect().height();
            });
        });
        height
    }

    #[test]
    fn inventory_row_height_does_not_depend_on_tile_count() {
        // Ассет со всей информацией должен занимать ровно одну строку таблицы.
        let single = render_inventory_height(1);
        let many = render_inventory_height(20);
        assert!(single > 0.0);
        assert!(
            (single - many).abs() < 0.5,
            "высота строки изменилась: {single} против {many}"
        );
    }

    #[test]
    fn excludes_leftover_work_directories_from_route_files() {
        let temp = tempfile::tempdir().unwrap();
        let route = temp.path().join("route");
        std::fs::create_dir_all(route.join("Scenery")).unwrap();
        std::fs::write(route.join("Scenery").join("tile.bin"), b"data").unwrap();
        let leftover = route
            .join("Scenery")
            .join(format!("{TEMP_WORK_DIRECTORY_PREFIX}abcd1234"));
        std::fs::create_dir_all(&leftover).unwrap();
        std::fs::write(leftover.join("source.xml"), b"<route />").unwrap();
        std::fs::write(leftover.join("replacement.bin"), b"data").unwrap();

        let files = collect_route_files(&route, false).unwrap();
        assert_eq!(files.len(), 1);
        assert!(files[0].ends_with("Scenery\\tile.bin") || files[0].ends_with("Scenery/tile.bin"));
        assert_eq!(
            leftover_temporary_work_directories(&route).unwrap(),
            [leftover.display().to_string()]
        );
    }

    #[test]
    fn extracts_locations_for_multiple_asset_references() {
        let xml = r#"
            <Provider>First</Provider><Product>ProductA</Product>
            <BlueprintID>Scenery\first.xml</BlueprintID>
            <Provider>Second</Provider><Product>ProductB</Product>
            <BlueprintID>Networks\second.xml</BlueprintID>
        "#;
        let references = extract_asset_references_with_locations(xml);

        assert_eq!(references.len(), 2);
        assert_eq!(references[0].path, "First/ProductA/Scenery/first.bin");
        assert_eq!(references[1].path, "Second/ProductB/Networks/second.bin");
        assert_eq!(&xml[references[0].provider_range.clone()], "First");
        assert_eq!(&xml[references[1].product_range.clone()], "ProductB");
    }

    #[test]
    fn extracts_references_from_wrapped_absolute_blueprint_blocks() {
        let xml = r#"
            <BlueprintID>
                <iBlueprintLibrary-cAbsoluteBlueprintID>
                    <BlueprintSetID>
                        <iBlueprintLibrary-cBlueprintSetID>
                            <Provider d:type="cDeltaString">JustTrains</Provider>
                            <Product d:type="cDeltaString">CommonLibrary</Product>
                        </iBlueprintLibrary-cBlueprintSetID>
                    </BlueprintSetID>
                    <BlueprintID d:type="cDeltaString">Scenery\Clutter\jt_clutter_pipe_low.xml</BlueprintID>
                </iBlueprintLibrary-cAbsoluteBlueprintID>
            </BlueprintID>
        "#;
        let references = extract_asset_references_with_locations(xml);

        assert_eq!(references.len(), 1);
        assert_eq!(
            references[0].path,
            "JustTrains/CommonLibrary/Scenery/Clutter/jt_clutter_pipe_low.bin"
        );
        assert_eq!(&xml[references[0].provider_range.clone()], "JustTrains");
        assert_eq!(&xml[references[0].product_range.clone()], "CommonLibrary");
        assert_eq!(
            &xml[references[0].blueprint_range.clone()],
            r"Scenery\Clutter\jt_clutter_pipe_low.xml"
        );
    }

    #[test]
    fn empty_reskin_block_does_not_hide_the_next_object_reference() {
        let xml = r#"
            <cDynamicEntity>
                <BlueprintID>
                    <iBlueprintLibrary-cAbsoluteBlueprintID>
                        <BlueprintSetID>
                            <iBlueprintLibrary-cBlueprintSetID>
                                <Provider d:type="cDeltaString">First</Provider>
                                <Product d:type="cDeltaString">ProductA</Product>
                            </iBlueprintLibrary-cBlueprintSetID>
                        </BlueprintSetID>
                        <BlueprintID d:type="cDeltaString">Scenery\first.xml</BlueprintID>
                    </iBlueprintLibrary-cAbsoluteBlueprintID>
                </BlueprintID>
                <ReskinBlueprintID>
                    <iBlueprintLibrary-cAbsoluteBlueprintID>
                        <BlueprintSetID>
                            <iBlueprintLibrary-cBlueprintSetID>
                                <Provider d:type="cDeltaString"></Provider>
                                <Product d:type="cDeltaString"></Product>
                            </iBlueprintLibrary-cBlueprintSetID>
                        </BlueprintSetID>
                        <BlueprintID d:type="cDeltaString"></BlueprintID>
                    </iBlueprintLibrary-cAbsoluteBlueprintID>
                </ReskinBlueprintID>
            </cDynamicEntity>
            <cDynamicEntity>
                <BlueprintID>
                    <iBlueprintLibrary-cAbsoluteBlueprintID>
                        <BlueprintSetID>
                            <iBlueprintLibrary-cBlueprintSetID>
                                <Provider d:type="cDeltaString">Second</Provider>
                                <Product d:type="cDeltaString">ProductB</Product>
                            </iBlueprintLibrary-cBlueprintSetID>
                        </BlueprintSetID>
                        <BlueprintID d:type="cDeltaString">Networks\second.xml</BlueprintID>
                    </iBlueprintLibrary-cAbsoluteBlueprintID>
                </BlueprintID>
                <ReskinBlueprintID>
                    <iBlueprintLibrary-cAbsoluteBlueprintID>
                        <BlueprintSetID>
                            <iBlueprintLibrary-cBlueprintSetID>
                                <Provider d:type="cDeltaString"></Provider>
                                <Product d:type="cDeltaString"></Product>
                            </iBlueprintLibrary-cBlueprintSetID>
                        </BlueprintSetID>
                        <BlueprintID d:type="cDeltaString"></BlueprintID>
                    </iBlueprintLibrary-cAbsoluteBlueprintID>
                </ReskinBlueprintID>
            </cDynamicEntity>
        "#;
        let references = extract_asset_references_with_locations(xml);

        assert_eq!(references.len(), 2);
        assert_eq!(references[0].path, "First/ProductA/Scenery/first.bin");
        assert_eq!(references[1].path, "Second/ProductB/Networks/second.bin");
    }

    #[test]
    fn detects_asset_paths_encoded_as_numeric_xml_entities() {
        let xml = r#"
            <Provider>RWAustria_3</Provider>
            <Product>Marchegger_Ostbahn</Product>
            <BlueprintID>Marchegg\Bruecke Fu&#223;ganger 5.xml</BlueprintID>
        "#;
        let references = extract_asset_references_with_locations(xml);

        assert_eq!(references.len(), 1);
        assert_eq!(
            references[0].path,
            "RWAustria_3/Marchegger_Ostbahn/Marchegg/Bruecke Fußganger 5.bin"
        );
        assert!(has_umlauts(&references[0].path));
    }

    #[test]
    fn detects_the_reported_bruecke_fussganger_asset_path() {
        let asset_path = r"X:\SteamLibrary\steamapps\common\RailWorks\Assets\RWAustria_3\Marchegger_Ostbahn\Marchegg\Bruecke Fußganger 5.bin";

        assert!(has_umlauts(asset_path));
        assert!(has_umlauts_in_xml(
            r"<GeometryID>Bruecke Fu&#223;ganger 5.GeoPcDx</GeometryID>"
        ));
        assert_eq!(
            transliterate_xml_umlauts(r"<GeometryID>Bruecke Fu&#223;ganger 5.GeoPcDx</GeometryID>"),
            r"<GeometryID>Bruecke Fussganger 5.GeoPcDx</GeometryID>"
        );
        let route_xml = r#"
            <Provider>RWAustria_3</Provider>
            <Product>Marchegger_Ostbahn</Product>
            <BlueprintID>Marchegg\Bruecke Fußganger 5.xml</BlueprintID>
        "#;
        let references = extract_asset_references_with_locations(route_xml);
        assert_eq!(references.len(), 1);
        assert_eq!(
            references[0].path,
            "RWAustria_3/Marchegger_Ostbahn/Marchegg/Bruecke Fußganger 5.bin"
        );
    }

    #[test]
    fn detects_corrupt_ffdf_reference_and_resolves_transliterated_asset() {
        let temp = tempfile::tempdir().unwrap();
        let assets = temp.path().join("Assets");
        let actual_relative = r"RWAustria_3\Marchegger_Ostbahn\Marchegg\Bruecke Fussganger 5.bin";
        let actual_path = assets.join(actual_relative);
        std::fs::create_dir_all(actual_path.parent().unwrap()).unwrap();
        std::fs::write(&actual_path, b"fixture").unwrap();

        let xml = "<Provider>RWAustria_3</Provider><Product>Marchegger_Ostbahn</Product>\
             <BlueprintID>Marchegg\\Bruecke Fu\u{FFDF}ganger 5.xml</BlueprintID>"
            .to_string();
        let references = extract_asset_references_with_locations(&xml);
        assert_eq!(references.len(), 1);
        assert!(references[0].path.contains('\u{FFDF}'));
        assert!(has_umlauts(&references[0].path));
        assert_eq!(
            resolve_asset_path(&assets, &references[0].path).unwrap(),
            Some(actual_path)
        );

        let uses = [AssetUse {
            source_file: PathBuf::from(r"C:\Route\Scenery\+000041+000009.bin"),
            tile: "тайл X=+000041 Z=+000009".to_owned(),
        }];
        let (asset, _) = inspect_umlaut_asset(
            &references[0].path,
            &uses,
            &assets,
            Path::new("missing-serz.exe"),
        );
        let asset = asset.expect("corrupted reference must be reported for checking");
        assert!(asset.path.contains('\u{FFDF}'));
        assert_eq!(asset.uses[0].tile, "тайл X=+000041 Z=+000009");
        assert!(!transliterate_umlauts(&references[0].path).contains('\u{FFDF}'));
    }

    #[test]
    fn refuses_ambiguous_ffdf_asset_resolution() {
        let temp = tempfile::tempdir().unwrap();
        let assets = temp.path().join("Assets");
        for file_name in ["Bruecke Fußganger 5.bin", "Bruecke Fussganger 5.bin"] {
            let path = assets
                .join("RWAustria_3")
                .join("Marchegger_Ostbahn")
                .join("Marchegg")
                .join(file_name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"fixture").unwrap();
        }

        let reference = "RWAustria_3/Marchegger_Ostbahn/Marchegg/Bruecke Fu\u{FFDF}ganger 5.bin";
        let error = resolve_asset_path(&assets, reference).unwrap_err();
        assert!(error.contains("совпадает с несколькими файлами"));
    }

    #[test]
    fn reports_the_reported_asset_when_it_exists_in_assets() {
        let temp = tempfile::tempdir().unwrap();
        let assets = temp.path().join("Assets");
        let relative = "RWAustria_3/Marchegger_Ostbahn/Marchegg/Bruecke Fußganger 5.bin";
        let asset_file = assets.join(relative);
        std::fs::create_dir_all(asset_file.parent().unwrap()).unwrap();
        std::fs::write(&asset_file, b"fixture").unwrap();
        let uses = [AssetUse {
            source_file: PathBuf::from(r"C:\Route\Scenery\+000001+000002.bin"),
            tile: "тайл X=+000001 Z=+000002".to_owned(),
        }];
        let (asset, _) =
            inspect_umlaut_asset(relative, &uses, &assets, Path::new("missing-serz.exe"));

        let asset = asset.expect("asset path with ß must be reported");
        assert_eq!(asset.path, relative);
        assert_eq!(asset.uses.len(), 1);
        assert!(asset
            .evidence
            .iter()
            .any(|item| item.contains("Bruecke Fußganger 5.bin")));
    }

    #[test]
    fn classifies_networks_and_scenery_without_confusing_them() {
        assert_eq!(
            asset_kind("Provider/Product/Networks/Track.bin"),
            AssetKind::Loft
        );
        assert_eq!(
            asset_kind("Provider/Product/Scenery/Building.bin"),
            AssetKind::Scenery
        );
        assert_eq!(
            asset_kind("Provider/Product/RailVehicles/Train.bin"),
            AssetKind::Other
        );
    }

    #[test]
    fn extracts_tile_coordinates_from_route_file_names() {
        let route = Path::new(r"C:\Routes\RouteA");
        let source = route.join("Scenery").join("+000012-000034.bin");
        assert_eq!(route_tile_label(route, &source), "тайл X=+000012 Z=-000034");
    }

    #[test]
    fn transliterates_german_umlauts_with_case_preserved() {
        assert_eq!(
            transliterate_umlauts("Ärger öde Übergröße Straße"),
            "Aerger oede Uebergroesse Strasse"
        );
        assert!(has_umlauts("Straße"));
        assert!(!has_umlauts("Strasse"));
    }

    #[test]
    fn detects_geometry_and_texture_references_in_asset_xml() {
        let xml = r#"
            <GeometryID d:type="cDeltaString">RailVehicles\Brücke.GeoPcDx</GeometryID>
            <Texture d:type="cDeltaString">Textures\Bäume.TgPcDx</Texture>
        "#;
        let references = referenced_geometry_paths(xml);

        assert_eq!(references.len(), 2);
        assert_eq!(references[0].0, r"RailVehicles\Brücke.GeoPcDx");
        assert_eq!(references[1].0, r"Textures\Bäume.TgPcDx");
    }

    #[test]
    fn transliterates_asset_file_names_but_rejects_directory_renames() {
        let temp = tempfile::tempdir().unwrap();
        let assets = temp.path().join("Assets");
        let file = assets
            .join("Provider")
            .join("Product")
            .join("Scenery")
            .join("Brücke.bin");
        assert_eq!(
            transliterated_asset_path(&assets, &file).unwrap(),
            assets
                .join("Provider")
                .join("Product")
                .join("Scenery")
                .join("Bruecke.bin")
        );

        let nested_directory = assets
            .join("Provider")
            .join("Product")
            .join("Bäume")
            .join("bridge.bin");
        assert!(transliterated_asset_path(&assets, &nested_directory).is_err());
    }
}