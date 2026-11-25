//! Faults a test injects into a publication. Release builds have none.
//!
//! `CYTIQ_FAILPOINT` holds `<step>|<suffix>|<action>`: at `step` of a target
//! whose path ends in `suffix`, `full` fails as a full disk does, and
//! `pause:<marker>` writes the file `marker` and waits to be killed.

use std::io;
use std::path::Path;

/// Fail or wait when the environment asks for it at `step` of `target`.
///
/// # Errors
/// The injected error.
#[cfg(debug_assertions)]
pub fn hit(step: &str, target: &Path) -> io::Result<()> {
    let Ok(request) = std::env::var("CYTIQ_FAILPOINT") else {
        return Ok(());
    };
    let mut fields = request.splitn(3, '|');
    let (Some(at), Some(suffix), Some(action)) = (fields.next(), fields.next(), fields.next())
    else {
        return Ok(());
    };
    if at != step || !target.to_string_lossy().ends_with(suffix) {
        return Ok(());
    }
    match action.strip_prefix("pause:") {
        Some(marker) => {
            std::fs::write(marker, step)?;
            std::thread::sleep(std::time::Duration::from_secs(120));
            Ok(())
        }
        None => Err(io::Error::from(io::ErrorKind::StorageFull)),
    }
}

/// Nothing: faults exist in debug builds only.
///
/// # Errors
/// Never.
#[cfg(not(debug_assertions))]
#[inline]
pub const fn hit(_step: &str, _target: &Path) -> io::Result<()> {
    Ok(())
}
