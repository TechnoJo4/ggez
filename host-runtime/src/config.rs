use std::{fs::File, io::Read, path::Path};

// Report paths and OS errors, never file contents (callers also use this for keys).
pub fn read_host_file(path: &Path, role: &str, limit: u64) -> Result<Vec<u8>, String> {
    if !path.is_absolute() {
        return Err(format!("{role} {path:?}: path must be absolute"));
    }
    let file =
        File::open(path).map_err(|error| format!("could not open {role} {path:?}: {error}"))?;
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("could not read {role} {path:?}: {error}"))?;
    if bytes.len() as u64 > limit {
        return Err(format!(
            "{role} {path:?}: exceeds size bound of {limit} bytes"
        ));
    }
    Ok(bytes)
}
