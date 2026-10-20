//! Windows shell integrations such as Explorer and the Recycle Bin.

use super::*;

pub(crate) fn move_folders_to_recycle_bin(
    routes_root: &Path,
    folders: &[PathBuf],
) -> Result<usize, String> {
    if folders.is_empty() {
        return Ok(0);
    }
    let canonical_root = routes_root
        .canonicalize()
        .map_err(|error| format!("Не удалось проверить папку Routes: {error}"))?;

    for folder in folders {
        let metadata = fs::symlink_metadata(folder)
            .map_err(|error| format!("Не удалось проверить {}: {error}", folder.display()))?;
        if !metadata.file_type().is_dir() {
            return Err(format!(
                "Удаление отменено: {} больше не является обычной папкой.",
                folder.display()
            ));
        }
        for entry in WalkDir::new(folder).follow_links(false) {
            let entry = entry.map_err(|error| {
                format!(
                    "Не удалось безопасно проверить {}: {error}",
                    folder.display()
                )
            })?;
            if entry.file_type().is_symlink() {
                return Err(format!(
                    "Удаление отменено: {} содержит ссылку или junction. Удалите её отдельно после проверки.",
                    folder.display()
                ));
            }
        }
        let canonical_folder = folder
            .canonicalize()
            .map_err(|error| format!("Не удалось проверить {}: {error}", folder.display()))?;
        if canonical_folder.parent() != Some(canonical_root.as_path()) {
            return Err(format!(
                "Удаление отменено: {} находится вне папки Content\\Routes.",
                folder.display()
            ));
        }
    }

    move_paths_to_recycle_bin(folders)
}

#[cfg(windows)]
pub(crate) fn move_paths_to_recycle_bin(folders: &[PathBuf]) -> Result<usize, String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::Shell::{
        SHFileOperationW, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FO_DELETE, SHFILEOPSTRUCTW,
    };

    let mut from = Vec::new();
    for path in folders {
        from.extend(legacy_windows_path(path).as_os_str().encode_wide());
        from.push(0);
    }
    from.push(0);
    let mut operation = SHFILEOPSTRUCTW {
        hwnd: std::ptr::null_mut(),
        wFunc: FO_DELETE,
        pFrom: from.as_ptr(),
        pTo: std::ptr::null(),
        fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION) as u16,
        fAnyOperationsAborted: 0,
        hNameMappings: std::ptr::null_mut(),
        lpszProgressTitle: std::ptr::null(),
    };
    let result = unsafe { SHFileOperationW(&mut operation) };
    if result != 0 {
        return Err(format!(
            "Windows не смогла переместить папки в Корзину (код {result})."
        ));
    }
    if operation.fAnyOperationsAborted != 0 {
        return Err("Перемещение папок в Корзину было отменено.".to_owned());
    }
    let remaining_paths: Vec<_> = folders.iter().filter(|path| path.exists()).collect();
    if !remaining_paths.is_empty() {
        let remaining = remaining_paths
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(format!(
            "Windows сообщила об успешной операции, но папки всё ещё существуют: {remaining}. Проверьте права доступа и повторите попытку."
        ));
    }
    Ok(folders.len())
}

#[cfg(not(windows))]
pub(crate) fn move_paths_to_recycle_bin(_folders: &[PathBuf]) -> Result<usize, String> {
    Err("Перемещение в Корзину поддерживается только в Windows.".to_owned())
}

pub(crate) fn open_in_file_explorer(path: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        Command::new("explorer.exe")
            .arg(path)
            .spawn()
            .map(|_| ())
            .map_err(|error| {
                format!(
                    "Не удалось открыть {} в Проводнике: {error}",
                    path.display()
                )
            })
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Err("Открытие Проводника поддерживается только в Windows.".to_owned())
    }
}
