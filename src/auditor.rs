use rayon::prelude::*;
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetResourceKind {
    Geometry,
    Texture,
    Animation,
    Script,
    Sound,
    ChildBlueprint,
}

#[derive(Debug, Clone)]
pub struct BrokenResourceRef {
    pub parent_file: PathBuf,
    pub raw_reference: String,
    pub expected_path: PathBuf,
    pub kind: AssetResourceKind,
}

#[derive(Debug, Default)]
pub struct AuditReport {
    pub broken_references: Vec<BrokenResourceRef>,
    pub scanned_files: usize,
}

#[derive(Default)]
pub struct ApArchiveCache {
    pub inner: Mutex<HashMap<PathBuf, HashSet<String>>>,
}

impl ApArchiveCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&self, product_dir: PathBuf, members: HashSet<String>) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.insert(product_dir, members);
        }
    }

    pub fn contains(&self, product_dir: &Path, relative: &str) -> bool {
        self.contains_path(product_dir, relative)
    }

    pub fn contains_path(&self, product_dir: &Path, relative: &str) -> bool {
        self.exists(product_dir, relative)
    }

    pub fn exists(&self, product_dir: &Path, relative: &str) -> bool {
        self.has(product_dir, relative)
    }

    pub fn has(&self, product_dir: &Path, relative: &str) -> bool {
        self.lookup(product_dir, relative)
    }

    pub fn lookup(&self, product_dir: &Path, relative: &str) -> bool {
        let guard = match self.inner.lock() {
            Ok(guard) => guard,
            Err(_) => return false,
        };
        let normalized = relative.replace('\\', "/").to_lowercase();
        if let Some(set) = guard.get(product_dir) {
            if set.contains(&normalized) {
                return true;
            }
        }
        for (key, set) in guard.iter() {
            if key.to_string_lossy().eq_ignore_ascii_case(&product_dir.to_string_lossy()) {
                if set.contains(&normalized) {
                    return true;
                }
            }
        }
        false
    }

    pub fn get(&self, product_dir: &Path) -> Option<HashSet<String>> {
        let guard = self.inner.lock().ok()?;
        if let Some(set) = guard.get(product_dir) {
            return Some(set.clone());
        }
        for (key, set) in guard.iter() {
            if key.to_string_lossy().eq_ignore_ascii_case(&product_dir.to_string_lossy()) {
                return Some(set.clone());
            }
        }
        None
    }
}

fn regex_tag(tag: &str) -> Regex {
    Regex::new(&format!(r"(?is)<{tag}[^>]*>(.*?)</{tag}>")).unwrap()
}

fn tag_text(xml: &str, tag: &str) -> Option<String> {
    regex_tag(tag)
        .captures(xml)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().trim().to_string())
        .filter(|s| !s.is_empty())
}

fn maybe_sound(raw: &str, fallback: AssetResourceKind) -> AssetResourceKind {
    let lower = raw.to_lowercase();
    if lower.contains("/audio/")
        || lower.contains("\\audio\\")
        || lower.contains("audio")
        || lower.contains("sound")
        || lower.contains(".proxybin")
        || lower.contains(".proxyxml")
        || lower.contains(".dav")
    {
        AssetResourceKind::Sound
    } else {
        fallback
    }
}

fn strip_tags(s: &str) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"<[^>]+>").unwrap());
    re.replace_all(s, "").to_string()
}

pub fn extract_bin_references(xml: &str) -> Vec<(AssetResourceKind, String)> {
    let mut out = Vec::new();
    for tag in ["GeometryID", "ShadowGeometryID"] {
        for value in regex_tag(tag).captures_iter(xml).filter_map(|c| c.get(1)) {
            let raw = value.as_str().trim().to_string();
            if !raw.is_empty() {
                out.push((maybe_sound(&raw, AssetResourceKind::Geometry), raw));
            }
        }
    }
    for tag in ["AnimationID", "AnimationName", "AnimName"] {
        for value in regex_tag(tag).captures_iter(xml).filter_map(|c| c.get(1)) {
            let raw = value.as_str().trim().to_string();
            if !raw.is_empty() {
                out.push((maybe_sound(&raw, AssetResourceKind::Animation), raw));
            }
        }
    }
    for value in regex_tag("ScriptComponent").captures_iter(xml).filter_map(|c| c.get(1)) {
        let raw = strip_tags(value.as_str()).trim().to_string();
        if !raw.is_empty() {
            out.push((maybe_sound(&raw, AssetResourceKind::Script), raw));
        }
    }
    for tag in ["SoundComponent", "cSoundComponentBlueprint"] {
        for value in regex_tag(tag).captures_iter(xml).filter_map(|c| c.get(1)) {
            let raw = strip_tags(value.as_str()).trim().to_string();
            if !raw.is_empty() {
                out.push((AssetResourceKind::Sound, raw));
            }
        }
    }
    for block in regex_tag("iBlueprintLibrary-cAbsoluteBlueprintID")
        .captures_iter(xml)
        .filter_map(|c| c.get(1))
    {
        let inner = block.as_str();
        let provider = tag_text(inner, "Provider").unwrap_or_default();
        let product = tag_text(inner, "Product").unwrap_or_default();
        let mut blueprint = tag_text(inner, "BlueprintID").unwrap_or_default();
        if provider.is_empty() || product.is_empty() || blueprint.is_empty() {
            continue;
        }
        blueprint = blueprint.trim().to_string();
        if blueprint.to_lowercase().ends_with(".xml") {
            blueprint = format!("{}.bin", &blueprint[..blueprint.len() - 4]);
        } else if !blueprint.to_lowercase().ends_with(".bin")
            && !blueprint.to_lowercase().ends_with(".geopcdx")
            && !blueprint.to_lowercase().ends_with(".tgpcdx")
        {
            blueprint = format!("{}.bin", blueprint);
        }
        let raw = format!("{}\\{}\\{}", provider, product, blueprint);
        out.push((maybe_sound(&raw, AssetResourceKind::ChildBlueprint), raw));
    }
    out
}

pub fn extract_references(xml: &str) -> Vec<(AssetResourceKind, String)> {
    extract_bin_references(xml)
}

pub fn extract_geo_texture_references(xml: &str) -> Vec<String> {
    let mut out = Vec::new();
    for tag in ["TextureName", "TextureByName"] {
        for value in regex_tag(tag).captures_iter(xml).filter_map(|c| c.get(1)) {
            let text = value.as_str();
            for item in regex_tag("e").captures_iter(text).filter_map(|c| c.get(1)) {
                let raw = item.as_str().trim().to_string();
                if !raw.is_empty() {
                    out.push(normalize_texture_reference(&raw));
                }
            }
            if !text.contains("<e") {
                let raw = text.trim().to_string();
                if !raw.is_empty() {
                    out.push(normalize_texture_reference(&raw));
                }
            }
        }
    }
    out
}

pub fn extract_texture_references(xml: &str) -> Vec<String> {
    extract_geo_texture_references(xml)
}

pub fn extract_geo_references(xml: &str) -> Vec<String> {
    extract_geo_texture_references(xml)
}

pub fn strip_texture_shader_prefix(raw: &str) -> String {
    strip_shader_prefix(raw)
}

pub fn strip_texture_prefix(raw: &str) -> String {
    strip_shader_prefix(raw)
}

pub fn strip_shader_prefix(raw: &str) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"\[\d{2,3}\]").unwrap());
    re.replace_all(raw, "").to_string()
}

pub fn strip_prefix_00(raw: &str) -> String {
    strip_shader_prefix(raw)
}

pub fn strip_prefix(raw: &str) -> String {
    strip_shader_prefix(raw)
}

pub fn normalize_texture_reference(raw: &str) -> String {
    let stripped = strip_shader_prefix(raw);
    let lower = stripped.to_lowercase();
    if lower.ends_with(".tgpcdx") {
        stripped
    } else {
        format!("{}.TgPcDx", stripped)
    }
}

fn product_root_for(file: &Path, assets_root: &Path) -> PathBuf {
    if let Ok(rel) = file.strip_prefix(assets_root) {
        let comps: Vec<_> = rel.components().collect();
        if comps.len() >= 2 {
            return assets_root.join(comps[0]).join(comps[1]);
        }
    }
    file.parent().unwrap_or(assets_root).to_path_buf()
}

fn ensure_extension(raw: &str, kind: AssetResourceKind) -> String {
    let lower = raw.to_lowercase();
    let has_ext = lower.ends_with(".geopcdx")
        || lower.ends_with(".tgpcdx")
        || lower.ends_with(".bin")
        || lower.ends_with(".xsec")
        || lower.ends_with(".lua")
        || lower.ends_with(".xml")
        || lower.ends_with(".dav")
        || lower.ends_with(".proxybin")
        || lower.ends_with(".proxyxml");
    if has_ext {
        raw.to_string()
    } else {
        match kind {
            AssetResourceKind::Texture => format!("{}.TgPcDx", raw),
            AssetResourceKind::Geometry => format!("{}.GeoPcDx", raw),
            AssetResourceKind::ChildBlueprint => {
                if raw.to_lowercase().ends_with(".xml") {
                    raw.to_string()
                } else {
                    format!("{}.bin", raw)
                }
            }
            _ => raw.to_string(),
        }
    }
}

fn candidate_relatives(raw: &str, kind: AssetResourceKind) -> Vec<String> {
    let cleaned = strip_shader_prefix(raw).replace('/', "\\");
    match kind {
        AssetResourceKind::Script => {
            let lower = cleaned.to_lowercase();
            let stripped = if lower.ends_with(".lua") || lower.ends_with(".out") || lower.ends_with(".outz") {
                // remove last extension
                match cleaned.rfind('.') {
                    Some(i) => cleaned[..i].to_string(),
                    None => cleaned.clone(),
                }
            } else {
                cleaned.clone()
            };
            let mut v = Vec::new();
            v.push(format!("{}.out", stripped));
            v.push(format!("{}.lua", stripped));
            v.push(format!("{}.outz", stripped));
            if lower.ends_with(".lua") {
                v.push(stripped.clone());
                v.push(format!("{}.out", stripped));
            }
            if lower.ends_with(".out") {
                v.push(cleaned.clone());
                v.push(format!("{}.lua", stripped));
            }
            v
        }
        AssetResourceKind::Texture => {
            let with_ext = ensure_extension(&cleaned, kind);
            let mut v = vec![with_ext.clone()];
            v.push(format!("textures\\{}", with_ext));
            v
        }
        AssetResourceKind::Animation => {
            let lower = cleaned.to_lowercase();
            let stripped = if lower.ends_with(".ia") || lower.ends_with(".ban") {
                match cleaned.rfind('.') {
                    Some(i) => cleaned[..i].to_string(),
                    None => cleaned.clone(),
                }
            } else {
                cleaned.clone()
            };
            let mut v = Vec::new();
            v.push(format!("{}.ia", stripped));
            v.push(format!("{}.ban", stripped));
            if lower.ends_with(".ia") || lower.ends_with(".ban") {
                v.push(cleaned.clone());
            }
            v
        }
        AssetResourceKind::ChildBlueprint | AssetResourceKind::Sound => {
            let lower = cleaned.to_lowercase();
            if lower.ends_with(".bin") || lower.ends_with(".xml") || lower.ends_with(".geopcdx") || lower.ends_with(".tgpcdx") {
                vec![cleaned]
            } else {
                vec![format!("{}.bin", cleaned), format!("{}.xml", cleaned)]
            }
        }
        _ => vec![ensure_extension(&cleaned, kind)],
    }
}

fn is_absolute_ref(norm: &str, assets_root: &Path) -> bool {
    let norm = norm.trim_start_matches('/').replace('\\', "/");
    let lower = norm.to_lowercase();
    if lower.starts_with("assets/") {
        return true;
    }
    let parts: Vec<&str> = norm.split('/').filter(|p| !p.is_empty()).collect();
    if parts.len() >= 2 {
        let p1 = parts[0];
        let p2 = parts[1];
        if assets_root.join(p1).join(p2).exists() {
            return true;
        }
        if assets_root.join(p1).is_dir() && assets_root.join(p1).join(p2).is_dir() {
            return true;
        }
    }
    false
}

fn candidate_paths(
    raw: &str,
    parent: &Path,
    product_root: &Path,
    assets_root: &Path,
    kind: AssetResourceKind,
) -> Vec<PathBuf> {
    let mut norm = raw.replace('\\', "/");
    if norm.to_lowercase().starts_with("assets/") {
        norm = norm["assets/".len()..].to_string();
    }
    let norm = strip_shader_prefix(&norm).replace('/', "\\");
    let rels = candidate_relatives(&norm, kind);
    let norm_slash = norm.replace('\\', "/");
    let absolute = is_absolute_ref(&norm_slash, assets_root);
    let mut cands = Vec::new();
    let dirs: Vec<&Path> = if absolute {
        vec![assets_root, parent, product_root]
    } else {
        vec![parent, product_root, assets_root]
    };
    for dir in dirs {
        for rel in &rels {
            cands.push(dir.join(rel));
        }
    }
    let mut seen = HashSet::new();
    cands.retain(|p| seen.insert(p.clone()));
    cands
}

fn loose_exists(candidates: &[PathBuf]) -> bool {
    candidates.iter().any(|p| p.is_file())
}

fn ap_exists(
    _candidates: &[PathBuf],
    raw: &str,
    _parent: &Path,
    product_root: &Path,
    assets_root: &Path,
    ap_cache: &ApArchiveCache,
    kind: AssetResourceKind,
) -> bool {
    for rel in candidate_relatives(raw, kind) {
        let cleaned = rel.replace('/', "\\");
        let parts: Vec<&str> = cleaned.split('\\').filter(|p| !p.is_empty()).collect();
        let (product_dir, inner) = if parts.len() >= 3 {
            (assets_root.join(parts[0]).join(parts[1]), parts[2..].join("/"))
        } else {
            (product_root.to_path_buf(), cleaned.replace('\\', "/"))
        };
        if ap_cache.lookup(&product_dir, &inner) {
            return true;
        }
        if ap_cache.lookup(&product_root, &inner) {
            return true;
        }
    }
    false
}

fn audit_one_file(
    file: &Path,
    folder: &Path,
    assets_root: &Path,
    serz_path: &Path,
    ap_cache: &ApArchiveCache,
) -> Vec<BrokenResourceRef> {
    let mut out = Vec::new();
    let xml = match crate::read_serz_xml(serz_path, file) {
        Ok(x) => x,
        Err(_) => match fs::read(file) {
            Ok(bytes) if bytes.starts_with(b"<?xm") => String::from_utf8_lossy(&bytes).to_string(),
            _ => return out,
        },
    };

    let parent = file.parent().unwrap_or(folder);
    let product_root = product_root_for(file, assets_root);

    let ext = file
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    if ext == "bin" {
        for (kind, raw) in extract_bin_references(&xml) {
            let cands = candidate_paths(&raw, parent, &product_root, assets_root, kind);
            let expected = cands.first().cloned().unwrap_or_else(|| parent.join(&raw));
            let exists = loose_exists(&cands)
                || ap_exists(&cands, &raw, parent, &product_root, assets_root, ap_cache, kind);
            if !exists {
                out.push(BrokenResourceRef {
                    parent_file: file.to_path_buf(),
                    raw_reference: raw,
                    expected_path: expected,
                    kind,
                });
            }
        }
    } else if ext == "geopcdx" {
        for raw in extract_geo_texture_references(&xml) {
            let cands = candidate_paths(&raw, parent, &product_root, assets_root, AssetResourceKind::Texture);
            let expected = cands.first().cloned().unwrap_or_else(|| parent.join(&raw));
            let exists = loose_exists(&cands)
                || ap_exists(&cands, &raw, parent, &product_root, assets_root, ap_cache, AssetResourceKind::Texture);
            if !exists {
                out.push(BrokenResourceRef {
                    parent_file: file.to_path_buf(),
                    raw_reference: raw,
                    expected_path: expected,
                    kind: AssetResourceKind::Texture,
                });
            }
        }
    }

    out
}

pub fn audit_folder(
    folder_path: &Path,
    assets_root: &Path,
    serz_path: &Path,
    max_concurrent: usize,
    ap_cache: &ApArchiveCache,
) -> AuditReport {
    audit_folder_with_progress(folder_path, assets_root, serz_path, max_concurrent, ap_cache, None)
}

pub fn audit_folder_with_progress(
    folder_path: &Path,
    assets_root: &Path,
    serz_path: &Path,
    max_concurrent: usize,
    ap_cache: &ApArchiveCache,
    progress: Option<std::sync::Arc<dyn Fn(usize, usize) + Send + Sync>>,
) -> AuditReport {
    let mut files = Vec::new();
    if let Ok(entries) = walkdir::WalkDir::new(folder_path)
        .into_iter()
        .filter_entry(|e| {
            !e.path()
                .components()
                .any(|c| c.as_os_str().to_string_lossy().eq_ignore_ascii_case("preload"))
        })
        .collect::<Result<Vec<_>, _>>()
    {
        for entry in entries {
            let path = entry.path();
            if path.is_file() {
                let is_preload = path.components().any(|c| c.as_os_str().to_string_lossy().eq_ignore_ascii_case("preload"));
                if is_preload {
                    continue;
                }
                let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
                if ext == "bin" || ext == "geopcdx" {
                    files.push(path.to_path_buf());
                }
            }
        }
    } else {
        collect_files(folder_path, &mut files);
    }

    let threads = max_concurrent.max(1);
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .unwrap_or_else(|_| rayon::ThreadPoolBuilder::new().build().unwrap());

    let processed = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let total_files = files.len();
    let progress_cb = progress.clone();

    let results: Vec<Vec<BrokenResourceRef>> = pool.install(|| {
        files
            .par_iter()
            .map(|f| {
                let r = audit_one_file(f, folder_path, assets_root, serz_path, ap_cache);
                let done = processed.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                if let Some(cb) = &progress_cb {
                    if total_files > 0 && (done == total_files || done % 10 == 0) {
                        cb(done, total_files);
                    }
                }
                r
            })
            .collect()
    });

    let mut broken = Vec::new();
    for r in results {
        broken.extend(r);
    }

    AuditReport {
        broken_references: broken,
        scanned_files: files.len(),
    }
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.components().any(|c| c.as_os_str().to_string_lossy().eq_ignore_ascii_case("preload")) {
                continue;
            }
            if path.is_dir() {
                collect_files(&path, out);
            } else if path.is_file() {
                let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
                if ext == "bin" || ext == "geopcdx" {
                    out.push(path);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_shader_prefix() {
        assert_eq!(strip_shader_prefix(r"textures\[00]body"), r"textures\body");
        assert_eq!(strip_texture_shader_prefix(r"textures\[00]body"), r"textures\body");
        assert_eq!(strip_texture_prefix(r"textures\[00]body"), r"textures\body");
        assert_eq!(strip_prefix_00(r"[00]x"), "x");
        assert_eq!(strip_prefix(r"[00]x"), "x");
    }

    #[test]
    fn normalizes_texture_reference() {
        assert_eq!(normalize_texture_reference(r"textures\[00]body"), r"textures\body.TgPcDx");
        assert_eq!(normalize_texture_reference(r"textures\body.TgPcDx"), r"textures\body.TgPcDx");
    }

    #[test]
    fn extracts_bin_refs() {
        let xml = r#"
            <GeometryID>x\[00]mesh</GeometryID>
            <ShadowGeometryID>y\[01]shadow</ShadowGeometryID>
            <AnimationID>anim\walk</AnimationID>
            <ScriptComponent>scripts\foo.lua</ScriptComponent>
            <iBlueprintLibrary-cAbsoluteBlueprintID>
                <BlueprintSetID><iBlueprintLibrary-cBlueprintSetID>
                    <Provider>Kuju</Provider>
                    <Product>RailSimulator</Product>
                </iBlueprintLibrary-cBlueprintSetID></BlueprintSetID>
                <BlueprintID>child\bar.xml</BlueprintID>
            </iBlueprintLibrary-cAbsoluteBlueprintID>
        "#;
        let refs = extract_bin_references(xml);
        assert!(refs.iter().any(|(k, r)| *k == AssetResourceKind::Geometry && r.contains("mesh")));
        assert!(refs.iter().any(|(k, r)| *k == AssetResourceKind::Animation));
        assert!(refs.iter().any(|(k, r)| *k == AssetResourceKind::Script));
        assert!(refs.iter().any(|(k, r)| *k == AssetResourceKind::ChildBlueprint && r.contains("Kuju") && r.contains("bar.bin")));
        assert!(refs.iter().any(|(k, _)| *k == AssetResourceKind::ChildBlueprint));
    }

    #[test]
    fn sound_classification() {
        let xml = r#"<GeometryID>audio\horn.wav</GeometryID>"#;
        let refs = extract_bin_references(xml);
        assert!(refs.iter().any(|(k, _)| *k == AssetResourceKind::Sound));
    }

    #[test]
    fn extracts_geo_textures() {
        let xml = r#"
            <TextureName>textures\[00]wall</TextureName>
        "#;
        let refs = extract_geo_texture_references(xml);
        assert_eq!(refs, vec![r"textures\wall.TgPcDx".to_string()]);
    }
}
