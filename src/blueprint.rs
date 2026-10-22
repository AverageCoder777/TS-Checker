//! Разрешение путей блюпринтов подвижного состава на диске.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

thread_local! {
    static BLUEPRINT_CACHE: RefCell<HashMap<(String, String, String), Option<PathBuf>>> =
        RefCell::new(HashMap::new());
}

/// Кэшированная версия resolve_blueprint_file.
pub(crate) fn resolve_blueprint_file_cached_free(
    rw_path: &Path,
    route_path: &Path,
    blueprint: &str,
    provider: &str,
    product: &str,
) -> Option<PathBuf> {
    let key = (
        blueprint.to_owned(),
        provider.to_owned(),
        product.to_owned(),
    );
    BLUEPRINT_CACHE.with(|cell| {
        if let Some(cached) = cell.borrow().get(&key) {
            return cached.clone();
        }
        let found = resolve_blueprint_file(rw_path, route_path, blueprint, provider, product);
        cell.borrow_mut().insert(key, found.clone());
        found
    })
}

/// Разрешает ссылку на блюпринт состава в реальный файл на диске.
/// Ищется относительно папки Assets и папки маршрута, с известными
/// расширениями RailWorks. Если указаны провайдер и продукт, сначала
/// пробуется полный путь Assets\{provider}\{product}\{blueprint}.
/// При неудаче обходится только папка провайдера/продукта (не всё Assets).
pub(crate) fn resolve_blueprint_file(
    rw_path: &Path,
    route_path: &Path,
    blueprint: &str,
    provider: &str,
    product: &str,
) -> Option<PathBuf> {
    let blueprint = blueprint.trim();
    if blueprint.is_empty() {
        return None;
    }
    let assets_path = rw_path.join("Assets");
    let normalized = blueprint.replace('/', "\\");

    let mut candidates: Vec<PathBuf> = Vec::new();
    if !provider.is_empty() && !product.is_empty() {
        let joined = format!(
            "{}\\{}\\{}",
            provider.replace('/', "\\"),
            product.replace('/', "\\"),
            normalized
        );
        candidates.push(PathBuf::from(&joined));
    }
    candidates.push(PathBuf::from(&normalized));
    if let Some(extension) = Path::new(&normalized).extension() {
        let stem = Path::new(&normalized)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| normalized.clone());
        for ext in ["bin", "GeoPcDx", "TgPcDx", "xml"] {
            if ext.eq_ignore_ascii_case(extension.to_str().unwrap_or("")) {
                continue;
            }
            candidates.push(PathBuf::from(format!("{stem}.{ext}")));
        }
    }

    let roots: Vec<PathBuf> = vec![assets_path.clone(), route_path.to_path_buf()];
    for root in &roots {
        for candidate in &candidates {
            let path = root.join(candidate);
            if path.is_file() {
                return Some(path);
            }
        }
    }

    // Fallback: ищем по имени только в папке провайдера/продукта.
    let file_stem = Path::new(&normalized)
        .file_stem()
        .map(|name| name.to_string_lossy().into_owned())?;

    let mut search_roots: Vec<PathBuf> = Vec::new();
    if !provider.is_empty() {
        let provider_dir = assets_path.join(provider.replace('/', "\\"));
        if !product.is_empty() {
            let product_dir = provider_dir.join(product.replace('/', "\\"));
            if product_dir.is_dir() {
                search_roots.push(product_dir);
            }
        }
        if provider_dir.is_dir() {
            search_roots.push(provider_dir);
        }
    }
    if search_roots.is_empty() {
        search_roots.push(assets_path.clone());
    }

    for root in &search_roots {
        if let Some(found) = walkdir::WalkDir::new(root)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_file())
            .find(|e| {
                e.path()
                    .file_stem()
                    .map(|s| s.eq_ignore_ascii_case(&file_stem))
                    .unwrap_or(false)
            })
        {
            return Some(found.into_path());
        }
        // Проверяем архивы .ap в этой папке.
        if let Ok(entries) = fs::read_dir(root) {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_file()
                    || !path
                        .extension()
                        .is_some_and(|e| e.eq_ignore_ascii_case("ap"))
                {
                    continue;
                }
                if let Ok(file) = fs::File::open(&path) {
                    if let Ok(mut zip) = zip::ZipArchive::new(file) {
                        for i in 0..zip.len() {
                            if let Ok(member) = zip.by_index(i) {
                                if member.is_file() {
                                    let member_name =
                                        member.name().map(|c| c.to_string()).unwrap_or_default();
                                    let member_stem = Path::new(&member_name)
                                        .file_stem()
                                        .map(|s| s.to_string_lossy().into_owned())
                                        .unwrap_or_default();
                                    if member_stem.eq_ignore_ascii_case(&file_stem) {
                                        return Some(path.clone());
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    None
}