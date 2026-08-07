//! Secure cookie material storage. Cookie values are session secrets: they are
//! stored in a permission-restricted file, never logged, and never placed in
//! diagnostics or notifications.

use std::path::{Path, PathBuf};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum CookieStoreError {
    #[error("cookie store io error: {0}")]
    Io(String),
    #[error("cookie store is empty")]
    Empty,
}

/// File-backed cookie store. The value is never included in error messages.
pub struct CookieStore {
    path: PathBuf,
}

impl CookieStore {
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn exists(&self) -> bool {
        self.path.exists()
    }

    /// Persist cookie material with owner-only permissions (0600 on Unix).
    /// The value is written verbatim; callers pass the raw `Cookie` header.
    pub fn save(&self, cookie_material: &str) -> Result<(), CookieStoreError> {
        if cookie_material.trim().is_empty() {
            return Err(CookieStoreError::Empty);
        }
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| CookieStoreError::Io(e.to_string()))?;
            }
        }
        std::fs::write(&self.path, cookie_material.as_bytes())
            .map_err(|e| CookieStoreError::Io(e.to_string()))?;
        self.restrict_permissions()?;
        tracing::info!(
            event = "session_cookie_saved",
            path = %self.path.display(),
            bytes = cookie_material.len(),
        );
        Ok(())
    }

    /// Load cookie material. The returned string is a secret — do not log it.
    pub fn load(&self) -> Result<String, CookieStoreError> {
        let data =
            std::fs::read_to_string(&self.path).map_err(|e| CookieStoreError::Io(e.to_string()))?;
        if data.trim().is_empty() {
            return Err(CookieStoreError::Empty);
        }
        Ok(data)
    }

    pub fn clear(&self) -> Result<(), CookieStoreError> {
        if self.path.exists() {
            std::fs::remove_file(&self.path).map_err(|e| CookieStoreError::Io(e.to_string()))?;
        }
        Ok(())
    }

    #[cfg(unix)]
    fn restrict_permissions(&self) -> Result<(), CookieStoreError> {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(&self.path, perms).map_err(|e| CookieStoreError::Io(e.to_string()))
    }

    #[cfg(not(unix))]
    fn restrict_permissions(&self) -> Result<(), CookieStoreError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_restricts_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let store = CookieStore::new(dir.path().join("sub/cookies.txt"));
        assert!(!store.exists());
        store.save("SPC_EC=secretvalue; SPC_ST=another").unwrap();
        assert!(store.exists());
        assert_eq!(store.load().unwrap(), "SPC_EC=secretvalue; SPC_ST=another");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(store.path())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        store.clear().unwrap();
        assert!(!store.exists());
    }

    #[test]
    fn empty_material_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let store = CookieStore::new(dir.path().join("c.txt"));
        assert!(matches!(store.save("   "), Err(CookieStoreError::Empty)));
    }

    #[test]
    fn error_messages_never_contain_cookie_value() {
        let dir = tempfile::tempdir().unwrap();
        let store = CookieStore::new(dir.path().join("c.txt"));
        // Saving to an unwritable path surfaces an io error without the value.
        let store_bad = CookieStore::new("/nonexistent-root-xyz/c.txt");
        if let Err(e) = store_bad.save("SPC_EC=topsecret") {
            assert!(!e.to_string().contains("topsecret"));
        }
        drop(store);
    }
}
