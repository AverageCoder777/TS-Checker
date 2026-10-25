//! Windows shell integrations such as Explorer and the Recycle Bin.

use std::path::Path;
use std::process::Command;

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
