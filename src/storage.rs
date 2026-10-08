use crate::parcel;
use anyhow::{Context, Result, ensure};
use std::{
    fs::{self, File, Metadata, TryLockError},
    io,
    path::Path,
};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct StorageUsage {
    pub downloads_bytes: u64,
    pub uploads_bytes: u64,
    pub sent_bytes: u64,
    pub settings_bytes: u64,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CleanupReport {
    pub removed_bytes: u64,
    pub removed_caches: usize,
    pub skipped_caches: usize,
    pub failed_caches: usize,
}

pub fn usage(root: &Path) -> Result<StorageUsage> {
    if !check_root(root)? {
        return Ok(StorageUsage::default());
    }
    let settings = root.join("settings.json");
    let settings_bytes = match optional_metadata(&settings)? {
        Some(metadata) if !is_link(&metadata) && metadata.is_file() => metadata.len(),
        _ => 0,
    };
    Ok(StorageUsage {
        downloads_bytes: directory_bytes(&root.join("downloads"))?,
        uploads_bytes: directory_bytes(&root.join("uploads"))?,
        sent_bytes: directory_bytes(&root.join("sent"))?,
        settings_bytes,
    })
}

pub fn clear_inactive_downloads(root: &Path) -> Result<CleanupReport> {
    let mut report = CleanupReport::default();
    if !check_cleanup_roots(root)? {
        return Ok(report);
    }
    let downloads = root.join("downloads");
    for entry in fs::read_dir(&downloads)
        .with_context(|| format!("Could not list {}", downloads.display()))?
    {
        let entry = entry.with_context(|| format!("Could not list {}", downloads.display()))?;
        let name = entry.file_name();
        let result = match name.to_str() {
            Some(key) if cache_key(key) => clear_cache(root, key, &mut report.removed_bytes),
            Some(name) if name.strip_suffix(".json").is_some_and(cache_key) => {
                let key = name
                    .strip_suffix(".json")
                    .expect("checkpoint suffix checked");
                clear_checkpoint(root, key, &mut report.removed_bytes)
            }
            _ => Ok(false),
        };
        match result {
            Ok(true) => report.removed_caches += 1,
            Ok(false) => report.skipped_caches += 1,
            Err(_) => report.failed_caches += 1,
        }
    }
    Ok(report)
}

fn clear_cache(root: &Path, key: &str, removed_bytes: &mut u64) -> Result<bool> {
    ensure!(
        check_cleanup_roots(root)?,
        "Storage directories disappeared."
    );
    let cache = root.join("downloads").join(key);
    let metadata = inspect(&cache)?;
    if is_link(&metadata) || !metadata.is_dir() {
        return Ok(false);
    }
    let Some(_lock) = try_download_lock(root, key)? else {
        return Ok(false);
    };

    ensure!(
        check_cleanup_roots(root)?,
        "Storage directories disappeared."
    );
    ensure!(
        plain_directory(&cache)?,
        "Cache directory disappeared: {}",
        cache.display()
    );
    let mut parts = Vec::new();
    for entry in
        fs::read_dir(&cache).with_context(|| format!("Could not list {}", cache.display()))?
    {
        let entry = entry.with_context(|| format!("Could not list {}", cache.display()))?;
        let metadata = inspect(&entry.path())?;
        if !entry.file_name().to_str().is_some_and(part_name)
            || is_link(&metadata)
            || !metadata.is_file()
        {
            return Ok(false);
        }
        parts.push(entry.path());
    }
    if parts.is_empty() {
        return Ok(false);
    }
    for part in parts {
        ensure!(
            check_cleanup_roots(root)? && plain_directory(&cache)?,
            "Storage directories disappeared."
        );
        let metadata = inspect(&part)?;
        ensure!(
            !is_link(&metadata) && metadata.is_file(),
            "Cache part changed: {}",
            part.display()
        );
        let total = removed_bytes
            .checked_add(metadata.len())
            .context("Removed byte count exceeds u64.")?;
        fs::remove_file(&part).with_context(|| format!("Could not remove {}", part.display()))?;
        *removed_bytes = total;
    }
    ensure!(
        check_cleanup_roots(root)? && plain_directory(&cache)?,
        "Storage directories disappeared."
    );
    fs::remove_dir(&cache)
        .with_context(|| format!("Could not remove empty cache {}", cache.display()))?;
    Ok(true)
}

pub(crate) fn try_download_lock(root: &Path, key: &str) -> Result<Option<File>> {
    ensure!(cache_key(key), "Invalid transfer identity.");
    ensure!(check_root(root)?, "Storage directory disappeared.");
    plain_directory(&root.join("locks"))?;
    let lock_path = root.join("locks").join(format!("download-{key}"));
    if let Some(metadata) = optional_metadata(&lock_path)?
        && (is_link(&metadata) || !metadata.is_file())
    {
        return Ok(None);
    }
    match parcel::lock_transfer(&lock_path) {
        Ok(lock) => Ok(Some(lock)),
        Err(error)
            if matches!(
                error.downcast_ref::<TryLockError>(),
                Some(TryLockError::WouldBlock)
            ) =>
        {
            Ok(None)
        }
        Err(error) => Err(error).with_context(|| format!("Could not lock {}", lock_path.display())),
    }
}

fn clear_checkpoint(root: &Path, key: &str, removed_bytes: &mut u64) -> Result<bool> {
    ensure!(
        check_cleanup_roots(root)?,
        "Download directory disappeared."
    );
    let path = root.join("downloads").join(format!("{key}.json"));
    let metadata = inspect(&path)?;
    if is_link(&metadata) || !metadata.is_file() {
        return Ok(false);
    }
    crate::low_disk::clear_checkpoint(root, key, removed_bytes)
}

fn cache_key(name: &str) -> bool {
    name.len() == 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn part_name(name: &str) -> bool {
    name.strip_suffix(".part").is_some_and(|index| {
        index.len() == 6 && index.bytes().all(|byte| byte.is_ascii_digit()) && index != "000000"
    })
}

fn inspect(path: &Path) -> Result<Metadata> {
    fs::symlink_metadata(path).with_context(|| format!("Could not inspect {}", path.display()))
}

fn optional_metadata(path: &Path) -> Result<Option<Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("Could not inspect {}", path.display())),
    }
}

fn is_link(metadata: &Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

fn plain_directory(path: &Path) -> Result<bool> {
    let Some(metadata) = optional_metadata(path)? else {
        return Ok(false);
    };
    ensure!(
        !is_link(&metadata),
        "Refusing linked/reparse directory {}",
        path.display()
    );
    ensure!(
        metadata.is_dir(),
        "Expected a directory: {}",
        path.display()
    );
    Ok(true)
}

pub(crate) fn check_root(root: &Path) -> Result<bool> {
    let ancestors: Vec<_> = root
        .ancestors()
        .filter(|path| !path.as_os_str().is_empty())
        .collect();
    ensure!(!ancestors.is_empty(), "Storage root must not be empty.");
    for path in ancestors.into_iter().rev() {
        if !plain_directory(path)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn check_cleanup_roots(root: &Path) -> Result<bool> {
    if !check_root(root)? {
        return Ok(false);
    }
    let downloads = plain_directory(&root.join("downloads"))?;
    plain_directory(&root.join("locks"))?;
    Ok(downloads)
}

fn directory_bytes(path: &Path) -> Result<u64> {
    let Some(metadata) = optional_metadata(path)? else {
        return Ok(0);
    };
    if is_link(&metadata) {
        return Ok(0);
    }
    ensure!(
        metadata.is_dir(),
        "Expected a directory: {}",
        path.display()
    );
    let mut directories = vec![path.to_path_buf()];
    let mut total = 0u64;
    while let Some(directory) = directories.pop() {
        for entry in fs::read_dir(&directory)
            .with_context(|| format!("Could not list {}", directory.display()))?
        {
            let entry = entry.with_context(|| format!("Could not list {}", directory.display()))?;
            let metadata = inspect(&entry.path())?;
            if is_link(&metadata) {
                continue;
            }
            if metadata.is_dir() {
                directories.push(entry.path());
            } else if metadata.is_file() {
                total = total
                    .checked_add(metadata.len())
                    .context("Storage usage exceeds u64.")?;
            }
        }
    }
    Ok(total)
}
