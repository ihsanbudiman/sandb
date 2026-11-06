use std::fs::{File, OpenOptions};
use std::path::Path;

use crate::core::error::AppError;

pub struct InstanceLock {
    _file: File,
}

impl InstanceLock {
    pub fn acquire(data_dir: &Path) -> Result<Self, AppError> {
        std::fs::create_dir_all(data_dir)?;
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(data_dir.join(".sandb.lock"))?;
        file.try_lock().map_err(|e| match e {
            std::fs::TryLockError::WouldBlock => {
                AppError::internal("Another sandb instance already uses this data directory")
            }
            std::fs::TryLockError::Error(e) => e.into(),
        })?;
        Ok(Self { _file: file })
    }
}

#[cfg(test)]
mod tests {
    use super::InstanceLock;
    use tempfile::tempdir;

    #[test]
    fn one_owner_per_data_dir_until_released() {
        let first_dir = tempdir().unwrap();
        let other_dir = tempdir().unwrap();
        let first = InstanceLock::acquire(first_dir.path()).unwrap();

        assert!(InstanceLock::acquire(first_dir.path()).is_err());
        let other = InstanceLock::acquire(other_dir.path()).unwrap();
        drop(first);
        assert!(InstanceLock::acquire(first_dir.path()).is_ok());
        drop(other);
    }
}
