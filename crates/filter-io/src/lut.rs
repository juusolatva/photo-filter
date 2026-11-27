//! Bounded `.cube` file reads. Parsing itself is pure and lives in `filter_core::lut`.
use anyhow::{Context, Result, ensure};
use filter_core::lut::{CubeLut, MAX_CUBE_BYTES, parse_cube};
use std::{fs::File, io::Read, path::Path};

const TOO_LARGE: &str = "LUT file exceeds the 24 MiB limit";

/// Read and parse a `.cube` file. Only a regular file is opened, never waiting on
/// a pipe, and the read is bounded again after the metadata size check. Errors
/// carry the parser's line numbers but never the path.
pub fn read_cube(path: &Path) -> Result<CubeLut> {
    ensure!(
        std::fs::metadata(path)
            .context("Could not inspect LUT file")?
            .is_file(),
        "LUT must be a regular file"
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
    let file = options.open(path).context("Could not open LUT file")?;
    let metadata = file.metadata().context("Could not inspect LUT file")?;
    ensure!(metadata.is_file(), "LUT must be a regular file");
    ensure!(metadata.len() <= MAX_CUBE_BYTES as u64, TOO_LARGE);
    // Metadata can change after the size check, so bound the actual read too.
    let mut bytes = Vec::new();
    file.take(MAX_CUBE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .context("Could not read LUT file")?;
    ensure!(bytes.len() <= MAX_CUBE_BYTES, TOO_LARGE);
    Ok(parse_cube(&bytes)?)
}

#[cfg(test)]
#[path = "../tests/unit/lut_tests.rs"]
mod tests;
