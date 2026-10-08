use crate::{
    crypto::Key,
    parcel::{self, Cancel, Manifest, Progress},
};
use anyhow::{Context, Result, ensure};
use std::path::Path;

pub(crate) fn check_download(
    manifest: &Manifest,
    key: Option<&Key>,
    cache: &Path,
    destination: &Path,
    cancel: &Cancel,
    progress: &dyn Fn(Progress),
) -> Result<Vec<bool>> {
    let mut cached = Vec::with_capacity(manifest.parts.len());
    let mut checked = 0;
    progress(Progress::new(
        "Checking cached parts and disk space",
        0,
        manifest.size,
    ));
    for part in &manifest.parts {
        cancel.check()?;
        cached.push(parcel::read_cached_part(manifest, cache, part, key).is_ok());
        checked += part.size;
        progress(Progress::new(
            "Checking cached parts and disk space",
            checked,
            manifest.size,
        ));
    }
    cancel.check()?;
    let cache_space = query(cache)?;
    let output_space = query(destination)?;
    let mut cache_required = 0u64;
    for (part, &present) in manifest.parts.iter().zip(&cached) {
        if !present {
            let wire_size = part
                .size
                .checked_add(if key.is_some() {
                    crate::crypto::OVERHEAD
                } else {
                    0
                })
                .context("Cached part size exceeds u64.")?;
            cache_required = cache_required
                .checked_add(allocated_bytes(wire_size, cache_space.allocation_unit)?)
                .context("Required cache space exceeds u64.")?;
        }
    }
    let output_required = allocated_bytes(manifest.size, output_space.allocation_unit)?;
    let separate = matches!(
        (&cache_space.volume, &output_space.volume),
        (Some(cache_volume), Some(output_volume)) if cache_volume != output_volume
    );
    if separate {
        require_space(
            cache,
            cache_required,
            cache_space.available,
            "download cache",
        )?;
        require_space(
            destination,
            output_required,
            output_space.available,
            "restored file",
        )?;
    } else {
        let required = cache_required
            .checked_add(output_required)
            .context("Required download space exceeds u64.")?;
        require_space(
            destination,
            required,
            cache_space.available.min(output_space.available),
            &format!("download cache ({}) and restored file", cache.display()),
        )?;
    }
    cancel.check()?;
    Ok(cached)
}

pub(crate) fn check_output(destination: &Path, size: u64) -> Result<()> {
    let space = query(destination)?;
    require_space(
        destination,
        allocated_bytes(size, space.allocation_unit)?,
        space.available,
        "restored file",
    )
}

pub(crate) fn check_low_disk(
    destination: &Path,
    checkpoints: &Path,
    partial: &std::fs::File,
    size: u64,
    verified: u64,
    checkpoint_size: u64,
) -> Result<()> {
    let output = query(destination)?;
    let metadata = query(checkpoints)?;
    let credited =
        partial_allocation(partial)?.min(allocated_bytes(verified, output.allocation_unit)?);
    let remaining = allocated_bytes(size, output.allocation_unit)?.saturating_sub(credited);
    let checkpoint_required = allocated_bytes(checkpoint_size, metadata.allocation_unit)?
        .checked_mul(2)
        .and_then(|bytes| bytes.checked_add(metadata.allocation_unit))
        .context("Required checkpoint space exceeds u64.")?;
    let output_required = remaining
        .checked_add(output.allocation_unit)
        .context("Required output space exceeds u64.")?;
    if matches!((&output.volume, &metadata.volume), (Some(a), Some(b)) if a != b) {
        require_space(
            destination,
            output_required,
            output.available,
            "remaining Low-disk output and publication metadata",
        )?;
        require_space(
            checkpoints,
            checkpoint_required,
            metadata.available,
            "Low-disk checkpoints",
        )?;
    } else {
        require_space(
            destination,
            output_required
                .checked_add(checkpoint_required)
                .context("Required Low-disk space exceeds u64.")?,
            output.available.min(metadata.available),
            "remaining Low-disk output and checkpoints (verified allocated partial data already counted)",
        )?;
    }
    Ok(())
}

#[cfg(unix)]
fn partial_allocation(file: &std::fs::File) -> Result<u64> {
    use std::os::unix::fs::MetadataExt;
    file.metadata()?
        .blocks()
        .checked_mul(512)
        .context("Partial allocation exceeds u64.")
}

#[cfg(windows)]
fn partial_allocation(file: &std::fs::File) -> Result<u64> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_STANDARD_INFO, FileStandardInfo, GetFileInformationByHandleEx,
    };
    let mut info = std::mem::MaybeUninit::<FILE_STANDARD_INFO>::uninit();
    ensure!(
        unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle(),
                FileStandardInfo,
                info.as_mut_ptr().cast(),
                std::mem::size_of::<FILE_STANDARD_INFO>() as u32,
            )
        } != 0,
        "Could not query partial output allocation: {}",
        std::io::Error::last_os_error()
    );
    u64::try_from(unsafe { info.assume_init() }.AllocationSize)
        .context("Invalid partial allocation.")
}

#[cfg(not(any(unix, windows)))]
fn partial_allocation(_file: &std::fs::File) -> Result<u64> {
    Ok(0)
}

fn allocated_bytes(size: u64, allocation_unit: u64) -> Result<u64> {
    size.div_ceil(allocation_unit)
        .checked_mul(allocation_unit)
        .context("Required disk space exceeds u64.")
}

fn require_space(path: &Path, required: u64, available: u64, purpose: &str) -> Result<()> {
    ensure!(
        available >= required,
        "Not enough disk space for {purpose} at {}. Need {} of additional free space, {} available.",
        path.display(),
        parcel::human_size(required),
        parcel::human_size(available),
    );
    Ok(())
}

pub(crate) struct DiskSpace {
    pub available: u64,
    pub allocation_unit: u64,
    pub volume: Option<String>,
}

#[cfg(unix)]
#[allow(clippy::useless_conversion)]
pub(crate) fn query(path: &std::path::Path) -> anyhow::Result<DiskSpace> {
    use anyhow::{Context, ensure};
    use std::ffi::CString;
    use std::os::unix::{ffi::OsStrExt, fs::MetadataExt};

    let resolved = path
        .canonicalize()
        .with_context(|| format!("Failed to resolve disk query path {}", path.display()))?;
    let metadata = resolved
        .metadata()
        .with_context(|| format!("Failed to identify filesystem for {}", path.display()))?;
    let encoded = CString::new(resolved.as_os_str().as_bytes())
        .with_context(|| format!("Disk query path contains a NUL byte: {}", path.display()))?;
    let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    if unsafe { libc::statvfs(encoded.as_ptr(), stats.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("statvfs failed for {}", path.display()));
    }
    let stats = unsafe { stats.assume_init() };
    let allocation_unit = u64::try_from(if stats.f_frsize != 0 {
        stats.f_frsize
    } else {
        stats.f_bsize
    })
    .with_context(|| {
        format!(
            "Filesystem allocation unit exceeds u64 for {}",
            path.display()
        )
    })?;
    ensure!(
        allocation_unit != 0,
        "Filesystem reported a zero allocation unit for {}",
        path.display()
    );
    let available = u64::try_from(stats.f_bavail)
        .ok()
        .and_then(|blocks| blocks.checked_mul(allocation_unit))
        .with_context(|| format!("Available disk space exceeds u64 for {}", path.display()))?;

    Ok(DiskSpace {
        available,
        allocation_unit,
        volume: Some(format!("unix:{}", metadata.dev())),
    })
}

#[cfg(windows)]
pub(crate) fn query(path: &std::path::Path) -> anyhow::Result<DiskSpace> {
    use anyhow::{Context, ensure};
    use std::os::windows::ffi::OsStrExt;
    use std::path::{Component, Prefix};
    use windows_sys::Win32::Foundation::{ERROR_INVALID_FUNCTION, ERROR_NOT_SUPPORTED};
    use windows_sys::Win32::Storage::FileSystem::{
        GetDiskFreeSpaceExW, GetDiskFreeSpaceW, GetVolumeNameForVolumeMountPointW,
        GetVolumePathNameW,
    };

    let resolved = path
        .canonicalize()
        .with_context(|| format!("Failed to resolve disk query path {}", path.display()))?;
    let mut encoded: Vec<u16> = resolved.as_os_str().encode_wide().collect();
    ensure!(
        !encoded.contains(&0),
        "Disk query path contains a NUL code unit: {}",
        path.display()
    );
    encoded.push(0);
    let mut root = vec![0u16; 32_768];
    if unsafe { GetVolumePathNameW(encoded.as_ptr(), root.as_mut_ptr(), root.len() as u32) } == 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("GetVolumePathNameW failed for {}", path.display()));
    }
    let root_len = root.iter().position(|&unit| unit == 0).with_context(|| {
        format!(
            "Volume root exceeds Windows path limit for {}",
            path.display()
        )
    })?;
    ensure!(root_len != 0, "Empty volume root for {}", path.display());
    root.truncate(root_len + 1);

    let mut available = 0u64;
    if unsafe {
        GetDiskFreeSpaceExW(
            root.as_ptr(),
            &mut available,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("GetDiskFreeSpaceExW failed for {}", path.display()));
    }
    let mut sectors_per_cluster = 0u32;
    let mut bytes_per_sector = 0u32;
    let mut free_clusters = 0u32;
    let mut total_clusters = 0u32;
    if unsafe {
        GetDiskFreeSpaceW(
            root.as_ptr(),
            &mut sectors_per_cluster,
            &mut bytes_per_sector,
            &mut free_clusters,
            &mut total_clusters,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("GetDiskFreeSpaceW failed for {}", path.display()));
    }
    let allocation_unit = u64::from(sectors_per_cluster)
        .checked_mul(u64::from(bytes_per_sector))
        .filter(|&size| size != 0)
        .with_context(|| format!("Invalid filesystem allocation unit for {}", path.display()))?;

    let is_unc = matches!(
        resolved.components().next(),
        Some(Component::Prefix(prefix))
            if matches!(prefix.kind(), Prefix::UNC(_, _) | Prefix::VerbatimUNC(_, _))
    );
    let volume = if is_unc {
        None
    } else {
        let mut name = [0u16; 50];
        if unsafe {
            GetVolumeNameForVolumeMountPointW(root.as_ptr(), name.as_mut_ptr(), name.len() as u32)
        } == 0
        {
            let error = std::io::Error::last_os_error();
            match error.raw_os_error().map(|code| code as u32) {
                Some(ERROR_INVALID_FUNCTION | ERROR_NOT_SUPPORTED) => None,
                _ => {
                    return Err(error).with_context(|| {
                        format!(
                            "GetVolumeNameForVolumeMountPointW failed for {}",
                            path.display()
                        )
                    });
                }
            }
        } else {
            let len = name.iter().position(|&unit| unit == 0).with_context(|| {
                format!(
                    "Volume GUID exceeds Windows buffer limit for {}",
                    path.display()
                )
            })?;
            ensure!(len != 0, "Empty volume GUID for {}", path.display());
            Some(
                String::from_utf16(&name[..len])
                    .with_context(|| format!("Invalid volume GUID for {}", path.display()))?
                    .to_ascii_lowercase(),
            )
        }
    };

    Ok(DiskSpace {
        available,
        allocation_unit,
        volume,
    })
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn query(path: &std::path::Path) -> anyhow::Result<DiskSpace> {
    anyhow::bail!(
        "Disk space queries are not supported on this platform: {}",
        path.display()
    )
}
