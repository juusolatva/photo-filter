use super::{MAX_CUBE_BYTES, read_cube};
use std::fs::File;
#[cfg(unix)]
use std::path::Path;

const IDENTITY: &str =
    "TITLE \"Id\"\nLUT_3D_SIZE 2\n0 0 0\n1 0 0\n0 1 0\n1 1 0\n0 0 1\n1 0 1\n0 1 1\n1 1 1\n";

#[test]
fn reads_a_valid_cube_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("look.cube");
    std::fs::write(&path, IDENTITY).unwrap();
    let lut = read_cube(&path).unwrap();
    assert_eq!((lut.size(), lut.title()), (2, "Id"));
    assert_eq!(lut.node([1, 0, 1]), [1.0, 0.0, 1.0]);
}

#[test]
fn parse_errors_carry_line_numbers_but_not_paths() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("secret-name.cube");
    std::fs::write(&path, IDENTITY.replacen("1 0 0", "x 0 0", 1)).unwrap();
    let message = format!("{:#}", read_cube(&path).unwrap_err());
    assert!(message.contains("line 4"), "{message}");
    assert!(!message.contains("secret-name"), "{message}");
    std::fs::write(&path, "LUT_1D_SIZE 4\n").unwrap();
    assert!(
        read_cube(&path)
            .unwrap_err()
            .to_string()
            .contains("1D and combined")
    );
}

#[test]
fn missing_files_and_oversized_files_are_rejected_before_parsing() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("none.cube");
    assert!(
        read_cube(&missing)
            .unwrap_err()
            .to_string()
            .contains("Could not inspect")
    );
    let path = dir.path().join("big.cube");
    File::create(&path)
        .unwrap()
        .set_len(MAX_CUBE_BYTES as u64 + 1)
        .unwrap();
    assert!(read_cube(&path).unwrap_err().to_string().contains("24 MiB"));
    File::create(&path)
        .unwrap()
        .set_len(MAX_CUBE_BYTES as u64)
        .unwrap();
    // At the cap the file is read, then fails to parse rather than for its size.
    let message = read_cube(&path).unwrap_err().to_string();
    assert!(!message.contains("24 MiB"), "{message}");
}

#[test]
#[cfg(unix)]
fn rejects_directories_devices_and_named_pipes_without_waiting() {
    use std::{
        ffi::CString,
        os::unix::{ffi::OsStrExt, fs::symlink},
    };
    let dir = tempfile::tempdir().unwrap();
    let regular = |p: &Path| {
        read_cube(p)
            .unwrap_err()
            .to_string()
            .contains("regular file")
    };
    assert!(regular(dir.path()));
    assert!(regular(Path::new("/dev/null")));
    let fifo = dir.path().join("pipe.cube");
    let name = CString::new(fifo.as_os_str().as_bytes()).unwrap();
    // SAFETY: name is a valid nul-terminated pathname, used only by mkfifo.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    assert!(regular(&fifo));
    let link = dir.path().join("linked-pipe.cube");
    symlink(&fifo, &link).unwrap();
    assert!(regular(&link));
}
