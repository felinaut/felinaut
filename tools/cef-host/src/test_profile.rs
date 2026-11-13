//! Strict opt-in for Chromium's public mock key. This is never production policy.
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::{
    io::Read,
    path::{Path, PathBuf},
};
const MARKER: &[u8] = b"felinaut-engine-test-v1\n";

/// Validate explicit native-test opt-in plus a marked, private, fresh temp home.
/// `claim` must be newly created by the caller before CEF is loaded, making reuse
/// fail closed. No environment mutation or user-profile access occurs here.
pub fn validate(opt_in: bool, home: &Path, cocoa: &Path) -> Result<PathBuf, String> {
    let error = || {
        "CEF mock keychain requires FELINAUT_NATIVE_TEST=1 and a fresh marked private engine-test HOME".to_owned()
    };
    if !opt_in {
        return Err(error());
    }
    let root = home.canonicalize().map_err(|_| error())?;
    let temporary = Path::new("/private/tmp")
        .canonicalize()
        .map_err(|_| error())?;
    let metadata = std::fs::symlink_metadata(&root).map_err(|_| error())?;
    if root.parent() != Some(temporary.as_path())
        || !root.file_name().and_then(|p| p.to_str()).is_some_and(|p| {
            p.starts_with("felinaut-engines-test-") && p.len() > "felinaut-engines-test-".len()
        })
        || cocoa.canonicalize().map_err(|_| error())? != root
        || !metadata.is_dir()
        || metadata.permissions().mode() & 0o7777 != 0o700
    {
        return Err(error());
    }
    let marker = root.join(".felinaut-engine-test");
    let marker_meta = std::fs::symlink_metadata(&marker).map_err(|_| error())?;
    if !marker_meta.is_file() || marker_meta.uid() != metadata.uid() || marker_meta.nlink() != 1 {
        return Err(error());
    }
    let mut bytes = Vec::new();
    std::fs::File::open(marker)
        .map_err(|_| error())?
        .take(MARKER.len() as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| error())?;
    if bytes != MARKER {
        return Err(error());
    }
    // A pre-existing user keychain is incompatible with a fresh fixture home.
    if root.join("Library/Keychains").exists() || root.join(".felinaut-cef-test-claimed").exists() {
        return Err(error());
    }
    Ok(root)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mock_key_requires_opt_in_matching_homes_private_mode_and_exact_marker() {
        let root = Path::new("/private/tmp")
            .join(format!("felinaut-engines-test-unit-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let marker = root.join(".felinaut-engine-test");
        assert!(validate(true, &root, &root).is_err());
        std::fs::write(&marker, MARKER).unwrap();
        assert!(validate(true, &root, &root).is_ok());
        assert!(validate(false, &root, &root).is_err());
        assert!(validate(true, &root, Path::new("/private/tmp")).is_err());
        std::fs::write(&marker, b"wrong\n").unwrap();
        assert!(validate(true, &root, &root).is_err());
        std::fs::remove_file(&marker).unwrap();
        std::os::unix::fs::symlink("/dev/null", &marker).unwrap();
        assert!(validate(true, &root, &root).is_err());
        std::fs::remove_file(&marker).unwrap();
        std::fs::write(&marker, MARKER).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(validate(true, &root, &root).is_err());
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(root.join(".felinaut-cef-test-claimed"), b"claimed").unwrap();
        assert!(validate(true, &root, &root).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
