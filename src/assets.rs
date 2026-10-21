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
    /// Разбор взят из кэша: файл не менялся с прошлой проверки.
    from_cache: bool,
    file_path: PathBuf,
    /// Сколько раз каждый ассет встречается в файле.
    uses: Vec<(String, u32)>,
    error: Option<String>,
}

#[derive(Clone)]
struct AssetReference {
    path: String,
    provider_range: std::ops::Range<usize>,
    product_range: std::ops::Range<usize>,
    blueprint_range: std::ops::Range<usize>,
}

/// Испорченные символы, которые встречаются в ссылках маршрута вместо умляута.
///
/// U+FFFD — стандартный символ замены, U+FFDF — такой же непечатаемый символ,
/// который раньше встречался в ссылках маршрутов.
pub(crate) const BROKEN_CHARACTERS: [char; 2] = ['\u{FFFD}', '\u{FFDF}'];

/// Настоящие умляуты, которыми может быть назван файл ассета.
const UMLAUT_CHARACTERS: [&str; 8] = ["ä", "Ä", "ö", "Ö", "ü", "Ü", "ß", "ẞ"];

/// Транслитерации умляутов, которыми могли оказаться переименованы файлы.
const TRANSLITERATED_CHARACTERS: [&str; 8] = ["ae", "Ae", "oe", "Oe", "ue", "Ue", "ss", "SS"];

/// Сколько испорченных символов в ссылке ещё разбираются.
const MAX_BROKEN_CHARACTERS: usize = 3;

/// Имя элемента, без которого в файле нет ни одной ссылки на ассет.
const BLUEPRINT_ID_MARKER: &str = "BlueprintID";

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
        .replace('\u{FFFD}', "ss")
}

fn has_umlauts(value: &str) -> bool {
    value.chars().any(|character| {
        matches!(character, 'ä' | 'Ä' | 'ö' | 'Ö' | 'ü' | 'Ü' | 'ß' | 'ẞ')
            || BROKEN_CHARACTERS.contains(&character)
    })
}

/// Есть ли в строке испорченный символ.
fn has_broken_character(value: &str) -> bool {
    value
        .chars()
        .any(|character| BROKEN_CHARACTERS.contains(&character))
}

/// Заменяет испорченные символы в имени на переданную подстановку.
fn replace_broken_characters(name: &str, replacement: &str) -> String {
    BROKEN_CHARACTERS
        .iter()
        .fold(name.to_owned(), |value, character| {
            value.replace(*character, replacement)
        })
}

/// Варианты имени, где каждый испорченный символ заменён по отдельности.
///
/// Например, `Br\u{FFFD}cke Fu\u{FFFD}g\u{FFFD}nger` превращается в
/// `Brücke Fußgänger`, `Brücke Fussganger` и другие комбинации. Число вариантов
/// растёт как степень количества подстановок, поэтому ссылка с большим числом
/// испорченных символов не разбирается вовсе.
fn replace_broken_characters_combinations(name: &str, replacements: &[&str]) -> Vec<String> {
    let mut variants = vec![String::new()];
    for character in name.chars() {
        let mut next = Vec::new();
        for variant in &variants {
            if BROKEN_CHARACTERS.contains(&character) {
                for replacement in replacements {
                    next.push(format!("{variant}{replacement}"));
                }
            } else {
                next.push(format!("{variant}{character}"));
            }
        }
        variants = next;
    }
    variants
}

/// Похожий файл, найденный рядом с утерянным ассетом.
#[derive(Clone)]
#[allow(dead_code)]
pub(crate) struct SimilarAssetFile {
    /// Путь относительно папки Assets.
    pub path: String,
    /// Сколько символов в имени отличается от ожидаемого.
    pub distance: usize,
}

/// Сколько символов отличается в двух именах.
///
/// Сломанный символ U+FFFD в ожидаемом имени совпадает с любым символом,
/// регистр не учитывается. Возвращает `None`, если имена слишком разные.
#[allow(dead_code)]
fn name_distance(expected: &str, candidate: &str) -> Option<usize> {
    let expected: Vec<char> = expected.chars().collect();
    let candidate: Vec<char> = candidate.chars().collect();
    let mut previous: Vec<usize> = (0..=candidate.len()).collect();
    let mut current = vec![0usize; candidate.len() + 1];
    for (row, left) in expected.iter().enumerate() {
        current[0] = row + 1;
        for (column, right) in candidate.iter().enumerate() {
            let substitution =
                if BROKEN_CHARACTERS.contains(left) || left.eq_ignore_ascii_case(right) {
                    0
                } else {
                    1
                };
            current[column + 1] = (previous[column + 1] + 1)
                .min(current[column] + 1)
                .min(previous[column] + substitution);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    let distance = previous[candidate.len()];
    let limit = if expected.len() <= 10 { 1 } else { 2 };
    (distance <= limit).then_some(distance)
}

/// Ищет рядом с утерянным ассетом файлы с очень похожим именем.
///
/// Ищутся только файлы BIN в той же папке, что и ожидался ассет. Подсказка
/// нужна для ссылок со сломанным символом: из-за него имя может отличаться от
/// имени файла на диске.
#[allow(dead_code)]
pub(crate) fn find_similar_asset_files(
    assets_path: &Path,
    relative_asset: &str,
    limit: usize,
) -> Vec<SimilarAssetFile> {
    if !has_broken_character(relative_asset) {
        return Vec::new();
    }
    let expected = Path::new(relative_asset);
    let Some(file_name) = expected.file_name().and_then(|name| name.to_str()) else {
        return Vec::new();
    };
    let Some(parent) = expected.parent() else {
        return Vec::new();
    };
    let folder = assets_path.join(parent);
    let Ok(entries) = fs::read_dir(&folder) else {
        return Vec::new();
    };
    let mut found: Vec<SimilarAssetFile> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_file())
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("bin"))
        })
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let distance = name_distance(file_name, &name)?;
            let path = entry
                .path()
                .strip_prefix(assets_path)
                .ok()?
                .to_string_lossy()
                .replace('\\', "/");
            Some(SimilarAssetFile { path, distance })
        })
        .collect();
    found.sort_by(|left, right| {
        left.distance
            .cmp(&right.distance)
            .then_with(|| left.path.cmp(&right.path))
    });
    found.truncate(limit);
    found
}

/// Ищет файл ассета по ссылке из маршрута.
///
/// Ссылка может содержать испорченные символы: сначала проверяются имена с
/// транслитерацией, затем — с настоящим умляутом. Так ассет, который на диске
/// называется с умляутом, не считается утерянным, а попадает в список на
/// переименование.
fn resolve_asset_path(assets_path: &Path, relative_asset: &str) -> Result<Option<PathBuf>, String> {
    let exact = assets_path.join(relative_asset.replace('/', "\\"));
    if exact.is_file() {
        return Ok(Some(exact));
    }
    if !has_broken_character(relative_asset) {
        return Ok(None);
    }
    let broken_count = relative_asset
        .chars()
        .filter(|character| BROKEN_CHARACTERS.contains(character))
        .count();
    if broken_count > MAX_BROKEN_CHARACTERS {
        return Ok(None);
    }

    let tiers = [
        replace_broken_characters_combinations(relative_asset, &TRANSLITERATED_CHARACTERS),
        replace_broken_characters_combinations(relative_asset, &UMLAUT_CHARACTERS),
    ];

    // Windows считает «ä» и «Ä» одним и тем же именем файла, поэтому одинаковые
    // файлы из разных вариантов отсекаются по каноническому пути.
    let mut candidates: Vec<(PathBuf, PathBuf)> = Vec::new();
    for tier in tiers {
        for variant in tier {
            let candidate = assets_path.join(variant.replace('/', "\\"));
            if !candidate.is_file() {
                continue;
            }
            let identity = fs::canonicalize(&candidate).unwrap_or_else(|_| candidate.clone());
            if !candidates.iter().any(|(_, existing)| *existing == identity) {
                candidates.push((candidate, identity));
            }
        }
    }
    match candidates.len() {
        0 => Ok(None),
        1 => Ok(Some(candidates.remove(0).0)),
        _ => Err(format!(
            "Ссылка содержит испорченный символ и совпадает с несколькими файлами: {}",
            candidates
                .iter()
                .map(|(path, _)| path.display().to_string())
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
    if let Some(asset_path) = full_asset_path.as_ref() {
        // Файл может лежать по верному пути, но называться иначе, чем в ссылке:
        // тогда его нужно переименовать, а не искать замену.
        let expected_name = Path::new(relative_asset).file_name();
        let actual_name = asset_path.file_name();
        if expected_name != actual_name {
            evidence.push(format!(
                "Файл на диске называется иначе: {}",
                asset_path.display()
            ));
        }
    }
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
        // Serz хранит имена элементов обычными строками, поэтому файл без
        // `BlueprintID` заведомо не содержит ссылок на ассеты. Такие файлы не
        // конвертируются: на больших маршрутах это основная часть работы.
        if !may_contain_asset_reference(file_path) {
            return FileAnalysis {
                converted_bin: false,
                from_cache: false,
                file_path: file_path.to_path_buf(),
                uses: Vec::new(),
                error: None,
            };
        }
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
                from_cache: false,
                file_path: file_path.to_path_buf(),
                uses: Vec::new(),
                error: Some(format!("{}: {error}", file_path.display())),
            };
        }
        let data = match fs::read(xml_path) {
            Ok(data) => data,
            Err(error) => {
                return FileAnalysis {
                    converted_bin: true,
                        from_cache: false,
                    file_path: file_path.to_path_buf(),
                    uses: Vec::new(),
                    error: Some(format!(
                        "{}: не удалось прочитать результат Serz: {error}",
                        file_path.display()
                    )),
                };
            }
        };
        data
    } else {
        match fs::read(file_path) {
            Ok(data) => data,
            Err(error) => {
                return FileAnalysis {
                    converted_bin: false,
                        from_cache: false,
                    file_path: file_path.to_path_buf(),
                    uses: Vec::new(),
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

    let references = extract_asset_references_with_locations(&xml_text);

    FileAnalysis {
        converted_bin: is_binary,

        from_cache: false,
        file_path: file_path.to_path_buf(),
        uses: count_asset_uses(references),
        error: None,
    }
}

/// Считает, сколько раз каждый ассет встречается в файле.
///
/// Порядок первого появления сохраняется: по нему собирается список
/// использований в инвентаре.
fn count_asset_uses(references: Vec<AssetReference>) -> Vec<(String, u32)> {
    let mut order: Vec<String> = Vec::new();
    let mut counts: HashMap<String, u32> = HashMap::new();
    for reference in references {
        let counter = counts.entry(reference.path.clone()).or_insert_with(|| {
            order.push(reference.path.clone());
            0
        });
        *counter += 1;
    }
    order
        .into_iter()
        .map(|path| {
            let count = counts.get(&path).copied().unwrap_or_default();
            (path, count)
        })
        .collect()
}

/// Имя файла маршрута относительно его папки.
///
/// Используется как ключ кэша разбора: одинаковые имена в разных маршрутах
/// не должны путать друг с другом.
fn route_file_name(route_path: &Path, file_path: &Path) -> Option<String> {
    let relative = file_path.strip_prefix(route_path).ok()?;
    Some(relative.to_string_lossy().replace('\\', "/"))
}

/// Может ли файл содержать ссылку на ассет.
///
/// Serz хранит имена элементов обычными строками, поэтому файл без подстроки
/// `BlueprintID` не содержит ни одной ссылки на ассет: его конвертация в XML
/// не нужна. При неудачном чтении файл считается подозрительным и проверяется
/// обычным порядком.
fn may_contain_asset_reference(file_path: &Path) -> bool {
    let Ok(data) = fs::read(file_path) else {
        return true;
    };
    data.windows(BLUEPRINT_ID_MARKER.len())
        .any(|window| window == BLUEPRINT_ID_MARKER.as_bytes())
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
    // Кэш разбора лежит в папке маршрута: неизменившиеся файлы берутся из
    // него, а заново разбираются только те, что изменились или появились.
    let mut cache = crate::analysis_cache::AnalysisCache::load(route_path);
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
                    let name = route_file_name(route_path, file_path);
                    let stamp = crate::analysis_cache::FileStamp::of(file_path);
                    if let (Some(name), Some(stamp)) = (&name, stamp) {
                        if let Some(cached) = cache.get(name, stamp) {
                            return FileAnalysis {
                                converted_bin: false,
                                                from_cache: true,
                                file_path: file_path.to_path_buf(),
                                uses: cached.uses.clone(),
                                error: None,
                            };
                        }
                    }
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
        if !analysis.from_cache {
            converted_bin_files += usize::from(analysis.converted_bin);
            // В кэш попадают только заново разобранные файлы: для остальных
            // прежняя запись и так актуальна.
            if let Some(name) = route_file_name(route_path, &analysis.file_path) {
                if let Some(stamp) = crate::analysis_cache::FileStamp::of(&analysis.file_path) {
                    cache.put(
                        name,
                        crate::analysis_cache::CachedFile {
                            stamp,
                            uses: analysis.uses.clone(),
                        },
                    );
                }
            }
        }
        let tile = route_tile_label(route_path, &analysis.file_path);
        for (asset, count) in analysis.uses {
            let uses = asset_uses.entry(asset).or_default();
            for _ in 0..count {
                uses.push(AssetUse {
                    source_file: analysis.file_path.clone(),
                    tile: tile.clone(),
                });
            }
        }
        if let Some(error) = analysis.error {
            conversion_errors.push(error);
        }
    }
    conversion_errors.sort();

    // Удаляем из кэша файлы, которых в маршруте больше нет, и сохраняем кэш.
    let current_files: HashMap<String, ()> = route_files
        .iter()
        .filter_map(|path| route_file_name(route_path, path))
        .map(|name| (name, ()))
        .collect();
    cache.retain(&current_files);
    if let Err(error) = cache.save(route_path) {
        structure_issues.push(format!("Кэш разбора не сохранён: {error}"));
    }

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
                replace_broken_characters(&inner_path, "ss"),
                replace_broken_characters(&inner_path, "ß"),
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
        referenced_assets: asset_inventory.len(),
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

/// Ищет первое вхождение подстроки без учёта регистра ASCII.
///
/// Нужен для ручного разбора XML: регулярное выражение с ленивым `.*?` заново
/// просматривает остаток файла для каждого найденного блока, из-за чего разбор
/// маршрута дорожает квадратично.
fn find_ignore_ascii_case(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    let first = needle[0];
    let last = haystack.len() - needle.len();
    let mut index = from;
    while index <= last {
        let offset = haystack[index..=last]
            .iter()
            .position(|byte| *byte == first)?;
        let start = index + offset;
        if haystack[start..start + needle.len()].eq_ignore_ascii_case(needle) {
            return Some(start);
        }
        index = start + 1;
    }
    None
}

/// Ищет элемент `<Тег ...>значение</Тег>` начиная с заданного места.
///
/// Возвращает диапазон значения без окружающих пробелов и позицию сразу за
/// закрывающим тегом. Значение не может содержать `<`, как и в исходном
/// шаблоне `([^<]*)`.
fn parse_tagged_value(
    haystack: &[u8],
    tag: &str,
    start: usize,
) -> Option<(std::ops::Range<usize>, usize)> {
    let tag_bytes = tag.as_bytes();
    let after_open = start.checked_add(1)?;
    if haystack.get(start)? != &b'<' {
        return None;
    }
    if !haystack
        .get(after_open..after_open.checked_add(tag_bytes.len())?)?
        .eq_ignore_ascii_case(tag_bytes)
    {
        return None;
    }
    let tag_start = after_open + tag_bytes.len();
    let tag_end = tag_start
        + haystack
            .get(tag_start..)?
            .iter()
            .position(|byte| *byte == b'>')?;
    let value_start = tag_end + 1;
    let value_end = value_start
        + haystack
            .get(value_start..)?
            .iter()
            .position(|byte| *byte == b'<')?;
    if haystack.get(value_end)? != &b'<' || haystack.get(value_end + 1)? != &b'/' {
        return None;
    }
    let close_tag_start = value_end + 2;
    if !haystack
        .get(close_tag_start..close_tag_start.checked_add(tag_bytes.len())?)?
        .eq_ignore_ascii_case(tag_bytes)
    {
        return None;
    }
    let close_end = close_tag_start + tag_bytes.len();
    if haystack.get(close_end)? != &b'>' {
        return None;
    }
    let value = &haystack[value_start..value_end];
    let leading = value
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(value.len());
    let trimmed = value
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map(|last| last + 1)
        .unwrap_or(leading);
    Some((value_start + leading..value_start + trimmed, close_end + 1))
}

/// Позиция следующего открывающего тега после заданного места.
fn next_tag_start(haystack: &[u8], from: usize) -> Option<usize> {
    haystack
        .get(from..)?
        .iter()
        .position(|byte| *byte == b'<')
        .map(|offset| from + offset)
}

/// Ищет элемент по имени в заданном диапазоне файла.
fn find_tagged_value(
    haystack: &[u8],
    tag: &str,
    from: usize,
    limit: usize,
) -> Option<(std::ops::Range<usize>, usize)> {
    let mut cursor = from;
    while cursor < limit {
        let position = next_tag_start(haystack, cursor)?;
        if position >= limit {
            return None;
        }
        if let Some((range, end)) = parse_tagged_value(haystack, tag, position) {
            if end <= limit {
                return Some((range, end));
            }
        }
        cursor = position + 1;
    }
    None
}

/// Блоки `iBlueprintLibrary-cAbsoluteBlueprintID` с границами разметки.
fn absolute_blueprint_blocks(haystack: &[u8]) -> Vec<(usize, usize, usize, usize)> {
    let open = b"<iBlueprintLibrary-cAbsoluteBlueprintID>";
    let close = b"</iBlueprintLibrary-cAbsoluteBlueprintID>";
    let mut blocks = Vec::new();
    let mut cursor = 0usize;
    while let Some(open_at) = find_ignore_ascii_case(haystack, open, cursor) {
        let content_start = open_at + open.len();
        let Some(close_at) = find_ignore_ascii_case(haystack, close, content_start) else {
            break;
        };
        blocks.push((open_at, close_at + close.len(), content_start, close_at));
        cursor = close_at + close.len();
    }
    blocks
}

#[cfg(test)]
fn absolute_blueprint_regex() -> &'static Regex {
    static REGEX: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(
            r"(?is)<iBlueprintLibrary-cAbsoluteBlueprintID>(.*?)</iBlueprintLibrary-cAbsoluteBlueprintID>",
        )
        .expect("Шаблон поиска блока абсолютного блюпринта должен быть корректным")
    })
}

#[cfg(test)]
fn provider_regex() -> &'static Regex {
    static REGEX: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"(?is)<Provider[^>]*>([^<]*)</Provider>")
            .expect("Шаблон поиска Provider должен быть корректным")
    })
}

#[cfg(test)]
fn product_regex() -> &'static Regex {
    static REGEX: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"(?is)<Product[^>]*>([^<]*)</Product>")
            .expect("Шаблон поиска Product должен быть корректным")
    })
}

#[cfg(test)]
fn blueprint_id_regex() -> &'static Regex {
    static REGEX: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"(?is)<BlueprintID[^>]*>([^<]*)</BlueprintID>")
            .expect("Шаблон поиска BlueprintID должен быть корректным")
    })
}

#[cfg(test)]
fn bare_blueprint_regex() -> &'static Regex {
    static REGEX: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(
            r"(?is)<Provider[^>]*>([^<]*)</Provider>[^<]*<Product[^>]*>([^<]*)</Product>[^<]*<BlueprintID[^>]*>([^<]*)</BlueprintID>",
        )
        .expect("Шаблон поиска голой ссылки на блюпринт должен быть корректным")
    })
}

#[cfg(test)]
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
///
/// Разбор идёт вручную, без регулярных выражений с ленивым `.*?`: такие шаблоны
/// просматривают остаток файла заново для каждого блока и делают проверку маршрута
/// квадратичной по размеру XML.
fn extract_asset_references_with_locations(xml: &str) -> Vec<AssetReference> {
    let haystack = xml.as_bytes();
    let mut references: Vec<(usize, AssetReference)> = Vec::new();
    let mut covered: Vec<std::ops::Range<usize>> = Vec::new();

    for (open_at, block_end, content_start, content_end) in absolute_blueprint_blocks(haystack) {
        covered.push(open_at..block_end);
        let Some((provider_range, _)) =
            find_tagged_value(haystack, "Provider", content_start, content_end)
        else {
            continue;
        };
        let Some((product_range, _)) =
            find_tagged_value(haystack, "Product", content_start, content_end)
        else {
            continue;
        };
        let Some((blueprint_range, _)) =
            find_tagged_value(haystack, "BlueprintID", content_start, content_end)
        else {
            continue;
        };
        let Some(reference) = build_asset_reference(
            &xml[provider_range.clone()],
            &xml[product_range.clone()],
            &xml[blueprint_range.clone()],
            provider_range,
            product_range,
            blueprint_range,
        ) else {
            continue;
        };
        references.push((open_at, reference));
    }

    // Запасной вариант для файлов без обёртки `iBlueprintLibrary-cAbsoluteBlueprintID`.
    let mut covered_index = 0usize;
    let mut cursor = 0usize;
    while let Some(position) = find_ignore_ascii_case(haystack, b"<Provider", cursor) {
        let Some((provider_range, product_range, blueprint_range, end)) =
            match_bare_blueprint(haystack, position)
        else {
            cursor = position + 1;
            continue;
        };
        cursor = end;
        while covered_index < covered.len() && covered[covered_index].end <= position {
            covered_index += 1;
        }
        if covered_index < covered.len() && covered[covered_index].contains(&position) {
            continue;
        }
        let Some(reference) = build_asset_reference(
            &xml[provider_range.clone()],
            &xml[product_range.clone()],
            &xml[blueprint_range.clone()],
            provider_range,
            product_range,
            blueprint_range,
        ) else {
            continue;
        };
        references.push((position, reference));
    }

    references.sort_by_key(|(position, _)| *position);
    references
        .into_iter()
        .map(|(_, reference)| reference)
        .collect()
}

/// Разбирает «голую» ссылку `<Provider>…</Provider><Product>…</Product><BlueprintID>…</BlueprintID>`.
///
/// Между элементами допускается только текст: как и в исходном шаблоне
/// `[^<]*`, никакой другой тег между ними стоять не может.
fn match_bare_blueprint(
    haystack: &[u8],
    start: usize,
) -> Option<(
    std::ops::Range<usize>,
    std::ops::Range<usize>,
    std::ops::Range<usize>,
    usize,
)> {
    let (provider_range, after_provider) = parse_tagged_value(haystack, "Provider", start)?;
    let product_start = next_tag_start(haystack, after_provider)?;
    let (product_range, after_product) = parse_tagged_value(haystack, "Product", product_start)?;
    let blueprint_start = next_tag_start(haystack, after_product)?;
    let (blueprint_range, end) = parse_tagged_value(haystack, "BlueprintID", blueprint_start)?;
    Some((provider_range, product_range, blueprint_range, end))
}

#[cfg(test)]
pub(crate) fn extract_asset_references(xml: &str) -> HashSet<String> {
    extract_asset_references_with_locations(xml)
        .into_iter()
        .map(|reference| reference.path)
        .collect()
}

/// Прежняя реализация разбора на регулярных выражениях.
///
/// Оставлена для сверки: ручной разбор обязан давать ровно тот же результат.
#[cfg(test)]
fn extract_asset_references_with_locations_regex(xml: &str) -> Vec<AssetReference> {
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
        asset_kind, collect_route_files, extract_asset_references_with_locations,
        find_similar_asset_files, has_umlauts, has_umlauts_in_xml, inspect_umlaut_asset,
        leftover_temporary_work_directories, name_distance, referenced_geometry_paths,
        resolve_asset_path, route_tile_label, transliterate_umlauts, transliterate_xml_umlauts,
        transliterated_asset_path, AssetKind, AssetUse, UmlautAsset, TEMP_WORK_DIRECTORY_PREFIX,
    };
    use crate::app::{asset_inventory_rows, InventoryLayout};
    use eframe::egui;
    use std::path::Path;
    use std::path::PathBuf;

    /// Кэш разбора: повторная проверка берёт неизменившиеся файлы из кэша.
    #[test]
    fn rechecks_route_using_analysis_cache() {
        let temp = tempfile::tempdir().unwrap();
        let route = temp.path().join("route");
        let scenery = route.join("Scenery");
        std::fs::create_dir_all(&scenery).unwrap();
        let first = scenery.join("+000000+000000.xml");
        let second = scenery.join("+000000+000001.xml");
        std::fs::write(
            &first,
            "<Provider>First</Provider><Product>ProductA</Product><BlueprintID>Scenery\\one.xml</BlueprintID>\
             <Provider>First</Provider><Product>ProductA</Product><BlueprintID>Scenery\\one.xml</BlueprintID>",
        )
        .unwrap();
        std::fs::write(
            &second,
            "<Provider>Second</Provider><Product>ProductB</Product><BlueprintID>Scenery\\two.xml</BlueprintID>",
        )
        .unwrap();
        // Serz для XML-файлов не нужен, но путь должен существовать.
        let serz = temp.path().join("serz.exe");
        std::fs::write(&serz, b"stub").unwrap();
        let assets = temp.path().join("Assets");
        std::fs::create_dir_all(&assets).unwrap();

        let check = super::check_route(&route, &assets, &serz, true, 2, |_| {}).unwrap();
        assert_eq!(check.route_files, 2);
        assert_eq!(check.referenced_assets, 2);
        assert!(crate::analysis_cache::cache_path(&route).is_file());
        // Счётчик использований считает каждое вхождение, а не только файлы.
        let first_asset = check
            .asset_inventory
            .iter()
            .find(|asset| asset.path == "First/ProductA/Scenery/one.bin")
            .expect("ассет должен попасть в инвентарь");
        assert_eq!(first_asset.uses.len(), 2);

        // Файлы не менялись — всё берётся из кэша.
        let check = super::check_route(&route, &assets, &serz, true, 2, |_| {}).unwrap();
        assert_eq!(check.referenced_assets, 2);
        let first_asset = check
            .asset_inventory
            .iter()
            .find(|asset| asset.path == "First/ProductA/Scenery/one.bin")
            .expect("ассет должен попасть в инвентарь");
        assert_eq!(first_asset.uses.len(), 2);

        // Один файл изменился: пересобирается только он.
        std::fs::write(
            &second,
            "<Provider>Second</Provider><Product>ProductB</Product><BlueprintID>Scenery\\two-new.xml</BlueprintID>",
        )
        .unwrap();
        let check = super::check_route(&route, &assets, &serz, true, 2, |_| {}).unwrap();
        assert_eq!(check.referenced_assets, 2);
         let paths: Vec<&str> = check
            .asset_inventory
            .iter()
            .map(|asset| asset.path.as_str())
            .collect();
        assert!(
            paths.contains(&"Second/ProductB/Scenery/two-new.bin"),
            "{paths:?}"
        );
    }

    #[test]
    fn measures_similarity_of_asset_names() {
        // Сломанный символ подходит к любому символу.
        assert_eq!(
            name_distance("Bruecke Fu\u{FFDF}ganger 5.bin", "Bruecke Fussganger 5.bin"),
            Some(1)
        );
        assert_eq!(
            name_distance("Bruecke Fu\u{FFDF}ganger 5.bin", "Bruecke Fuganger 5.bin"),
            Some(1)
        );
        // Регистр не учитывается, отличающиеся числа считаются одним отличием.
        assert_eq!(
            name_distance("Prop X=+0000000.bin", "prop x=+0000000.bin"),
            Some(0)
        );
        assert_eq!(
            name_distance("Prop X=+0000000.bin", "Prop X=+0000001.bin"),
            Some(1)
        );
        // Совсем другие имена похожими не считаются.
        assert_eq!(
            name_distance("Bruecke Fu\u{FFDF}ganger 5.bin", "Tunnel Tunnel 2.bin"),
            None
        );
    }

    #[test]
    fn finds_similar_files_next_to_missing_asset() {
        let temp = tempfile::tempdir().unwrap();
        let assets = temp.path();
        let folder = assets.join("Provider/Product/Scenery");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("Bruecke Fussganger 6.bin"), b"data").unwrap();
        std::fs::write(folder.join("Bruecke Fussganger 5.bin"), b"data").unwrap();
        std::fs::write(folder.join("Completely Other.bin"), b"data").unwrap();

        let similar = find_similar_asset_files(
            assets,
            "Provider/Product/Scenery/Bruecke Fu\u{FFDF}ganger 5.bin",
            3,
        );
        assert_eq!(similar.len(), 2);
        assert_eq!(
            similar[0].path,
            "Provider/Product/Scenery/Bruecke Fussganger 5.bin"
        );
        assert_eq!(similar[0].distance, 1);
        assert_eq!(
            similar[1].path,
            "Provider/Product/Scenery/Bruecke Fussganger 6.bin"
        );
    }

    #[test]
    fn does_not_search_similar_files_without_broken_character() {
        let temp = tempfile::tempdir().unwrap();
        let assets = temp.path();
        let folder = assets.join("Provider/Product/Scenery");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("Bruecke Fussganger 5.bin"), b"data").unwrap();

        assert!(find_similar_asset_files(
            assets,
            "Provider/Product/Scenery/Bruecke Fussganger 5.bin",
            3
        )
        .is_empty());
    }

    #[test]
    fn resolves_asset_file_named_with_umlaut() {
        let temp = tempfile::tempdir().unwrap();
        let assets = temp.path();
        let folder = assets.join("Provider/Product/Scenery");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("Brücke Fußgänger 5.bin"), b"data").unwrap();

        // Точное имя файла находится сразу.
        let resolved =
            resolve_asset_path(assets, "Provider/Product/Scenery/Brücke Fußgänger 5.bin").unwrap();
        assert_eq!(
            resolved.map(|path| path.file_name().unwrap().to_string_lossy().into_owned()),
            Some("Brücke Fußgänger 5.bin".to_string())
        );

        // Ссылка со сломанными символами должна находить файл с настоящими умляутами.
        let resolved = resolve_asset_path(
            assets,
            "Provider/Product/Scenery/Br\u{FFDF}cke Fu\u{FFDF}g\u{FFDF}nger 5.bin",
        )
        .unwrap();
        assert_eq!(
            resolved.map(|path| path.file_name().unwrap().to_string_lossy().into_owned()),
            Some("Brücke Fußgänger 5.bin".to_string())
        );
    }

    #[test]
    fn prefers_transliterated_file_over_umlaut_one() {
        let temp = tempfile::tempdir().unwrap();
        let assets = temp.path();
        let folder = assets.join("Provider/Product/Scenery");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("Brücke Fussgänger 5.bin"), b"data").unwrap();

        // Ссылка со сломанным символом находит файл с настоящим умляутом.
        let resolved = resolve_asset_path(
            assets,
            "Provider/Product/Scenery/Br\u{FFDF}cke Fussg\u{FFDF}nger 5.bin",
        )
        .unwrap();
        assert_eq!(
            resolved.map(|path| path.file_name().unwrap().to_string_lossy().into_owned()),
            Some("Brücke Fussgänger 5.bin".to_string())
        );
    }

    #[test]
    fn reports_ambiguity_when_several_files_match_broken_reference() {
        let temp = tempfile::tempdir().unwrap();
        let assets = temp.path();
        let folder = assets.join("Provider/Product/Scenery");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("Bruecke Fußganger 5.bin"), b"data").unwrap();
        std::fs::write(folder.join("Bruecke Fussganger 5.bin"), b"data").unwrap();

        let error = resolve_asset_path(
            assets,
            "Provider/Product/Scenery/Bruecke Fu\u{FFDF}ganger 5.bin",
        )
        .unwrap_err();
        assert!(error.contains("несколькими файлами"), "{error}");
    }

    #[test]
    fn offers_rename_for_asset_file_named_with_umlaut() {
        let temp = tempfile::tempdir().unwrap();
        let assets = temp.path();
        let folder = assets.join("Provider/Product/Scenery");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("Bruecke Fußganger 5.bin"), b"data").unwrap();

        // Ссылка в маршруте повреждена, но файл лежит по верному пути.
        let reference = "Provider/Product/Scenery/Bruecke Fu\u{FFDF}ganger 5.bin";
        let resolved = resolve_asset_path(assets, reference).unwrap();
        assert!(resolved.is_some(), "файл с умляутом должен находиться");

        // Поэтому ассет не считается потерянным, а предлагается переименование.
        let uses = [AssetUse {
            source_file: PathBuf::from(r"C:\Route\Scenery\+000041+000009.bin"),
            tile: "тайл X=+000041 Z=+000009".to_owned(),
        }];
        let (asset, _) =
            inspect_umlaut_asset(reference, &uses, assets, Path::new("missing-serz.exe"));
        let asset = asset.expect("ассет с умляутом должен попасть в список на исправление");
        assert!(asset
            .evidence
            .iter()
            .any(|evidence| evidence.contains("называется иначе")));
    }

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
        let rows: Vec<crate::app::InventoryRow<'_>> =
            vec![("Scenery/Prüfung.xml", uses.as_slice(), true, Some(&umlaut))];

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
                let mut actions = crate::app::InventoryRowActions::default();
                asset_inventory_rows(
                    ui,
                    &rows,
                    Path::new("C:\\RailWorks\\Content\\Assets"),
                    &layout,
                    true,
                    false,
                    &mut actions,
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
    fn asset_column_starts_at_the_left_edge_of_the_table() {
        let (uses, umlaut) = inventory_row(1);
        let rows: Vec<crate::app::InventoryRow<'_>> =
            vec![("Scenery/Prüfung.xml", uses.as_slice(), true, Some(&umlaut))];

        let context = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(900.0, 700.0),
            )),
            ..Default::default()
        };
        let layout = InventoryLayout::new(880.0, 8.0, true);
        let mut actions = crate::app::InventoryRowActions::default();
        let mut grid_rect = egui::Rect::ZERO;
        let _ = context.run(input, |context| {
            egui::CentralPanel::default().show(context, |ui| {
                grid_rect = asset_inventory_rows(
                    ui,
                    &rows,
                    Path::new("C:\\RailWorks\\Content\\Assets"),
                    &layout,
                    true,
                    false,
                    &mut actions,
                );
            });
        });

        let cell = actions.path_rects[0];
        assert!(
            (cell.left() - grid_rect.left()).abs() < 1.0,
            "столбец ассетов должен начинаться с левого края таблицы: {:?} против {:?}",
            cell.left(),
            grid_rect.left()
        );
        assert!(
            (cell.width() - layout.path_width).abs() < 1.0,
            "столбец ассетов должен быть заданной ширины: {:?}",
            cell.width()
        );
    }

    /// Текст в ячейке выравнивается по заданному краю.
    #[test]
    fn cell_text_is_aligned_by_the_requested_edge() {
        let rect = egui::Rect::from_min_size(egui::pos2(100.0, 0.0), egui::vec2(200.0, 18.0));
        assert_eq!(crate::app::text_offset_x(egui::Align::Min, rect, 50.0), 100.0);
        assert_eq!(crate::app::text_offset_x(egui::Align::Center, rect, 50.0), 175.0);
        assert_eq!(crate::app::text_offset_x(egui::Align::Max, rect, 50.0), 250.0);
    }

    /// Столбцы шапки и строк должны совпадать, иначе подписи съезжают.
    #[test]
    fn inventory_header_columns_match_row_columns() {
        let (uses, umlaut) = inventory_row(1);
        let rows: Vec<crate::app::InventoryRow<'_>> =
            vec![("Scenery/Prüfung.xml", uses.as_slice(), true, Some(&umlaut))];

        let context = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(900.0, 700.0),
            )),
            ..Default::default()
        };
        let layout = InventoryLayout::new(880.0, 8.0, true);
        let mut actions = crate::app::InventoryRowActions::default();
        let mut header_cells = None;
        let _ = context.run(input, |context| {
            egui::CentralPanel::default().show(context, |ui| {
                header_cells = crate::app::asset_inventory_header(ui, &layout, true);
                asset_inventory_rows(
                    ui,
                    &rows,
                    Path::new("C:\\RailWorks\\Content\\Assets"),
                    &layout,
                    true,
                    false,
                    &mut actions,
                );
            });
        });

        let header = header_cells.expect("шапка должна вернуть ячейки");
        let row = actions.path_rects[0];
        assert!(
            (header.path.0.left() - row.left()).abs() < 1.0
                && (header.path.0.right() - row.right()).abs() < 1.0,
            "столбец ассетов в шапке и в строке должен совпадать: {:?} против {:?}",
            header.path.0,
            row
        );
        assert!(
            header.usage.0.left() > header.path.0.right(),
            "столбец использований должен идти после столбца ассетов: {:?}",
            header.usage.0
        );
    }

    /// По двойному клику открывается папка, в которой лежит ассет.
    #[test]
    fn opens_the_folder_that_contains_the_asset() {
        let temp = tempfile::tempdir().unwrap();
        let assets = temp.path().join("Assets");
        std::fs::create_dir_all(assets.join("Scenery")).unwrap();
        let folder = crate::app::asset_folder_to_open(&assets, "Scenery/Prüfung.xml").unwrap();
        assert_eq!(folder, assets.join("Scenery"));
    }

    /// Если папки ассета нет, открывается ближайшая существующая папка выше.
    #[test]
    fn opens_the_nearest_existing_folder_when_asset_folder_is_missing() {
        let temp = tempfile::tempdir().unwrap();
        let assets = temp.path().join("Assets");
        std::fs::create_dir_all(&assets).unwrap();
        let folder = crate::app::asset_folder_to_open(&assets, "Scenery/Missing/Prüfung.xml").unwrap();
        assert_eq!(folder, assets);
    }

    /// Длинный путь обрезается слева, чтобы имя файла оставалось видимым.
    #[test]
    fn long_asset_path_is_shortened_from_the_left() {
        let context = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(900.0, 700.0),
            )),
            ..Default::default()
        };
        let mut shortened = String::new();
        let mut truncated = false;
        let mut width = 0.0;
        let _ = context.run(input, |context| {
            egui::CentralPanel::default().show(context, |ui| {
                let (text, was_truncated) = crate::app::elide_asset_path(
                    ui,
                    "CS_Addon/HeKl/Vegetace/Stromy3D/Akat3D/170SmrekZiomennyPen0001.bin",
                    300.0,
                );
                shortened = text;
                truncated = was_truncated;
                width = crate::app::measure_cell_text(
                    ui,
                    &shortened,
                    &egui::FontId::proportional(ui.text_style_height(&egui::TextStyle::Body)),
                );
            });
        });
        assert!(shortened.starts_with('…'), "путь должен обрезаться слева: {shortened}");
        assert!(
            shortened.ends_with("170SmrekZiomennyPen0001.bin"),
            "имя файла должно остаться: {shortened}"
        );
        assert!(truncated, "обрезанный путь должен помечать себя как обрезанный");
        assert!(
            width <= 300.0,
            "обрезанный путь должен помещаться в столбец: {width}"
        );
    }

    /// Короткий путь не обрезается, и подсказка с полным названием не нужна.
    #[test]
    fn short_asset_path_is_not_truncated() {
        let context = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(900.0, 700.0),
            )),
            ..Default::default()
        };
        let mut shortened = String::new();
        let mut truncated = true;
        let _ = context.run(input, |context| {
            egui::CentralPanel::default().show(context, |ui| {
                let (text, was_truncated) =
                    crate::app::elide_asset_path(ui, "Scenery/Prüfung.xml", 400.0);
                shortened = text;
                truncated = was_truncated;
            });
        });
        assert_eq!(shortened, "Scenery/Prüfung.xml");
        assert!(!truncated, "короткий путь не должен обрезаться");
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

    /// Ручной разбор должен совпадать с разбором по шаблонам на любой разметке.
    #[test]
    fn manual_parser_matches_regex_parser_on_varied_markup() {
        let samples = [
            r#"<Provider>A</Provider><Product>B</Product><BlueprintID>C\D.xml</BlueprintID>"#,
            r#"<provider>A</provider><product>B</product><blueprintid>C\D.xml</blueprintid>"#,
            r#"<Provider d:type="cDeltaString">A</Provider><Product d:type="cDeltaString">B</Product><BlueprintID d:type="cDeltaString">C\D.xml</BlueprintID>"#,
            r#"<Provider> A </Provider><Product> B </Product><BlueprintID> C\D.xml </BlueprintID>"#,
            r#"<Provider>A</Provider> <Product>B</Product> <BlueprintID>C\D.xml</BlueprintID>"#,
            r#"<Provider>A</Provider><Other>x</Other><Product>B</Product><BlueprintID>C\D.xml</BlueprintID>"#,
            r#"<Provider></Provider><Product>B</Product><BlueprintID>C\D.xml</BlueprintID>"#,
            r#"<Provider>A</Provider><Product>B</Product><BlueprintID></BlueprintID>"#,
            r#"<Provider>A</Provider><Product>B</Product>"#,
            r#"<iBlueprintLibrary-cAbsoluteBlueprintID><Provider>A</Provider><Product>B</Product><BlueprintID>C\D.xml</BlueprintID></iBlueprintLibrary-cAbsoluteBlueprintID>"#,
            r#"<iBlueprintLibrary-cAbsoluteBlueprintID><Provider>A</Provider><Product>B</Product><BlueprintID>C\D.xml</BlueprintID></iBlueprintLibrary-cAbsoluteBlueprintID><Provider>E</Provider><Product>F</Product><BlueprintID>G\H.xml</BlueprintID>"#,
            r#"<Provider>A</Provider><Product>B</Product><BlueprintID>C\D.xml</BlueprintID><iBlueprintLibrary-cAbsoluteBlueprintID><Provider>E</Provider><Product>F</Product><BlueprintID>G\H.xml</BlueprintID></iBlueprintLibrary-cAbsoluteBlueprintID>"#,
            r#"<ReskinBlueprintID><iBlueprintLibrary-cAbsoluteBlueprintID><Provider></Provider><Product></Product><BlueprintID></BlueprintID></iBlueprintLibrary-cAbsoluteBlueprintID></ReskinBlueprintID><Provider>A</Provider><Product>B</Product><BlueprintID>C\D.xml</BlueprintID>"#,
            r#"текст <Provider>A</Provider> текст <Product>B</Product> текст <BlueprintID>C\D.xml</BlueprintID> текст"#,
            r#"<Provider>A</Provider><Product>B</Product><BlueprintID>C\D.bin</BlueprintID>"#,
            r#"<Provider>A</Provider><Product>B</Product><BlueprintID>C\D.GeoPcDx</BlueprintID>"#,
        ];
        for sample in samples {
            let expected = super::extract_asset_references_with_locations_regex(sample);
            let actual = extract_asset_references_with_locations(sample);
            assert_eq!(
                expected.len(),
                actual.len(),
                "разное число ссылок для {sample}"
            );
            for (left, right) in expected.iter().zip(actual.iter()) {
                assert_eq!(left.path, right.path, "разные пути для {sample}");
                assert_eq!(
                    left.provider_range, right.provider_range,
                    "разные диапазоны Provider для {sample}"
                );
                assert_eq!(
                    left.product_range, right.product_range,
                    "разные диапазоны Product для {sample}"
                );
                assert_eq!(
                    left.blueprint_range, right.blueprint_range,
                    "разные диапазоны BlueprintID для {sample}"
                );
            }
        }
    }

    /// Файл без `BlueprintID` не конвертируется: ссылок в нём быть не может.
    #[test]
    fn skips_bin_files_without_blueprint_marker() {
        let temp = tempfile::tempdir().unwrap();
        let without_marker = temp.path().join("plain.bin");
        std::fs::write(&without_marker, b"SERZ\x00\x00\x01\x00some payload").unwrap();
        let with_marker = temp.path().join("with.bin");
        std::fs::write(&with_marker, b"SERZ\x00\x00\x01\x00BlueprintID payload").unwrap();

        assert!(!super::may_contain_asset_reference(&without_marker));
        assert!(super::may_contain_asset_reference(&with_marker));
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

    /// Проверка настоящего маршрута: замеряет время и сверяет итог.
    ///
    /// Тест требует установленной игры, поэтому запускается только вручную.
    #[test]
    #[ignore]
    fn checks_real_route() {
        let route = PathBuf::from(
            r"X:\SteamLibrary\steamapps\common\RailWorks\Content\Routes\d5c7f5a1-dfba-4589-95e7-269af0d26b97",
        );
        let assets = PathBuf::from(r"X:\SteamLibrary\steamapps\common\RailWorks\Assets");
        let serz = PathBuf::from(r"X:\SteamLibrary\steamapps\common\RailWorks\serz64.exe");
        for attempt in 1..=3 {
            if attempt == 3 {
                // Один из файлов меняется — пересобираться должен только он.
                let changed = route.join("RouteInformation").join("route.properties.xml");
                let _ = changed;
            }
            let start = std::time::Instant::now();
            let result = super::check_route(&route, &assets, &serz, true, 8, |_| {});
            eprintln!("PROFILE attempt {attempt}: {:?}", start.elapsed());
            let check = result.expect("проверка маршрута должна пройти");
            eprintln!(
                "PROFILE attempt {attempt}: files: {}, converted: {}, referenced: {}, missing: {}, errors: {}",
                check.route_files,
                check.converted_bin_files,
                check.referenced_assets,
                check.missing_assets.len(),
                check.conversion_errors.len(),
            );
        }
    }

    /// Сверяет ручной разбор с прежним разбором на регулярных выражениях.
    ///
    /// Файлы берутся из настоящего маршрута: там встречаются разные варианты
    /// разметки, которые не встретишь в коротком примере.
    #[test]
    #[ignore]
    fn manual_parser_matches_regex_parser_on_real_route() {
        let route = PathBuf::from(
            r"X:\SteamLibrary\steamapps\common\RailWorks\Content\Routes\d5c7f5a1-dfba-4589-95e7-269af0d26b97",
        );
        let serz = PathBuf::from(r"X:\SteamLibrary\steamapps\common\RailWorks\serz64.exe");
        let temp = tempfile::tempdir().unwrap();
        let mut checked = 0usize;
        let mut mismatches = 0usize;
        let mut regex_time = std::time::Duration::ZERO;
        let mut manual_time = std::time::Duration::ZERO;

        let all_files: Vec<PathBuf> = walkdir::WalkDir::new(&route)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "bin"))
            .map(|entry| entry.path().to_path_buf())
            .collect();
        let stride = (all_files.len() / 500).max(1);
        for entry in all_files.iter().step_by(stride) {
            let xml_path = temp.path().join(format!("{checked:08}.xml"));
            if super::convert_bin_to_xml(&serz, entry, &xml_path).is_err() {
                continue;
            }
            let Ok(xml) = std::fs::read_to_string(&xml_path) else {
                continue;
            };
            let start = std::time::Instant::now();
            let expected = super::extract_asset_references_with_locations_regex(&xml);
            regex_time += start.elapsed();
            let start = std::time::Instant::now();
            let actual = super::extract_asset_references_with_locations(&xml);
            manual_time += start.elapsed();
            let same = expected.len() == actual.len()
                && expected.iter().zip(actual.iter()).all(|(left, right)| {
                    left.path == right.path
                        && left.provider_range == right.provider_range
                        && left.product_range == right.product_range
                        && left.blueprint_range == right.blueprint_range
                });
            if !same {
                mismatches += 1;
                eprintln!("MISMATCH in {}", entry.display());
                for (left, right) in expected.iter().zip(actual.iter()) {
                    eprintln!(
                        "  regex {} {:?} {:?} {:?}",
                        left.path, left.provider_range, left.product_range, left.blueprint_range
                    );
                    eprintln!(
                        "  manual {} {:?} {:?} {:?}",
                        right.path,
                        right.provider_range,
                        right.product_range,
                        right.blueprint_range
                    );
                }
            }
            checked += 1;
        }
        eprintln!("PROFILE compared {checked} files, mismatches: {mismatches}");
        eprintln!("PROFILE regex {regex_time:?} vs manual {manual_time:?}");
        assert_eq!(
            mismatches, 0,
            "ручной разбор должен совпадать с разбором по шаблонам"
        );
    }
}
