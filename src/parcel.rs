use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::option::Option::Some;
use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use uuid::Uuid;

pub const MIB: u64 = 1024 * 1024;
pub const DEFAULT_CHUNK_SIZE: u64 = 20_000_000;
pub const MAX_CHUNK_SIZE: u64 = 20_000_000;
pub const MAX_MANIFEST_SIZE: u64 = 8 * MIB;
const MAX_PARTS: usize = 10_000;

#[derive(Clone, Default)]
pub struct Cancel(pub Arc<AtomicBool>);

impl Cancel {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }
    pub fn check(&self) -> Result<()> {
        ensure!(
            !self.0.load(Ordering::Relaxed),
            "Transfer paused. Start it again to resume."
        );
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct Progress {
    pub stage: String,
    pub done: u64,
    pub total: u64,
    pub bytes_per_second: Option<f64>,
}

impl Progress {
    pub fn new(stage: impl Into<String>, done: u64, total: u64) -> Self {
        Self {
            stage: stage.into(),
            done,
            total,
            bytes_per_second: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemotePart {
    pub message_id: String,
    pub attachment_id: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Part {
    pub index: usize,
    pub size: u64,
    pub sha256: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote: Option<RemotePart>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub format: String,
    pub version: u32,
    pub id: Uuid,
    pub filename: String,
    pub size: u64,
    pub chunk_size: u64,
    pub sha256: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encryption: Option<crate::crypto::Encryption>,
    pub parts: Vec<Part>,
}

pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub fn snowflake(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 20
        && id.bytes().all(|b| b.is_ascii_digit())
        && id.parse::<u64>().is_ok_and(|n| n > 0)
}

fn hash_valid(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

impl Manifest {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.format == "discord-parcel"
                && ((self.version == 1 && self.encryption.is_none())
                    || (self.version == 2 && self.encryption.is_some())),
            "Unsupported parcel format or version."
        );
        ensure!(
            !self.filename.is_empty()
                && self.filename != "."
                && self.filename != ".."
                && !self.filename.contains(['/', '\\', ':'])
                && !self.filename.chars().any(char::is_control),
            "The parcel contains an unsafe filename."
        );
        ensure!(self.filename.len() <= 255, "The filename is too long.");
        ensure!(
            (1..=MAX_CHUNK_SIZE).contains(&self.chunk_size),
            "Invalid parcel chunk size."
        );
        if let Some(encryption) = &self.encryption {
            encryption.validate()?;
            ensure!(
                self.chunk_size + crate::crypto::OVERHEAD <= MAX_CHUNK_SIZE,
                "Encrypted part exceeds 20 MB."
            );
        }
        ensure!(hash_valid(&self.sha256), "Invalid file checksum.");
        let count = self.size.div_ceil(self.chunk_size).max(1);
        ensure!(
            count <= MAX_PARTS as u64 && self.parts.len() as u64 == count,
            "Invalid parcel part count."
        );
        if let Some(channel) = &self.channel_id {
            ensure!(snowflake(channel), "Invalid channel ID.");
        }
        for (index, part) in self.parts.iter().enumerate() {
            ensure!(
                part.index == index && hash_valid(&part.sha256),
                "Invalid part order or checksum."
            );
            let expected = if index + 1 == self.parts.len() {
                self.size - index as u64 * self.chunk_size
            } else {
                self.chunk_size
            };
            ensure!(part.size == expected, "Invalid part size.");
            if let Some(remote) = &part.remote {
                ensure!(
                    self.channel_id.is_some(),
                    "Remote parcel has no channel ID."
                );
                ensure!(
                    snowflake(&remote.message_id) && snowflake(&remote.attachment_id),
                    "Invalid attachment IDs."
                );
                validate_attachment_url(&remote.url)?;
            }
        }
        Ok(())
    }

    pub fn read(path: &Path) -> Result<Self> {
        let bytes = read_bounded(path, MAX_MANIFEST_SIZE)?;
        Self::from_bytes(&bytes)
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        ensure!(
            bytes.len() as u64 <= MAX_MANIFEST_SIZE,
            "Parcel manifest is too large."
        );
        let manifest: Self =
            serde_json::from_slice(bytes).context("This is not a valid .parcel.json file.")?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn write(&self, path: &Path) -> Result<()> {
        self.validate()?;
        atomic_write(path, &serde_json::to_vec_pretty(self)?)
    }

    pub fn cache_key(&self) -> Result<String> {
        let mut identity = self.clone();
        identity.channel_id = None;
        for part in &mut identity.parts {
            part.remote = None;
        }
        Ok(digest(&serde_json::to_vec(&identity)?))
    }
}

pub fn validate_attachment_url(raw: &str) -> Result<()> {
    let url = reqwest::Url::parse(raw).context("Invalid attachment URL.")?;
    ensure!(
        url.scheme() == "https"
            && matches!(
                url.host_str(),
                Some("cdn.discordapp.com" | "media.discordapp.net")
            )
            && url.port().is_none()
            && url.username().is_empty()
            && url.password().is_none()
            && url.path().starts_with("/attachments/"),
        "Parcel attachment must use Discord's HTTPS CDN."
    );
    Ok(())
}

pub fn part_path(dir: &Path, index: usize) -> PathBuf {
    dir.join(format!("{:06}.part", index + 1))
}

pub fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let file = File::open(path).with_context(|| format!("Could not open {}", path.display()))?;
    ensure!(file.metadata()?.is_file(), "Expected a regular file.");
    ensure!(
        file.metadata()?.len() <= limit,
        "File exceeds the expected size."
    );
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= limit,
        "File exceeds the expected size."
    );
    Ok(bytes)
}

pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("Missing parent directory.")?;
    fs::create_dir_all(parent)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    temp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

pub fn lock_transfer(path: &Path) -> Result<File> {
    fs::create_dir_all(path.parent().context("Missing lock directory.")?)?;
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    file.try_lock()
        .context("This transfer is already running in another window. Wait for it to finish.")?;
    Ok(file)
}

pub fn inspect(
    source: &Path,
    chunk_size: u64,
    cancel: &Cancel,
    progress: &dyn Fn(Progress),
) -> Result<Manifest> {
    ensure!(
        (1..=MAX_CHUNK_SIZE).contains(&chunk_size),
        "Choose a chunk size between 1 byte and 20 MB."
    );
    let mut file = File::open(source).context("Could not open the source file.")?;
    let metadata = file.metadata()?;
    ensure!(metadata.is_file(), "Choose a regular file.");
    let size = metadata.len();
    ensure!(
        size.div_ceil(chunk_size).max(1) <= MAX_PARTS as u64,
        "This file needs too many parts. Increase the chunk size."
    );
    let filename = source
        .file_name()
        .and_then(|n| n.to_str())
        .context("The filename must be valid UTF-8.")?
        .to_owned();
    let mut full_hash = Sha256::new();
    let mut parts = Vec::new();
    let mut done = 0;
    loop {
        cancel.check()?;
        let mut bytes = Vec::with_capacity(chunk_size as usize);
        (&mut file).take(chunk_size).read_to_end(&mut bytes)?;
        if bytes.is_empty() && !parts.is_empty() {
            break;
        }
        full_hash.update(&bytes);
        done += bytes.len() as u64;
        ensure!(
            done <= size && parts.len() < MAX_PARTS,
            "The source file grew while reading. Try again."
        );
        parts.push(Part {
            index: parts.len(),
            size: bytes.len() as u64,
            sha256: digest(&bytes),
            remote: None,
        });
        progress(Progress::new("Reading and verifying source", done, size));
        if bytes.is_empty() {
            break;
        }
    }
    ensure!(
        done == size && file.metadata()?.modified()? == metadata.modified()?,
        "The source file changed while reading. Try again."
    );
    let manifest = Manifest {
        format: "discord-parcel".into(),
        version: 1,
        id: Uuid::new_v4(),
        filename,
        size,
        chunk_size,
        sha256: format!("{:x}", full_hash.finalize()),
        channel_id: None,
        encryption: None,
        parts,
    };
    manifest.validate()?;
    Ok(manifest)
}

pub fn verify_part(bytes: &[u8], part: &Part) -> Result<()> {
    ensure!(
        bytes.len() as u64 == part.size && digest(bytes) == part.sha256,
        "Part {} failed checksum verification. Retry the transfer.",
        part.index + 1
    );
    Ok(())
}

pub fn cached_part(dir: &Path, part: &Part) -> bool {
    read_bounded(&part_path(dir, part.index), part.size)
        .and_then(|bytes| verify_part(&bytes, part))
        .is_ok()
}

pub fn read_cached_part(
    manifest: &Manifest,
    dir: &Path,
    part: &Part,
    key: Option<&crate::crypto::Key>,
) -> Result<Vec<u8>> {
    let limit = part.size
        + if key.is_some() {
            crate::crypto::OVERHEAD
        } else {
            0
        };
    let bytes = read_bounded(&part_path(dir, part.index), limit)?;
    let bytes = if let Some(key) = key {
        key.open(&bytes, &crate::crypto::context(manifest, part.index))?
    } else {
        bytes
    };
    verify_part(&bytes, part)?;
    Ok(bytes)
}

pub fn assemble(
    manifest: &Manifest,
    key: Option<&crate::crypto::Key>,
    parts_dir: &Path,
    destination: &Path,
    cancel: &Cancel,
    progress: &dyn Fn(Progress),
) -> Result<PathBuf> {
    manifest.validate()?;
    ensure!(
        destination.is_dir(),
        "Choose an existing destination folder."
    );
    ensure!(
        manifest.encryption.is_some() == key.is_some(),
        "Encrypted parcels require a decryption key."
    );
    let output = destination.join(output_filename(&manifest.filename));
    ensure!(
        !output.exists(),
        "{} already exists. Choose another folder or rename the existing file.",
        output.file_name().unwrap_or_default().to_string_lossy()
    );
    let mut temp = tempfile::NamedTempFile::new_in(destination)?;
    let mut hash = Sha256::new();
    let mut done = 0;
    for part in &manifest.parts {
        cancel.check()?;
        let bytes = read_cached_part(manifest, parts_dir, part, key)
            .with_context(|| format!("Part {} is missing or unreadable.", part.index + 1))?;
        verify_part(&bytes, part)?;
        hash.update(&bytes);
        temp.write_all(&bytes)?;
        done += part.size;
        progress(Progress::new(
            "Reassembling and verifying",
            done,
            manifest.size,
        ));
    }
    if format!("{:x}", hash.finalize()) != manifest.sha256 {
        bail!("The complete file failed checksum verification.");
    }
    cancel.check()?;
    temp.as_file().sync_all()?;
    temp.persist_noclobber(&output)
        .map_err(|e| e.error)
        .context("Could not save the file without overwriting an existing file.")?;
    Ok(output)
}

struct DataLocation {
    path: PathBuf,
    portable: bool,
}

fn data_location() -> &'static DataLocation {
    static LOCATION: std::sync::OnceLock<DataLocation> = std::sync::OnceLock::new();

    LOCATION.get_or_init(|| {
        if let Ok(executable) = std::env::current_exe()
            && let Some(folder) = executable.parent()
        {
            let has_settings = folder.join("settings.json").is_file();
            let has_folders = ["downloads", "locks", "sent", "uploads"]
                .iter()
                .all(|name| folder.join(name).is_dir());

            if has_settings || has_folders {
                return DataLocation {
                    path: folder.to_path_buf(),
                    portable: true,
                };
            }
        }

        DataLocation {
            path: dirs::data_local_dir()
                .unwrap_or_else(std::env::temp_dir)
                .join("discord-parcel"),
            portable: false,
        }
    })
}

pub fn data_dir() -> PathBuf {
    data_location().path.clone()
}

pub fn is_portable() -> bool {
    data_location().portable
}

pub fn initialize_data_dir() -> Result<()> {
    let location = data_location();

    if location.portable {
        let _probe = tempfile::NamedTempFile::new_in(&location.path).with_context(|| {
            format!(
                "Cannot write portable data to {}. \
                     Move the application to a writable folder.",
                location.path.display()
            )
        })?;
    }

    crate::low_disk::migrate_checkpoints(&location.path)
}

pub fn output_filename(filename: &str) -> String {
    #[cfg(not(windows))]
    {
        filename.to_owned()
    }
    #[cfg(windows)]
    {
        let mut name: String = filename
            .chars()
            .map(|c| {
                if c.is_control() || "<>:\"/\\|?*".contains(c) {
                    '_'
                } else {
                    c
                }
            })
            .collect();
        name = name.trim_end_matches([' ', '.']).to_owned();
        if name.is_empty() {
            name = "file".into();
        }
        let stem = name
            .split('.')
            .next()
            .unwrap_or("")
            .trim_end()
            .to_uppercase();
        let numbered_device = ["COM", "LPT"].iter().any(|prefix| {
            stem.strip_prefix(prefix).is_some_and(|suffix| {
                matches!(
                    suffix,
                    "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                )
            })
        });
        if numbered_device
            || matches!(
                stem.as_str(),
                "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
            )
        {
            name.insert(0, '_');
        }
        name
    }
}

pub fn human_size(size: u64) -> String {
    if size >= 1024 * MIB {
        format!("{:.1} GiB", size as f64 / (1024 * MIB) as f64)
    } else if size >= MIB {
        format!("{:.1} MiB", size as f64 / MIB as f64)
    } else if size >= 1024 {
        format!("{:.1} KiB", size as f64 / 1024.0)
    } else {
        format!("{size} bytes")
    }
}
