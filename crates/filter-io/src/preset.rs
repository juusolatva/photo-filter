//! Bounded, validated preset imports and atomic file publication.
use anyhow::{Context, Result, ensure};
use filter_core::preset::{MAX_PRESET_BYTES, Preset};
use std::{
    fs::File,
    io::{Read, Write},
    path::Path,
};

pub fn read(path: &Path) -> Result<Preset> {
    let file = File::open(path).context("Could not open preset")?;
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
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut temporary = tempfile::Builder::new()
        .prefix(".photo-filter-preset-")
        .tempfile_in(parent)
        .context("Could not create preset temporary file")?;
    temporary
        .write_all(json.as_bytes())
        .context("Could not write preset")?;
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
mod tests {
    use super::*;
    use filter_core::{edit::EditState, preset::Sections};

    #[test]
    fn partial_preset_round_trip_and_atomic_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("värit.pfpreset.json");
        let mut edit = EditState::default();
        edit.calibration.exposure = 1.25;
        let preset = Preset::capture(
            "Värit",
            &edit,
            &Sections {
                calibration: true,
                ..Default::default()
            },
        )
        .unwrap();
        std::fs::write(&path, "old content").unwrap();
        write(&path, &preset).unwrap();
        assert_eq!(read(&path).unwrap(), preset);
        assert!(preset.patch.tone.is_none());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn invalid_or_oversized_save_preserves_destination() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("saved.json");
        std::fs::write(&path, "keep me").unwrap();
        let mut preset = Preset::capture("Test", &EditState::default(), &Sections::ALL).unwrap();
        preset.patch.calibration.as_mut().unwrap().exposure = Some(99.0);
        assert!(write(&path, &preset).is_err());
        preset.patch.calibration.as_mut().unwrap().exposure = Some(0.0);
        preset.name = "x".repeat(MAX_PRESET_BYTES);
        assert!(write(&path, &preset).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "keep me");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn imports_reject_oversized_invalid_utf8_and_new_versions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.json");
        File::create(&path)
            .unwrap()
            .set_len(MAX_PRESET_BYTES as u64 + 1)
            .unwrap();
        assert!(read(&path).unwrap_err().to_string().contains("16 MiB"));
        std::fs::write(&path, [0xff]).unwrap();
        assert!(read(&path).unwrap_err().to_string().contains("UTF-8"));
        std::fs::write(&path, r#"{"schema_version":2}"#).unwrap();
        assert!(
            format!("{:#}", read(&path).unwrap_err())
                .contains("unsupported preset schema version 2")
        );
    }
}
