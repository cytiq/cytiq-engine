//! Size-bounded input documents shared by model and manifest readers.
use std::{
    fs::File,
    io::{self, Read},
    path::Path,
};

pub const DOCUMENT_BYTES_MAX: u64 = 16 << 20;

/// Read at most 16 MiB, including files that grow after opening.
///
/// # Errors
/// Returns an I/O error or `InvalidData` when the document exceeds the budget.
pub fn document_bytes(path: &Path) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(DOCUMENT_BYTES_MAX + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > DOCUMENT_BYTES_MAX {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "input document exceeds 16 MiB",
        ));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enforces_document_budget_before_deserialization() {
        let file = tempfile::NamedTempFile::new().unwrap();
        file.as_file().set_len(DOCUMENT_BYTES_MAX + 1).unwrap();
        assert_eq!(
            document_bytes(file.path()).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        std::fs::write(file.path(), br#"{"model":[]} "#).unwrap();
        assert_eq!(document_bytes(file.path()).unwrap(), br#"{"model":[]} "#);
    }
}
