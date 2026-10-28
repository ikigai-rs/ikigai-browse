//! One per-file byte budget for content reads and content hashing.
use std::io::Read;
use std::path::Path;

use ikigai_core::{Error, Result};

/// Maximum file input for browse content and hashing: 16 MiB (16,777,216 bytes).
/// Larger files fail with `Error::OverBudget`; content and hashes are never truncated.
/// Directory hashing applies this limit to each file, not the aggregate tree.
pub const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;

pub(crate) fn over_budget(path: &Path) -> Error {
    Error::OverBudget {
        detail: format!(
            "browse: file {} exceeds the {MAX_FILE_BYTES}-byte per-file limit",
            path.display()
        ),
    }
}

pub(crate) fn visit_file(path: &Path, visit: impl FnMut(&[u8])) -> Result<()> {
    let file = std::fs::File::open(path)
        .map_err(|e| Error::Endpoint(format!("browse: open {}: {e}", path.display())))?;
    let size = file
        .metadata()
        .map_err(|e| Error::Endpoint(format!("browse: metadata {}: {e}", path.display())))?
        .len();
    if size > MAX_FILE_BYTES {
        return Err(over_budget(path));
    }
    visit_reader(file, path, visit)
}

fn visit_reader(reader: impl Read, path: &Path, mut visit: impl FnMut(&[u8])) -> Result<()> {
    // Metadata is only an early refusal: the file can grow or report no size.
    // Probe one byte beyond the budget to distinguish EOF from truncation.
    let mut reader = reader.take(MAX_FILE_BYTES + 1);
    let mut buffer = [0; 8192];
    let mut total = 0;
    loop {
        let n = match reader.read(&mut buffer) {
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            result => result
                .map_err(|e| Error::Endpoint(format!("browse: read {}: {e}", path.display())))?,
        };
        if n == 0 {
            return Ok(());
        }
        total += n as u64;
        if total > MAX_FILE_BYTES {
            return Err(over_budget(path));
        }
        visit(&buffer[..n]);
    }
}

pub(crate) fn read_file(path: &Path) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    visit_file(path, |chunk| bytes.extend_from_slice(chunk))?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actual_bytes_are_bounded_even_without_metadata() {
        let mut input = std::io::repeat(0).take(MAX_FILE_BYTES + 2);
        let result = visit_reader(&mut input, Path::new("growing-file"), |_| {});
        assert!(matches!(result, Err(Error::OverBudget { .. })));
        assert_eq!(input.limit(), 1, "only the budget plus one byte was read");
    }
}
