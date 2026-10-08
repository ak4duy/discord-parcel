use crate::parcel::{self, Cancel, MAX_MANIFEST_SIZE, Manifest, Progress, RemotePart};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serenity::{
    Error as SerenityError,
    builder::{CreateAllowedMentions, CreateAttachment, CreateEmbed, CreateMessage},
    http::{Http, StatusCode},
    model::{
        channel::{Attachment, Message, Nonce},
        id::{ChannelId, MessageId, UserId},
    },
};
use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

#[derive(Clone)]
pub struct Connection {
    pub token: String,
    pub channel_id: String,
}

#[derive(Default)]
pub struct SendOptions {
    pub recipient: Option<u64>,
    pub password: Option<String>,
}

#[derive(Debug)]
pub struct UploadResult {
    pub manifest_path: PathBuf,
    pub message_link: String,
    pub parts: usize,
}

#[derive(Serialize, Deserialize)]
struct UploadState {
    manifest: Manifest,
    manifest_message_id: Option<String>,
    #[serde(default = "uuid::Uuid::new_v4")]
    notification_id: uuid::Uuid,
}

pub fn upload(
    source: &Path,
    options: &SendOptions,
    chunk_size: u64,
    connection: &Connection,
    storage: &Path,
    cancel: &Cancel,
    progress: &dyn Fn(Progress),
) -> Result<UploadResult> {
    runtime()?.block_on(upload_async(
        source, options, chunk_size, connection, storage, cancel, progress,
    ))
}

async fn upload_async(
    source: &Path,
    options: &SendOptions,
    chunk_size: u64,
    connection: &Connection,
    storage: &Path,
    cancel: &Cancel,
    progress: &dyn Fn(Progress),
) -> Result<UploadResult> {
    ensure!(
        !connection.token.trim().is_empty(),
        "Enter a Discord bot token."
    );

    let http = Http::new(connection.token.trim());
    let channel_id = parse_channel_id(connection.channel_id.trim())?;

    progress(Progress::new("Connecting to Discord", 0, 0));
    cancel.check()?;

    let channel = channel_id
        .to_channel(&http)
        .await
        .context("Could not access the Discord channel.")?
        .guild()
        .context("Discord Parcel requires a server channel.")?;

    cancel.check()?;

    let channel_id_string = channel.id.to_string();

    ensure!(options.recipient != Some(0), "Invalid recipient ID.");
    let chunk_size = if options.password.is_some() {
        chunk_size
            .checked_sub(crate::crypto::OVERHEAD)
            .context("Part size is too small for encryption.")?
    } else {
        chunk_size
    };
    let mut manifest = parcel::inspect(source, chunk_size, cancel, progress)?;
    manifest.channel_id = Some(channel_id_string.clone());

    let key = parcel::digest(
        format!(
            "{}:{}:{}:{}:{}:{}:{}",
            manifest.sha256,
            manifest.size,
            manifest.chunk_size,
            manifest.filename,
            channel.id,
            parcel::digest(connection.token.as_bytes()),
            options.password.is_some()
        )
        .as_bytes(),
    );

    let state_path = storage.join("uploads").join(format!("{key}.json"));

    let _lock = parcel::lock_transfer(&storage.join("locks").join(format!("upload-{key}")))?;

    let mut state = UploadState {
        manifest,
        manifest_message_id: None,
        notification_id: uuid::Uuid::new_v4(),
    };

    if state_path.exists() {
        let saved: UploadState =
            serde_json::from_slice(&parcel::read_bounded(&state_path, MAX_MANIFEST_SIZE)?)
                .context("The saved upload checkpoint is unreadable.")?;

        saved.manifest.validate()?;

        ensure!(
            checkpoint_matches(&saved.manifest, &state.manifest, options.password.is_some()),
            "Upload checkpoint does not match this file."
        );

        state = saved;
    }

    let mut legacy_lock = None;
    if !state_path.exists()
        && let Some((saved, lock)) =
            legacy_checkpoint(storage, &state.manifest, options, &http, channel.id, cancel).await?
    {
        state = saved;
        legacy_lock = Some(lock);
    }
    let _legacy_lock = legacy_lock;

    if state.manifest.encryption.is_none() && options.password.is_some() {
        state.manifest.version = 2;
        state.manifest.encryption = Some(crate::crypto::Encryption::new(
            options.password.as_deref().unwrap(),
        )?);
    }
    let encryption_key = state
        .manifest
        .encryption
        .as_ref()
        .map(|metadata| metadata.unlock(options.password.as_deref().unwrap_or_default()))
        .transpose()?;

    save_state(&state_path, &state)?;

    let mut file = File::open(source)?;
    let mut done = 0;

    for index in 0..state.manifest.parts.len() {
        cancel.check()?;

        let part_size = state.manifest.parts[index].size;
        let wire_size = part_size
            + if encryption_key.is_some() {
                crate::crypto::OVERHEAD
            } else {
                0
            };
        let saved_remote = state.manifest.parts[index].remote.clone();

        progress(Progress::new(
            format!(
                "Uploading part {} of {}",
                index + 1,
                state.manifest.parts.len()
            ),
            done,
            state.manifest.size,
        ));

        let existing = if let Some(remote) = &saved_remote {
            let message_id = parse_message_id(&remote.message_id)?;

            message_if_exists(&http, channel.id, message_id)
                .await?
                .and_then(|message| {
                    message.attachments.into_iter().find(|attachment| {
                        attachment.id.to_string() == remote.attachment_id
                            && u64::from(attachment.size) == wire_size
                    })
                })
        } else {
            None
        };

        let remote = if let Some(attachment) = existing {
            let mut remote = saved_remote.expect("remote exists when attachment exists");

            remote.url = attachment.url;
            remote
        } else {
            state.manifest_message_id = None;
            state.notification_id = uuid::Uuid::new_v4();

            file.seek(SeekFrom::Start(index as u64 * chunk_size))?;

            let mut bytes = vec![0; part_size as usize];

            file.read_exact(&mut bytes)
                .context("The source file changed during upload.")?;

            parcel::verify_part(&bytes, &state.manifest.parts[index])
                .context("The source file changed during upload.")?;

            if let Some(key) = &encryption_key {
                bytes = key.seal(&bytes, &crate::crypto::context(&state.manifest, index))?;
            }

            let filename = format!("{}.{:06}.part", state.manifest.id, index + 1);

            let embed = CreateEmbed::new()
                .title("Discord Parcel")
                .description("File part")
                .field(
                    "Part",
                    format!("{} / {}", index + 1, state.manifest.parts.len()),
                    true,
                )
                .field("Size", parcel::human_size(part_size), true)
                .field("Transfer", state.manifest.id, false);

            let nonce = parcel::digest(format!("{}:{index}", state.manifest.id).as_bytes());

            let builder = CreateMessage::new()
                .embed(embed)
                .nonce(Nonce::String(nonce[..24].to_owned()))
                .enforce_nonce(true);

            let attachment = CreateAttachment::bytes(bytes, filename.clone());

            cancel.check()?;

            let message = channel
                .id
                .send_files(&http, [attachment], builder)
                .await
                .context("Could not upload a parcel part to Discord.")?;

            let attachment = message
                .attachments
                .into_iter()
                .find(|attachment| {
                    attachment.filename == filename && u64::from(attachment.size) == wire_size
                })
                .context("Discord did not return the uploaded attachment.")?;

            RemotePart {
                message_id: message.id.to_string(),
                attachment_id: attachment.id.to_string(),
                url: attachment.url,
            }
        };

        state.manifest.parts[index].remote = Some(remote);

        done += state.manifest.parts[index].size;

        save_state(&state_path, &state)?;
        cancel.check()?;

        progress(Progress::new(
            format!(
                "Uploaded {} of {} parts",
                index + 1,
                state.manifest.parts.len()
            ),
            done,
            state.manifest.size,
        ));
    }

    cancel.check()?;

    let bytes = serde_json::to_vec_pretty(&state.manifest)?;

    ensure!(
        bytes.len() as u64 <= MAX_MANIFEST_SIZE,
        "The transfer manifest is too large."
    );

    let manifest_path = storage
        .join("sent")
        .join(format!("{}.parcel.json", state.manifest.id));

    state.manifest.write(&manifest_path)?;

    progress(Progress::new(
        "Publishing transfer manifest",
        state.manifest.size,
        state.manifest.size,
    ));

    let content = options
        .recipient
        .map(|id| format!("<@{id}> Your parcel is ready!"))
        .unwrap_or_default();

    if let Some(message_id) = &state.manifest_message_id {
        let message_id = parse_message_id(message_id)?;
        let existing = message_if_exists(&http, channel.id, message_id).await?;

        if existing
            .as_ref()
            .is_none_or(|message| message.content != content)
        {
            state.manifest_message_id = None;
            state.notification_id = uuid::Uuid::new_v4();
        }
    }

    save_state(&state_path, &state)?;

    if state.manifest_message_id.is_none() {
        let embed = CreateEmbed::new()
            .title("Discord Parcel")
            .description("Transfer ready")
            .field("File", state.manifest.filename.clone(), false)
            .field("Size", parcel::human_size(state.manifest.size), true)
            .field("Parts", state.manifest.parts.len().to_string(), true)
            .field("Transfer", state.manifest.id, false)
            .field(
                "Restore",
                "Download the attached transfer file and open it in Discord Parcel.",
                false,
            );

        let nonce = parcel::digest(
            format!(
                "{}:manifest:{}:{}",
                state.manifest.id,
                state.notification_id,
                parcel::digest(&bytes)
            )
            .as_bytes(),
        );

        let mentions = CreateAllowedMentions::new()
            .everyone(false)
            .all_users(false)
            .all_roles(false)
            .users(options.recipient.map(UserId::new));
        let builder = CreateMessage::new()
            .content(content)
            .allowed_mentions(mentions)
            .embed(embed)
            .nonce(Nonce::String(nonce[..24].to_owned()))
            .enforce_nonce(true);

        let attachment = CreateAttachment::bytes(bytes, "transfer.parcel.json");

        cancel.check()?;

        let message = channel
            .id
            .send_files(&http, [attachment], builder)
            .await
            .context("Could not publish the parcel manifest to Discord.")?;

        state.manifest_message_id = Some(message.id.to_string());

        save_state(&state_path, &state)?;
    }

    cancel.check()?;

    Ok(UploadResult {
        manifest_path,
        message_link: format!(
            "https://discord.com/channels/{}/{}/{}",
            channel.guild_id,
            channel.id,
            state.manifest_message_id.unwrap()
        ),
        parts: state.manifest.parts.len(),
    })
}

fn checkpoint_matches(candidate: &Manifest, manifest: &Manifest, encrypted: bool) -> bool {
    candidate.sha256 == manifest.sha256
        && candidate.size == manifest.size
        && candidate.chunk_size == manifest.chunk_size
        && candidate.filename == manifest.filename
        && candidate.channel_id == manifest.channel_id
        && candidate.encryption.is_some() == encrypted
}

async fn legacy_checkpoint(
    storage: &Path,
    manifest: &Manifest,
    options: &SendOptions,
    http: &Http,
    channel: ChannelId,
    cancel: &Cancel,
) -> Result<Option<(UploadState, File)>> {
    let directory = storage.join("uploads");
    if !directory.exists() {
        return Ok(None);
    }

    let mut candidates = Vec::new();
    for entry in fs::read_dir(directory)? {
        cancel.check()?;
        let path = entry?.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            continue;
        }
        let saved = parcel::read_bounded(&path, MAX_MANIFEST_SIZE)
            .and_then(|bytes| Ok(serde_json::from_slice::<UploadState>(&bytes)?));
        let Ok(saved) = saved else {
            continue;
        };
        let candidate = &saved.manifest;
        if candidate.validate().is_err()
            || !checkpoint_matches(candidate, manifest, options.password.is_some())
        {
            continue;
        }
        if let Some(encryption) = &candidate.encryption
            && encryption
                .unlock(options.password.as_deref().unwrap_or_default())
                .is_err()
        {
            continue;
        }
        let completed = candidate
            .parts
            .iter()
            .filter(|part| part.remote.is_some())
            .count();
        if completed > 0 {
            candidates.push((completed, path));
        }
    }

    if candidates.is_empty() {
        return Ok(None);
    }
    candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.0));
    cancel.check()?;
    let bot = http
        .get_current_user()
        .await
        .context("Could not verify the upload bot.")?;

    for (_, path) in candidates {
        cancel.check()?;
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        if stem.len() != 64 || !stem.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            continue;
        }
        let lock = parcel::lock_transfer(&storage.join("locks").join(format!("upload-{stem}")))?;
        let saved: UploadState =
            serde_json::from_slice(&parcel::read_bounded(&path, MAX_MANIFEST_SIZE)?)?;
        saved.manifest.validate()?;
        if !checkpoint_matches(&saved.manifest, manifest, options.password.is_some()) {
            continue;
        }
        let Some(remote) = saved
            .manifest
            .parts
            .iter()
            .find_map(|part| part.remote.as_ref())
        else {
            continue;
        };
        let message =
            message_if_exists(http, channel, parse_message_id(&remote.message_id)?).await?;
        if message.is_some_and(|message| message.author.id == bot.id) {
            return Ok(Some((saved, lock)));
        }
    }

    Ok(None)
}

fn save_state(path: &Path, state: &UploadState) -> Result<()> {
    state.manifest.validate()?;

    parcel::atomic_write(path, &serde_json::to_vec_pretty(state)?)
}

pub fn manifest_from_link(link: &str, token: &str, cancel: &Cancel) -> Result<Manifest> {
    runtime()?.block_on(manifest_from_link_async(link, token, cancel))
}

async fn manifest_from_link_async(link: &str, token: &str, cancel: &Cancel) -> Result<Manifest> {
    ensure!(!token.trim().is_empty(), "Enter a Discord bot token.");

    let (channel_id, message_id) = parse_message_link(link)?;

    let http = Http::new(token.trim());

    cancel.check()?;

    let message = channel_id
        .message(&http, message_id)
        .await
        .context("Could not open the Discord message.")?;

    cancel.check()?;

    let attachment = message
        .attachments
        .iter()
        .find(|attachment| {
            attachment
                .filename
                .ends_with(".parcel.json")
        })
        .context(
            "This message has no parcel manifest. Check the link, channel permissions, and Message Content Intent if another bot sent it.",
        )?;

    ensure!(
        u64::from(attachment.size) <= MAX_MANIFEST_SIZE,
        "The parcel manifest is too large."
    );

    let bytes = download_attachment(attachment, MAX_MANIFEST_SIZE, cancel).await?;

    let manifest = Manifest::from_bytes(&bytes)?;

    ensure!(
        manifest.channel_id.as_deref() == Some(&channel_id.to_string(),),
        "This manifest points to a different Discord channel."
    );

    Ok(manifest)
}

pub fn download(
    manifest: &Manifest,
    password: Option<&str>,
    destination: &Path,
    token: &str,
    storage: &Path,
    cancel: &Cancel,
    progress: &dyn Fn(Progress),
) -> Result<PathBuf> {
    runtime()?.block_on(download_async(
        manifest,
        password,
        destination,
        token,
        storage,
        cancel,
        progress,
    ))
}

async fn download_async(
    manifest: &Manifest,
    password: Option<&str>,
    destination: &Path,
    token: &str,
    storage: &Path,
    cancel: &Cancel,
    progress: &dyn Fn(Progress),
) -> Result<PathBuf> {
    manifest.validate()?;
    let encryption_key = manifest
        .encryption
        .as_ref()
        .map(|metadata| metadata.unlock(password.unwrap_or_default()))
        .transpose()?;

    ensure!(
        destination.is_dir(),
        "Choose an existing destination folder."
    );

    ensure!(
        !destination
            .join(parcel::output_filename(&manifest.filename,),)
            .exists(),
        "{} already exists in the destination folder.",
        parcel::output_filename(&manifest.filename)
    );

    ensure!(
        manifest.parts.iter().all(|part| part.remote.is_some()),
        "This parcel is missing Discord attachment information for one or more parts."
    );

    let key = manifest.cache_key()?;

    let _lock = parcel::lock_transfer(&storage.join("locks").join(format!("download-{key}")))?;

    let cache = storage.join("downloads").join(key);

    fs::create_dir_all(&cache)?;
    let cached = crate::disk_space::check_download(
        manifest,
        encryption_key.as_ref(),
        &cache,
        destination,
        cancel,
        progress,
    )?;

    let channel_id = manifest
        .channel_id
        .as_deref()
        .context("The parcel has no source channel.")
        .and_then(parse_channel_id)?;

    let has_token = !token.trim().is_empty();

    let http = has_token.then(|| Http::new(token.trim()));

    let mut done = 0;

    for (part, &cached) in manifest.parts.iter().zip(&cached) {
        cancel.check()?;
        let wire_size = part.size
            + if encryption_key.is_some() {
                crate::crypto::OVERHEAD
            } else {
                0
            };

        progress(Progress::new(
            format!(
                "Downloading part {} of {}",
                part.index + 1,
                manifest.parts.len()
            ),
            done,
            manifest.size,
        ));

        if !cached {
            let remote = part.remote.as_ref().unwrap();

            let bytes = match download_discord_url(&remote.url, wire_size, cancel).await {
                Ok(bytes) => bytes,

                Err(error) if !has_token => {
                    return Err(error);
                }

                Err(_) => {
                    cancel.check()?;

                    let http = http.as_ref().expect("token exists");

                    let message_id = parse_message_id(&remote.message_id)?;

                    let message = channel_id
                        .message(http, message_id)
                        .await
                        .context("Could not refresh a Discord parcel part.")?;

                    let attachment = message
                            .attachments
                            .iter()
                            .find(|attachment| {
                                attachment.id.to_string()
                                    == remote.attachment_id
                                    && u64::from(
                                        attachment.size,
                                    ) == wire_size
                            })
                            .context(
                                "A parcel part is missing or inaccessible. Check channel permissions and Message Content Intent if another bot sent it.",
                            )?;

                    download_attachment(attachment, wire_size, cancel).await?
                }
            };

            let plaintext = if let Some(key) = &encryption_key {
                key.open(&bytes, &crate::crypto::context(manifest, part.index))?
            } else {
                bytes.clone()
            };
            parcel::verify_part(&plaintext, part)?;

            parcel::atomic_write(&parcel::part_path(&cache, part.index), &bytes)?;
        }

        done += part.size;

        progress(Progress::new(
            format!(
                "Received {} of {} parts",
                part.index + 1,
                manifest.parts.len()
            ),
            done,
            manifest.size,
        ));
    }

    cancel.check()?;
    crate::disk_space::check_output(destination, manifest.size)?;
    let output = parcel::assemble(
        manifest,
        encryption_key.as_ref(),
        &cache,
        destination,
        cancel,
        progress,
    )?;

    let _ = fs::remove_dir_all(&cache);

    Ok(output)
}

async fn message_if_exists(
    http: &Http,
    channel_id: ChannelId,
    message_id: MessageId,
) -> Result<Option<Message>> {
    match channel_id.message(http, message_id).await {
        Ok(message) => Ok(Some(message)),

        Err(SerenityError::Http(error)) if error.status_code() == Some(StatusCode::NOT_FOUND) => {
            Ok(None)
        }

        Err(error) => Err(error.into()),
    }
}

async fn download_attachment(
    attachment: &Attachment,
    max_size: u64,
    cancel: &Cancel,
) -> Result<Vec<u8>> {
    ensure!(
        u64::from(attachment.size) <= max_size,
        "Discord attachment is larger than expected."
    );

    download_discord_url(&attachment.url, max_size, cancel).await
}

async fn download_discord_url(url: &str, max_size: u64, cancel: &Cancel) -> Result<Vec<u8>> {
    parcel::validate_attachment_url(url)?;
    let url = url.to_owned();
    let cancel = cancel.clone();
    tokio::task::spawn_blocking(move || {
        crate::attachments::Attachments::new()?.download(&url, max_size, &cancel)
    })
    .await
    .context("Attachment download worker stopped.")?
}

fn parse_channel_id(value: &str) -> Result<ChannelId> {
    let id = parse_snowflake(value, "channel")?;

    Ok(ChannelId::new(id))
}

fn parse_message_id(value: &str) -> Result<MessageId> {
    let id = parse_snowflake(value, "message")?;

    Ok(MessageId::new(id))
}

fn parse_snowflake(value: &str, kind: &str) -> Result<u64> {
    let id = value
        .trim()
        .parse::<u64>()
        .with_context(|| format!("Invalid Discord {kind} ID."))?;

    ensure!(id != 0, "Invalid Discord {kind} ID.");

    Ok(id)
}

fn parse_message_link(link: &str) -> Result<(ChannelId, MessageId)> {
    let link = link.trim();

    let marker = "/channels/";

    let start = link
        .find(marker)
        .context("This is not a Discord message link.")?
        + marker.len();

    let path = link[start..].split(['?', '#']).next().unwrap_or_default();

    let mut parts = path.split('/').filter(|part| !part.is_empty());

    let _guild = parts
        .next()
        .context("This Discord message link is incomplete.")?;

    let channel = parts
        .next()
        .context("This Discord message link is incomplete.")?;

    let message = parts
        .next()
        .context("This Discord message link is incomplete.")?;

    Ok((parse_channel_id(channel)?, parse_message_id(message)?))
}

fn runtime() -> Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("Could not start the Discord runtime.")
}
