use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// Maximum allocation for one remote file-range request, independent of advertised file size.
pub const FILE_TRANSFER_CHUNK_SIZE: u32 = 64 * 1024;

/// Read file contents at a specific offset and length.
pub fn read_file_range(
    path: &Path,
    offset: u64,
    length: u32,
    max_file_size: u64,
) -> anyhow::Result<Vec<u8>> {
    let mut file = fs::File::open(path)?;
    let meta = file.metadata()?;
    anyhow::ensure!(meta.is_file(), "Clipboard source must be a regular file");
    if meta.len() > max_file_size {
        anyhow::bail!("File exceeds maximum size limit");
    }

    let remaining = meta.len().saturating_sub(offset);
    let length = u64::from(length.min(FILE_TRANSFER_CHUNK_SIZE)).min(remaining) as usize;
    if length == 0 {
        return Ok(Vec::new());
    }
    file.seek(SeekFrom::Start(offset))?;
    let mut buf = vec![0u8; length];
    file.read_exact(&mut buf)?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_range_basic() {
        let dir = std::env::temp_dir().join(format!("macrdp-file-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).ok();
        let path = dir.join("test.bin");
        std::fs::write(&path, b"0123456789").unwrap();

        let data = read_file_range(&path, 3, 4, 1024).unwrap();
        assert_eq!(data, b"3456");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn read_range_exceeds_max() {
        let dir = std::env::temp_dir().join(format!("macrdp-file-test2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).ok();
        let path = dir.join("big.bin");
        std::fs::write(&path, vec![0u8; 200]).unwrap();

        let result = read_file_range(&path, 0, 100, 50);
        assert!(result.is_err());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn hostile_request_size_is_bounded_by_chunk_and_remaining_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.bin");
        fs::write(&path, vec![7u8; FILE_TRANSFER_CHUNK_SIZE as usize + 8]).unwrap();
        assert_eq!(
            read_file_range(&path, 0, u32::MAX, u64::MAX).unwrap().len(),
            FILE_TRANSFER_CHUNK_SIZE as usize
        );
        assert_eq!(
            read_file_range(&path, FILE_TRANSFER_CHUNK_SIZE as u64, u32::MAX, u64::MAX).unwrap(),
            vec![7; 8]
        );
        assert!(read_file_range(&path, u64::MAX, u32::MAX, u64::MAX)
            .unwrap()
            .is_empty());
    }
}
