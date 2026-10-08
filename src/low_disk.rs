use crate::parcel::{self, Cancel, Manifest, Part, Progress};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

#[derive(Serialize, Deserialize, PartialEq, Eq)]
struct Checkpoint {
    version: u32,
    manifest: String,
    destination: PathBuf,
    partial: uuid::Uuid,
    file_identity: String,
    completed: usize,
}

pub(crate) fn migrate_checkpoints(root: &Path) -> Result<()> {
    let legacy = root.join("receiving");
    if !crate::storage::check_root(&legacy)? {
        return Ok(());
    }
    let downloads = root.join("downloads");
    fs::create_dir_all(&downloads)?;
    ensure!(
        crate::storage::check_root(&downloads)?,
        "Download directory disappeared."
    );
    sync_directory(root)?;
    for entry in fs::read_dir(&legacy)? {
        let path = entry?.path();
        let Some(key) = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".json"))
        else {
            continue;
        };
        let Ok(state) = read_cleanup_checkpoint(&path) else {
            continue;
        };
        if state.version != 1
            || !state.destination.is_absolute()
            || parcel::digest(&serde_json::to_vec(&(&state.manifest, &state.destination))?) != key
        {
            continue;
        }
        let Some(_lock) = crate::storage::try_download_lock(root, &state.manifest)? else {
            continue;
        };
        ensure!(
            crate::storage::check_root(&legacy)? && crate::storage::check_root(&downloads)?,
            "Download directories changed during migration."
        );
        if read_cleanup_checkpoint(&path)? != state {
            continue;
        }
        let destination = downloads.join(format!("{key}.json"));
        if destination.try_exists()? {
            ensure!(
                read_cleanup_checkpoint(&destination)? == state,
                "Conflicting Low-disk checkpoints. Both were kept."
            );
        } else {
            publish_noclobber(&path, &destination)?;
        }
        sync_directory(&downloads)?;
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        sync_directory(&legacy)?;
    }
    if fs::remove_dir(&legacy).is_ok() {
        sync_directory(root)?;
    }
    Ok(())
}

pub(crate) fn clear_checkpoint(root: &Path, key: &str, removed_bytes: &mut u64) -> Result<bool> {
    let directory = root.join("downloads");
    let path = directory.join(format!("{key}.json"));
    let state = read_cleanup_checkpoint(&path)?;
    if state.version != 1
        || parcel::digest(&serde_json::to_vec(&(&state.manifest, &state.destination))?) != key
        || !state.destination.is_absolute()
    {
        return Ok(false);
    }
    let Some(_lock) = crate::storage::try_download_lock(root, &state.manifest)? else {
        return Ok(false);
    };
    ensure!(
        crate::storage::check_root(&directory)?,
        "Checkpoint directory disappeared."
    );
    if read_cleanup_checkpoint(&path)? != state {
        return Ok(false);
    }
    if !crate::storage::check_root(&state.destination)?
        || state.destination.canonicalize()? != state.destination
    {
        return Ok(false);
    }
    let partial = partial_path(&state.destination, state.partial);
    let file = match fs::symlink_metadata(&partial) {
        Ok(_) => Some(open_partial(&partial)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    if let Some(file) = file {
        if file_identity(&file, 1)? != state.file_identity {
            return Ok(false);
        }
        let size = file.metadata()?.len();
        let total = removed_bytes
            .checked_add(size)
            .context("Removed byte count exceeds u64.")?;
        ensure!(
            crate::storage::check_root(&state.destination)?,
            "Destination disappeared."
        );
        let current = open_partial(&partial)?;
        if file_identity(&current, 1)? != state.file_identity {
            return Ok(false);
        }
        drop(current);
        drop(file);
        fs::remove_file(&partial)?;
        *removed_bytes = total;
        sync_directory(&state.destination)?;
    }
    ensure!(
        crate::storage::check_root(&directory)?,
        "Checkpoint directory disappeared."
    );
    if read_cleanup_checkpoint(&path)? != state {
        return Ok(false);
    }
    let size = fs::symlink_metadata(&path)?.len();
    let total = removed_bytes
        .checked_add(size)
        .context("Removed byte count exceeds u64.")?;
    fs::remove_file(&path)?;
    *removed_bytes = total;
    sync_directory(&directory)?;
    Ok(true)
}

fn read_cleanup_checkpoint(path: &Path) -> Result<Checkpoint> {
    let file = open_partial(path)?;
    ensure!(
        file.metadata()?.len() <= parcel::MAX_MANIFEST_SIZE,
        "Checkpoint is too large."
    );
    let mut bytes = Vec::new();
    file.take(parcel::MAX_MANIFEST_SIZE + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= parcel::MAX_MANIFEST_SIZE,
        "Checkpoint is too large."
    );
    Ok(serde_json::from_slice(&bytes)?)
}

pub(crate) struct PartialOutput {
    file: File,
    path: PathBuf,
    checkpoint_path: PathBuf,
    state: Checkpoint,
    done: u64,
    published: bool,
}

impl PartialOutput {
    pub(crate) fn open(
        manifest: &Manifest,
        destination: &Path,
        storage: &Path,
        cancel: &Cancel,
        progress: &dyn Fn(Progress),
    ) -> Result<Self> {
        let destination = destination.canonicalize()?;
        let identity = manifest.cache_key()?;
        let key = parcel::digest(&serde_json::to_vec(&(&identity, &destination))?);
        let directory = storage.join("downloads");
        fs::create_dir_all(&directory)?;
        sync_directory(storage)?;
        let checkpoint_path = directory.join(format!("{key}.json"));
        let final_path = destination.join(parcel::output_filename(&manifest.filename));
        let published = fs::symlink_metadata(&final_path).is_ok();
        ensure!(
            !published || checkpoint_path.try_exists()?,
            "{} already exists in the destination folder.",
            final_path.display()
        );
        let mut output = if checkpoint_path.try_exists()? {
            let state: Checkpoint = serde_json::from_slice(&parcel::read_bounded(
                &checkpoint_path,
                parcel::MAX_MANIFEST_SIZE,
            )?)
            .context(
                "The Low-disk checkpoint is unreadable. Its partial output has been preserved.",
            )?;
            ensure!(
                state.version == 1
                    && state.manifest == identity
                    && state.destination == destination
                    && state.completed <= manifest.parts.len(),
                "Low-disk checkpoint does not match this parcel and destination."
            );
            let path = partial_path(&destination, state.partial);
            ensure!(
                !published || state.completed == manifest.parts.len(),
                "An existing destination file blocks this incomplete Low-disk transfer."
            );
            let file = open_partial(if published { &final_path } else { &path })?;
            ensure!(
                file_identity(&file, 1 + u64::from(published))? == state.file_identity,
                "The Low-disk partial file was replaced. Refusing to modify it."
            );
            Self {
                file,
                path,
                checkpoint_path,
                state,
                done: 0,
                published,
            }
        } else {
            cancel.check()?;
            let partial = uuid::Uuid::new_v4();
            let path = partial_path(&destination, partial);
            let mut options = OpenOptions::new();
            options.read(true).write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let file = options
                .open(&path)
                .context("Could not create a new Low-disk partial file.")?;
            file.sync_all()?;
            sync_directory(&destination)?;
            let state = Checkpoint {
                version: 1,
                manifest: identity,
                destination,
                partial,
                file_identity: file_identity(&file, 1)?,
                completed: 0,
            };
            let output = Self {
                file,
                path,
                checkpoint_path,
                state,
                done: 0,
                published,
            };
            output.check_space(manifest.size)?;
            output.save()?;
            output
        };
        if published {
            output.done = manifest.size;
            return Ok(output);
        }
        let mut completed = 0;
        for part in manifest.parts.iter().take(output.state.completed) {
            cancel.check()?;
            progress(Progress::new(
                "Verifying Low-disk partial output",
                output.done,
                manifest.size,
            ));
            let mut bytes = vec![0; part.size as usize];
            match output.file.read_exact(&mut bytes) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(error) => return Err(error).context("Could not read Low-disk partial output."),
            }
            if parcel::verify_part(&bytes, part).is_err() {
                break;
            }
            output.done += part.size;
            completed += 1;
        }
        cancel.check()?;
        output.file.set_len(output.done)?;
        output.file.seek(SeekFrom::Start(output.done))?;
        output.file.sync_all()?;
        output.state.completed = completed;
        output.check_space(manifest.size)?;
        output.save()?;
        Ok(output)
    }

    pub(crate) fn completed(&self) -> usize {
        self.state.completed
    }

    fn check_space(&self, size: u64) -> Result<()> {
        let checkpoint_size = serde_json::to_vec(&self.state)?.len() as u64 + 32;
        crate::disk_space::check_low_disk(
            &self.state.destination,
            self.checkpoint_path
                .parent()
                .context("Missing checkpoint directory.")?,
            &self.file,
            size,
            self.done,
            checkpoint_size,
        )
    }

    fn save(&self) -> Result<()> {
        let bytes = serde_json::to_vec(&self.state)?;
        #[cfg(not(windows))]
        parcel::atomic_write(&self.checkpoint_path, &bytes)?;
        #[cfg(windows)]
        {
            let mut temporary = tempfile::NamedTempFile::new_in(
                self.checkpoint_path
                    .parent()
                    .context("Missing checkpoint directory.")?,
            )?;
            temporary.write_all(&bytes)?;
            temporary.as_file().sync_all()?;
            move_durable(temporary.path(), &self.checkpoint_path, true)?;
        }
        sync_directory(
            self.checkpoint_path
                .parent()
                .context("Missing checkpoint directory.")?,
        )
    }

    pub(crate) fn append(&mut self, bytes: &[u8], part: &Part) -> Result<()> {
        ensure!(
            part.index == self.state.completed,
            "Unexpected Low-disk part order."
        );
        parcel::verify_part(bytes, part)?;
        ensure!(
            file_identity(&self.file, 1)? == self.state.file_identity,
            "Low-disk partial output identity changed."
        );
        self.file.write_all(bytes)?;
        self.file.sync_all()?;
        self.done += part.size;
        self.state.completed += 1;
        self.save()
    }

    pub(crate) fn publish(
        mut self,
        manifest: &Manifest,
        cancel: &Cancel,
        progress: &dyn Fn(Progress),
    ) -> Result<PathBuf> {
        ensure!(
            self.state.completed == manifest.parts.len(),
            "Low-disk output is incomplete."
        );
        self.file.seek(SeekFrom::Start(0))?;
        let mut hash = Sha256::new();
        let mut done = 0;
        for part in &manifest.parts {
            cancel.check()?;
            let mut bytes = vec![0; part.size as usize];
            self.file.read_exact(&mut bytes)?;
            parcel::verify_part(&bytes, part)?;
            hash.update(&bytes);
            done += part.size;
            progress(Progress::new(
                "Verifying complete Low-disk output",
                done,
                manifest.size,
            ));
        }
        ensure!(
            self.file.metadata()?.len() == manifest.size
                && format!("{:x}", hash.finalize()) == manifest.sha256,
            "The complete file failed checksum verification. Partial output was preserved."
        );
        cancel.check()?;
        self.file.sync_all()?;
        let output = self
            .state
            .destination
            .join(parcel::output_filename(&manifest.filename));
        let current = open_partial(if self.published { &output } else { &self.path })?;
        ensure!(
            file_identity(&current, 1 + u64::from(self.published))? == self.state.file_identity,
            "The Low-disk output was replaced before publication."
        );
        drop(current);
        drop(self.file);
        if !self.published {
            publish_noclobber(&self.path, &output)
                .context("Could not publish the verified Low-disk file without overwriting. Partial output and checkpoint were preserved.")?;
        }
        sync_directory(&self.state.destination)?;
        if fs::symlink_metadata(&self.path).is_ok() {
            let remaining = open_partial(&self.path)?;
            ensure!(
                file_identity(&remaining, 2)? == self.state.file_identity,
                "The partial path changed after publication; checkpoint was preserved."
            );
            drop(remaining);
            fs::remove_file(&self.path)?;
            sync_directory(&self.state.destination)?;
        }
        fs::remove_file(&self.checkpoint_path)?;
        sync_directory(
            self.checkpoint_path
                .parent()
                .context("Missing checkpoint directory.")?,
        )?;
        Ok(output)
    }
}

fn partial_path(destination: &Path, id: uuid::Uuid) -> PathBuf {
    destination.join(format!(".discord-parcel-{id}.partial"))
}

fn open_partial(path: &Path) -> Result<File> {
    let metadata = fs::symlink_metadata(path).context("Low-disk partial file is missing or inaccessible. Restore it to its original destination to resume.")?;
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "Low-disk partial output must be a regular file, not a link."
    );
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(path)?;
    ensure!(
        file.metadata()?.is_file(),
        "Low-disk partial output must be a regular file."
    );
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        ensure!(
            file.metadata()?.file_attributes() & 0x400 == 0,
            "Low-disk partial output cannot be a reparse point."
        );
    }
    Ok(file)
}

#[cfg(unix)]
fn file_identity(file: &File, max_links: u64) -> Result<String> {
    use std::os::unix::fs::MetadataExt;
    let metadata = file.metadata()?;
    ensure!(
        metadata.nlink() <= max_links,
        "Low-disk partial output has unexpected hard links."
    );
    Ok(format!("{}:{}", metadata.dev(), metadata.ino()))
}

#[cfg(windows)]
fn file_identity(file: &File, max_links: u64) -> Result<String> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };
    let mut info = std::mem::MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
    ensure!(
        unsafe { GetFileInformationByHandle(file.as_raw_handle(), info.as_mut_ptr()) } != 0,
        "Could not identify Low-disk partial file: {}",
        std::io::Error::last_os_error()
    );
    let info = unsafe { info.assume_init() };
    ensure!(
        u64::from(info.nNumberOfLinks) <= max_links,
        "Low-disk partial output has unexpected hard links."
    );
    Ok(format!(
        "{}:{}:{}",
        info.dwVolumeSerialNumber, info.nFileIndexHigh, info.nFileIndexLow
    ))
}

#[cfg(not(any(unix, windows)))]
fn file_identity(_file: &File, _max_links: u64) -> Result<String> {
    anyhow::bail!("Low-disk receiving is unsupported on this platform.")
}

fn sync_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

#[cfg(not(windows))]
fn publish_noclobber(source: &Path, destination: &Path) -> Result<()> {
    let mut temporary = tempfile::TempPath::try_from_path(source)?;
    temporary.disable_cleanup(true);
    temporary
        .persist_noclobber(destination)
        .map_err(|error| error.error)?;
    Ok(())
}

#[cfg(windows)]
fn publish_noclobber(source: &Path, destination: &Path) -> Result<()> {
    move_durable(source, destination, false)
}

#[cfg(windows)]
fn move_durable(source: &Path, destination: &Path, replace: bool) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let flags = MOVEFILE_WRITE_THROUGH
        | if replace {
            MOVEFILE_REPLACE_EXISTING
        } else {
            0
        };
    if unsafe { MoveFileExW(source.as_ptr(), destination.as_ptr(), flags) } == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}
