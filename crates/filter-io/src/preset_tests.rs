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
        format!("{:#}", read(&path).unwrap_err()).contains("unsupported preset schema version 2")
    );
}

#[test]
#[cfg(unix)]
fn imports_reject_directories_devices_and_named_pipes_without_waiting() {
    use std::{
        ffi::CString,
        os::unix::{ffi::OsStrExt, fs::symlink},
    };
    let dir = tempfile::tempdir().unwrap();
    assert!(
        read(dir.path())
            .unwrap_err()
            .to_string()
            .contains("regular file")
    );
    assert!(
        read(Path::new("/dev/null"))
            .unwrap_err()
            .to_string()
            .contains("regular file")
    );
    let fifo = dir.path().join("pipe.json");
    let name = CString::new(fifo.as_os_str().as_bytes()).unwrap();
    // SAFETY: name is a valid nul-terminated pathname, used only by mkfifo.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    assert!(
        read(&fifo)
            .unwrap_err()
            .to_string()
            .contains("regular file")
    );
    let link = dir.path().join("linked-pipe.json");
    symlink(&fifo, &link).unwrap();
    assert!(
        read(&link)
            .unwrap_err()
            .to_string()
            .contains("regular file")
    );
}

#[test]
fn publication_failure_cleans_temporary_and_preserves_destination() {
    let dir = tempfile::tempdir().unwrap();
    let destination = dir.path().join("directory.json");
    std::fs::create_dir(&destination).unwrap();
    std::fs::write(destination.join("keep"), "untouched").unwrap();
    let preset = Preset::capture("Test", &EditState::default(), &Sections::ALL).unwrap();
    assert!(write(&destination, &preset).is_err());
    assert_eq!(
        std::fs::read_to_string(destination.join("keep")).unwrap(),
        "untouched"
    );
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
#[cfg(unix)]
fn new_files_follow_umask_and_replacements_preserve_unix_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("preset.json");
    let reference = dir.path().join("normal.json");
    File::create(&reference).unwrap();
    let preset = Preset::capture("Test", &EditState::default(), &Sections::ALL).unwrap();
    write(&path, &preset).unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        std::fs::metadata(reference).unwrap().permissions().mode() & 0o777
    );
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
    write(&path, &preset).unwrap();
    assert_eq!(
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o640
    );
}

#[test]
#[cfg(unix)]
fn linked_saves_update_target_and_preserve_link_and_target_permissions() {
    use std::os::unix::{fs::PermissionsExt, fs::symlink};
    let dir = tempfile::tempdir().unwrap();
    let shared = dir.path().join("shared");
    let links = dir.path().join("links");
    std::fs::create_dir(&shared).unwrap();
    std::fs::create_dir(&links).unwrap();
    let target = shared.join("preset.json");
    std::fs::write(&target, "old content").unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o640)).unwrap();
    let link = links.join("linked.json");
    symlink("../shared/preset.json", &link).unwrap();
    let preset = Preset::capture("Shared", &EditState::default(), &Sections::ALL).unwrap();
    write(&link, &preset).unwrap();
    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        std::fs::read_link(&link).unwrap(),
        Path::new("../shared/preset.json")
    );
    assert_eq!(read(&target).unwrap(), preset);
    assert_eq!(read(&link).unwrap(), preset);
    assert_eq!(
        std::fs::metadata(target).unwrap().permissions().mode() & 0o777,
        0o640
    );
    assert_eq!(std::fs::read_dir(shared).unwrap().count(), 1);
    assert_eq!(std::fs::read_dir(links).unwrap().count(), 1);
    let broken = dir.path().join("broken.json");
    symlink("missing.json", &broken).unwrap();
    assert!(write(&broken, &preset).is_err());
    assert!(
        std::fs::symlink_metadata(broken)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(!dir.path().join("missing.json").exists());
}
