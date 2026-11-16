//! Bounded, validated preset imports and atomic file publication.
use anyhow::{Context, Result, ensure};
use filter_core::preset::{MAX_PRESET_BYTES, Preset};
use std::{
    fs::File,
    io::{Read, Write},
    path::Path,
};

pub fn read(path: &Path) -> Result<Preset> {
    ensure!(
        std::fs::metadata(path)
            .context("Could not inspect preset")?
            .is_file(),
        "Preset must be a regular file"
    );
    let mut options = File::options();
    options.read(true);
    // Prevent an open from blocking if a regular path becomes a FIFO between
    // the path check and open. Check the opened descriptor as well.
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(path).context("Could not open preset")?;
    ensure!(file.metadata()?.is_file(), "Preset must be a regular file");
    ensure!(
        file.metadata()?.len() <= MAX_PRESET_BYTES as u64,
        "Preset exceeds the 16 MiB limit"
    );
    // Bound the actual read as well: metadata can change after the size check.
    let mut bytes = Vec::new();
    file.take(MAX_PRESET_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= MAX_PRESET_BYTES,
        "Preset exceeds the 16 MiB limit"
    );
    let json = std::str::from_utf8(&bytes).context("Preset must be UTF-8 JSON")?;
    Preset::from_json(json).context("Invalid preset")
}

/// The caller owns overwrite confirmation. Validation/writing failures leave an
/// existing file untouched; a completed sibling temporary replaces it atomically.
pub fn write(path: &Path, preset: &Preset) -> Result<()> {
    let json = preset.to_json().context("Invalid preset")?;
    // Pretty printing and programmatic payloads can exceed the import limit.
    ensure!(
        json.len() <= MAX_PRESET_BYTES,
        "Preset exceeds the 16 MiB limit"
    );
    // Resolve an existing final-component link before choosing the sibling temp.
    // Publishing over the selected link would detach it from a shared preset.
    // Broken links are errors rather than silently replacing the link itself.
    let destination = match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Some(std::fs::canonicalize(path).context("Could not resolve preset destination link")?)
        }
        Ok(_) => None,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error).context("Could not inspect preset destination"),
    };
    let path = destination.as_deref().unwrap_or(path);
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut builder = tempfile::Builder::new();
    builder.prefix(".photo-filter-preset-");
    #[cfg(unix)]
    let existing_mode = {
        use std::os::unix::fs::PermissionsExt;
        let mode = match std::fs::metadata(path) {
            Ok(metadata) => Some(metadata.permissions().mode() & 0o777),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error).context("Could not inspect preset destination"),
        };
        builder.permissions(std::fs::Permissions::from_mode(if mode.is_some() {
            0o600
        } else {
            0o666
        }));
        mode
    };
    let mut temporary = builder
        .tempfile_in(parent)
        .context("Could not create preset temporary file")?;
    temporary
        .write_all(json.as_bytes())
        .context("Could not write preset")?;
    #[cfg(unix)]
    if let Some(mode) = existing_mode {
        use std::os::unix::fs::PermissionsExt;
        temporary
            .as_file()
            .set_permissions(std::fs::Permissions::from_mode(mode))
            .context("Could not preserve preset destination permissions")?;
    }
    temporary
        .as_file()
        .sync_all()
        .context("Could not flush preset")?;
    temporary
        .persist(path)
        .map_err(|e| e.error)
        .context("Could not replace preset destination")?;
    Ok(())
}

#[cfg(test)]
#[path = "preset_tests.rs"]
mod tests;
