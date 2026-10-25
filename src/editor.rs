//! XML editor for RailWorks binary assets.

use super::*;

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
    pub(crate) saved_xml: String,
    pub(crate) xml: String,
    pub(crate) xml_validation: Option<Result<(), String>>,
}

impl EditorDocument {
    pub(crate) fn is_dirty(&self) -> bool {
        self.xml != self.saved_xml
    }

    pub(crate) fn validate_xml(&mut self) -> bool {
        let result = validate_xml_document(&self.xml);
        self.xml_validation = Some(result.clone());
        result.is_ok()
    }
}

pub(crate) fn is_viewable_asset_name(name: &str) -> bool {
    Path::new(name).extension().is_some_and(|extension| {
        extension.eq_ignore_ascii_case("bin") || extension.eq_ignore_ascii_case("geopcdx")
    })
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
    Ok(EditorDocument {
        source_path: source_path.to_path_buf(),
        source: source_path.display().to_string(),
        saved_xml: xml.clone(),
        xml,
        xml_validation: None,
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
        install_converted_asset, is_viewable_asset_name, next_backup_path, validate_xml_document,};

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
    fn validates_well_formed_xml_and_reports_malformed_xml() {
        assert!(
            validate_xml_document("<?xml version=\"1.0\"?><root><node id=\"1\"/></root>").is_ok()
        );
        let error = validate_xml_document("<root><node></root>").unwrap_err();
        assert!(error.contains("Некорректный XML"));
    }
}
