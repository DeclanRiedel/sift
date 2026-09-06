//! Bounded local upload reads, off the UI thread.

use std::io::{self, Read as _};
use std::path::Path;

const MAX_BYTES: u64 = 64 * 1024 * 1024;

pub(super) fn read(path: &Path) -> io::Result<Vec<u8>> {
    let file = std::fs::File::open(path)?;
    if file.metadata()?.len() > MAX_BYTES {
        return Err(too_large());
    }
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(too_large());
    }
    Ok(bytes)
}

fn too_large() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "Transfer input exceeds the 64 MiB payload limit",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_small_files_and_rejects_oversized_sparse_files() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        std::io::Write::write_all(&mut file, b"id\n1\n").unwrap();
        assert_eq!(read(file.path()).unwrap(), b"id\n1\n");
        file.as_file().set_len(MAX_BYTES + 1).unwrap();
        assert_eq!(
            read(file.path()).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
}
