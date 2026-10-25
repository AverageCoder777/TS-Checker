//! XML editor for RailWorks binary assets.

use super::*;
use std::ops::Range;
use std::sync::Arc;

#[derive(Clone, Default)]
pub(crate) struct XmlSyntax {}

impl XmlSyntax {
    pub(crate) fn for_text(_text: &str) -> Self {
        Self::default()
    }
}

pub(crate) fn validate_xml_document(xml: &str) -> Result<(), String> {
    for event in xml::reader::EventReader::new(xml.as_bytes()) {
        event.map_err(|error| format!("Некорректный XML: {error}"))?;
    }
    Ok(())
}

#[derive(Clone)]
pub(crate) struct EditorDocument {
    pub(crate) source_path: PathBuf,
    pub(crate) source: String,
    pub(crate) xml: String,
    pub(crate) saved_xml: String,
    pub(crate) find_query: String,
    pub(crate) replace_text: String,
    pub(crate) case_sensitive: bool,
    pub(crate) match_index: usize,
    pub(crate) match_selected: bool,
    pub(crate) pending_selection: Option<(usize, usize)>,
    pub(crate) search_error: Option<String>,
    pub(crate) xml_validation: Option<Result<(), String>>,
    syntax: Arc<XmlSyntax>,
    match_ranges: Vec<Range<usize>>,
    match_query: String,
    match_case_sensitive: bool,
    match_revision: u64,
    edit_revision: u64,
}

impl EditorDocument {
    pub(crate) fn is_dirty(&self) -> bool {
        self.xml != self.saved_xml
    }

    pub(crate) fn refresh_matches(&mut self) -> Result<(), String> {
        if self.match_revision == self.edit_revision
            && self.match_query == self.find_query
            && self.match_case_sensitive == self.case_sensitive
        {
            return self
                .search_error
                .as_ref()
                .map_or(Ok(()), |error| Err(error.clone()));
        }
        match find_matches(&self.xml, &self.find_query, self.case_sensitive) {
            Ok(matches) => {
                self.match_ranges = matches;
                self.search_error = None;
            }
            Err(error) => {
                self.match_ranges.clear();
                self.search_error = Some(error.clone());
            }
        }
        self.match_query.clone_from(&self.find_query);
        self.match_case_sensitive = self.case_sensitive;
        self.match_revision = self.edit_revision;
        self.match_index = 0;
        self.match_selected = false;
        self.search_error
            .as_ref()
            .map_or(Ok(()), |error| Err(error.clone()))
    }

    pub(crate) fn move_match(&mut self, forward: bool) {
        if self.refresh_matches().is_err() {
            return;
        }
        if self.match_ranges.is_empty() {
            return;
        }
        self.match_index = if !self.match_selected && forward {
            0
        } else if !self.match_selected {
            self.match_ranges.len() - 1
        } else if forward {
            (self.match_index + 1) % self.match_ranges.len()
        } else if self.match_index == 0 {
            self.match_ranges.len() - 1
        } else {
            self.match_index - 1
        };
        self.match_selected = true;
        let range = &self.match_ranges[self.match_index];
        self.pending_selection = Some((
            self.xml[..range.start].chars().count(),
            self.xml[..range.end].chars().count(),
        ));
    }

    pub(crate) fn replace_current(&mut self) -> bool {
        if self.refresh_matches().is_err() {
            return false;
        }
        if !self.match_selected {
            self.match_index = 0;
        }
        let Some(range) = self.match_ranges.get(self.match_index).cloned() else {
            return false;
        };
        self.xml.replace_range(range.clone(), &self.replace_text);
        self.note_text_edited();
        if self.refresh_matches().is_err() {
            return false;
        }
        let selection_start = self.xml[..range.start].chars().count();
        self.pending_selection = Some((
            selection_start,
            selection_start + self.replace_text.chars().count(),
        ));
        self.match_selected = true;
        true
    }

    pub(crate) fn replace_all(&mut self) -> usize {
        if self.refresh_matches().is_err() {
            return 0;
        }
        let ranges = self.match_ranges.clone();
        for range in ranges.iter().rev() {
            self.xml.replace_range(range.clone(), &self.replace_text);
        }
        if !ranges.is_empty() {
            self.note_text_edited();
            if self.refresh_matches().is_err() {
                return 0;
            }
            self.match_index = 0;
            self.match_selected = false;
            self.pending_selection = None;
        }
        ranges.len()
    }

    pub(crate) fn match_count(&self) -> usize {
        self.match_ranges.len()
    }

    pub(crate) fn note_text_edited(&mut self) {
        self.edit_revision = self.edit_revision.wrapping_add(1);
        self.xml_validation = None;
        self.syntax = Arc::new(XmlSyntax::for_text(&self.xml));
    }

    pub(crate) fn validate_xml(&mut self) -> bool {
        let result = validate_xml_document(&self.xml);
        self.xml_validation = Some(result.clone());
        result.is_ok()
    }

    pub(crate) fn matching_tag_ranges(&self, _cursor_char: usize) -> Option<(Range<usize>, Range<usize>)> {
        None
    }

    pub(crate) fn syntax(&self) -> &XmlSyntax {
        &self.syntax
    }
}

pub(crate) fn is_viewable_asset_name(name: &str) -> bool {
    Path::new(name).extension().is_some_and(|extension| {
        extension.eq_ignore_ascii_case("bin") || extension.eq_ignore_ascii_case("geopcdx")
    })
}

fn find_matches(
    text: &str,
    query: &str,
    case_sensitive: bool,
) -> Result<Vec<Range<usize>>, String> {
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let mut builder = regex::RegexBuilder::new(&regex::escape(query));
    builder.case_insensitive(!case_sensitive);
    builder
        .build()
        .map(|regex| {
            regex
                .find_iter(text)
                .map(|matched| matched.range())
                .collect()
        })
        .map_err(|error| format!("Не удалось выполнить поиск: {error}"))
}

pub(crate) fn load_asset_as_xml(
    serz_path: &Path,
    source_path: &Path,
) -> Result<EditorDocument, String> {
    if !is_viewable_asset_name(&source_path.to_string_lossy()) {
        return Err(format!(
            "Открывать для редактирования можно только .bin и .GeoPcDx: {}",
            source_path.display()
        ));
    }

    let temp = tempdir()
        .map_err(|error| format!("Не удалось создать временную папку для просмотра: {error}"))?;
    let xml_path = temp.path().join("view.xml");
    let conversion = convert_bin_to_xml(serz_path, source_path, &xml_path).or_else(|error| {
        let Some(fallback) = fallback_serz_path(serz_path) else {
            return Err(error);
        };
        convert_bin_to_xml(&fallback, source_path, &xml_path).map_err(|fallback_error| {
            format!("{error}; повторная попытка через Serz тоже не удалась: {fallback_error}")
        })
    });
    conversion?;
    let xml = fs::read_to_string(&xml_path)
        .map_err(|error| format!("Не удалось прочитать XML {}: {error}", xml_path.display()))?;
    let syntax = XmlSyntax::for_text(&xml);
    Ok(EditorDocument {
        source_path: source_path.to_path_buf(),
        source: source_path.display().to_string(),
        saved_xml: xml.clone(),
        xml,
        find_query: String::new(),
        replace_text: String::new(),
        case_sensitive: false,
        match_index: 0,
        match_selected: false,
        pending_selection: None,
        search_error: None,
        xml_validation: None,
        syntax: Arc::new(syntax),
        match_ranges: Vec::new(),
        match_query: String::new(),
        match_case_sensitive: false,
        match_revision: 0,
        edit_revision: 0,
    })
}

pub(crate) fn save_asset_xml(
    serz_path: &Path,
    source_path: &Path,
    xml: &str,
) -> Result<Option<PathBuf>, String> {
    validate_xml_document(xml)?;
    if !is_viewable_asset_name(&source_path.to_string_lossy()) {
        return Err(format!(
            "Сохранять можно только .bin и .GeoPcDx: {}",
            source_path.display()
        ));
    }
    let metadata = fs::symlink_metadata(source_path)
        .map_err(|error| format!("Не удалось проверить исходный файл: {error}"))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(format!(
            "Исходный путь не является обычным файлом: {}",
            source_path.display()
        ));
    }
    let parent = source_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let temp = create_temporary_work_directory(parent)?;
    let xml_path = temp.path().join("edited.xml");
    fs::write(&xml_path, xml)
        .map_err(|error| format!("Не удалось записать временный XML: {error}"))?;

    let extension = source_path
        .extension()
        .ok_or_else(|| format!("Не удалось определить тип файла: {}", source_path.display()))?;
    let mut converted_path = temp.path().join("converted");
    converted_path.set_extension(extension);
    let conversion = convert_xml_to_bin(serz_path, &xml_path, &converted_path).or_else(|error| {
        let Some(fallback) = fallback_serz_path(serz_path) else {
            return Err(error);
        };
        convert_xml_to_bin(&fallback, &xml_path, &converted_path).map_err(|fallback_error| {
            format!("{error}; повторная попытка через Serz тоже не удалась: {fallback_error}")
        })
    });
    conversion?;

    install_converted_asset(source_path, &converted_path).map(Some)
}

pub(crate) fn install_converted_asset(
    source_path: &Path,
    converted_path: &Path,
) -> Result<PathBuf, String> {
    let backup_path = next_backup_path(source_path)?;
    fs::rename(source_path, &backup_path).map_err(|error| {
        format!(
            "Не удалось сохранить резервную копию {}: {error}",
            backup_path.display()
        )
    })?;
    if let Err(error) = fs::rename(converted_path, source_path) {
        return match fs::rename(&backup_path, source_path) {
            Ok(()) => Err(format!(
                "Не удалось записать новый файл: {error}. Исходник восстановлен."
            )),
            Err(restore_error) => Err(format!(
                "Не удалось записать новый файл: {error}. Не удалось восстановить исходник из {}: {restore_error}",
                backup_path.display()
            )),
        };
    }
    Ok(backup_path)
}

pub(crate) fn next_backup_path(source_path: &Path) -> Result<PathBuf, String> {
    let file_name = source_path
        .file_name()
        .ok_or_else(|| format!("Некорректный путь к файлу: {}", source_path.display()))?;
    for index in 1_u64.. {
        let mut backup_name = file_name.to_os_string();
        backup_name.push(format!(".bak{index}"));
        let backup_path = source_path.with_file_name(backup_name);
        if !backup_path.exists() {
            return Ok(backup_path);
        }
    }
    Err(format!(
        "Не удалось подобрать имя резервной копии для {}",
        source_path.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::{
        find_matches, install_converted_asset, is_viewable_asset_name, next_backup_path,
        validate_xml_document, EditorDocument, XmlSyntax,
    };
    use std::path::Path;
    use std::sync::Arc;

    #[test]
    fn supports_bin_and_geopcdx_names_case_insensitively() {
        assert!(is_viewable_asset_name("sample.bin"));
        assert!(is_viewable_asset_name("sample.GeoPcDx"));
        assert!(is_viewable_asset_name("folder/SAMPLE.GEOPCDX"));
    }

    #[test]
    fn rejects_other_extensions() {
        assert!(!is_viewable_asset_name("sample.ap"));
        assert!(!is_viewable_asset_name("sample.xml"));
        assert!(!is_viewable_asset_name("sample"));
    }

    #[test]
    fn finds_matches_with_case_option() {
        assert_eq!(find_matches("Tree tree", "tree", false).unwrap().len(), 2);
        assert_eq!(find_matches("Tree tree", "tree", true).unwrap().len(), 1);
        assert!(find_matches("anything", "", false).unwrap().is_empty());
    }

    #[test]
    fn backup_name_appends_number_after_original_extension() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("source.bin");
        std::fs::write(&path, b"original").unwrap();
        std::fs::write(temp.path().join("source.bin.bak1"), b"previous backup").unwrap();
        assert_eq!(
            next_backup_path(&path).unwrap(),
            temp.path().join("source.bin.bak2")
        );
    }

    #[test]
    fn installs_converted_file_and_keeps_original_backup() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source.bin");
        let converted = temp.path().join("converted.bin");
        std::fs::write(&source, b"original").unwrap();
        std::fs::write(&converted, b"edited").unwrap();

        let backup = install_converted_asset(&source, &converted).unwrap();
        assert_eq!(std::fs::read(source).unwrap(), b"edited");
        assert_eq!(std::fs::read(backup).unwrap(), b"original");
    }

    #[test]
    fn restores_original_when_installing_converted_file_fails() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source.bin");
        let missing_converted = temp.path().join("missing.bin");
        std::fs::write(&source, b"original").unwrap();

        assert!(install_converted_asset(&source, &missing_converted).is_err());
        assert_eq!(std::fs::read(source).unwrap(), b"original");
    }

    #[test]
    fn replaces_current_match_and_all_remaining_matches() {
        let mut document = EditorDocument {
            source_path: Path::new("source.bin").to_path_buf(),
            source: "source.bin".to_owned(),
            xml: "cat CAT cat".to_owned(),
            saved_xml: "cat CAT cat".to_owned(),
            find_query: "cat".to_owned(),
            replace_text: "dog".to_owned(),
            case_sensitive: false,
            match_index: 0,
            match_selected: false,
            pending_selection: None,
            search_error: None,
            xml_validation: None,
            syntax: Arc::new(XmlSyntax::for_text("cat CAT cat")),
            match_ranges: Vec::new(),
            match_query: String::new(),
            match_case_sensitive: false,
            match_revision: 0,
            edit_revision: 0,
        };
        assert!(document.replace_current());
        assert_eq!(document.xml, "dog CAT cat");
        assert_eq!(document.replace_all(), 2);
        assert_eq!(document.xml, "dog dog dog");
        assert!(document.matching_tag_ranges(2).is_none());
        assert_eq!(document.xml_validation, None);
    }

    #[test]
    fn validates_well_formed_xml_and_reports_malformed_xml() {
        assert!(
            validate_xml_document("<?xml version=\"1.0\"?><root><node id=\"1\"/></root>").is_ok()
        );
        let error = validate_xml_document("<root><node></root>").unwrap_err();
        assert!(error.contains("Некорректный XML"));
    }

    
}
