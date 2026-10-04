use crate::{
    attachments::Attachments,
    parcel::{Cancel, snowflake},
};
use anyhow::{Context, Result, ensure};
use serenity::{
    all::{
        ChannelId, CreateAllowedMentions, CreateAttachment, CreateMessage, GuildChannel, Message,
        MessageId, Nonce,
    },
    http::{Http, HttpBuilder, HttpError},
};
use std::{future::Future, time::Duration};
use tokio::runtime::Runtime;

pub struct Discord {
    http: Option<Http>,
    attachments: Attachments,
    runtime: Runtime,
}

impl Discord {
    pub fn new(token: &str) -> Result<Self> {
        let token = token.trim();
        let http = if token.is_empty() {
            None
        } else {
            let token = token.strip_prefix("Bot ").unwrap_or(token);
            ensure!(
                !token.is_empty() && !token.chars().any(char::is_whitespace),
                "Enter a valid Discord bot token."
            );
            reqwest::header::HeaderValue::from_str(token)
                .context("The bot token contains invalid characters.")?;
            let client = reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(15))
                .timeout(Duration::from_secs(180))
                .redirect(reqwest::redirect::Policy::none())
                .build()?;
            Some(HttpBuilder::new(token).client(client).build())
        };
        Ok(Self {
            http,
            attachments: Attachments::new()?,
            runtime: tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?,
        })
    }

    pub fn has_token(&self) -> bool {
        self.http.is_some()
    }

    fn http(&self) -> Result<&Http> {
        self.http
            .as_ref()
            .context("Connect a Discord bot in Connection settings first.")
    }

    pub fn channel(&self, channel: &str, cancel: &Cancel) -> Result<GuildChannel> {
        let channel = channel_id(channel)?;
        let http = self.http()?;
        self.request(cancel, || http.get_channel(channel))?
            .guild()
            .context("Choose a Discord server channel.")
    }

    pub fn participants(&self, channel: &str, cancel: &Cancel) -> Result<Vec<(String, String)>> {
        let channel = channel_id(channel)?;
        let http = self.http()?;
        let messages = self.request(cancel, || http.get_messages(channel, None, Some(100)))?;
        let mut people = std::collections::BTreeMap::new();

        for message in messages {
            for user in std::iter::once(message.author).chain(message.mentions) {
                if !user.bot {
                    let display = user.global_name.as_deref().unwrap_or(&user.name);
                    people.insert(user.id.to_string(), format!("{} (@{})", display, user.name));
                }
            }
        }

        Ok(people.into_iter().collect())
    }

    pub fn message(&self, channel: &str, message: &str, cancel: &Cancel) -> Result<Message> {
        let channel = channel_id(channel)?;
        ensure!(snowflake(message), "Invalid Discord message ID.");
        let message = MessageId::new(message.parse()?);
        let http = self.http()?;
        self.request(cancel, || http.get_message(channel, message))
    }

    pub fn message_if_exists(
        &self,
        channel: &str,
        message: &str,
        cancel: &Cancel,
    ) -> Result<Option<Message>> {
        match self.message(channel, message, cancel) {
            Ok(message) => Ok(Some(message)),
            Err(error)
                if error
                    .downcast_ref::<serenity::Error>()
                    .and_then(status_code)
                    == Some(404) =>
            {
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    pub fn upload(
        &self,
        channel: &str,
        filename: &str,
        bytes: &[u8],
        content: &str,
        nonce: &str,
        cancel: &Cancel,
    ) -> Result<Message> {
        let channel = channel_id(channel)?;
        let http = self.http()?;
        let message = CreateMessage::new()
            .content(content)
            .allowed_mentions(CreateAllowedMentions::new())
            .nonce(Nonce::String(nonce.to_owned()))
            .enforce_nonce(true)
            .add_file(CreateAttachment::bytes(bytes.to_vec(), filename));
        self.request(cancel, || channel.send_message(http, message.clone()))
    }

    pub fn download(&self, url: &str, limit: u64, cancel: &Cancel) -> Result<Vec<u8>> {
        self.attachments.download(url, limit, cancel)
    }

    fn request<T, F, Fut>(&self, cancel: &Cancel, request: F) -> Result<T>
    where
        F: Fn() -> Fut,
        Fut: Future<Output = serenity::Result<T>>,
    {
        cancel.check()?;
        self.runtime.block_on(async {
            let operation = async {
                for attempt in 0..4 {
                    match request().await {
                        Ok(value) => return Ok(value),
                        Err(error) if attempt < 3 && retryable(&error) => {
                            tokio::time::sleep(Duration::from_secs(1 << attempt)).await;
                        }
                        Err(error) => return Err(api_error(error)),
                    }
                }
                unreachable!("last attempt returns its error")
            };
            tokio::select! {
                result = tokio::time::timeout(Duration::from_secs(180), operation) => {
                    result.context("Discord did not finish the request within 180 seconds. Resume to retry.")?
                }
                result = async {
                    loop {
                        cancel.check()?;
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                } => result,
            }
        })
    }
}

fn channel_id(channel: &str) -> Result<ChannelId> {
    ensure!(snowflake(channel), "Enter a numeric Discord channel ID.");
    Ok(ChannelId::new(channel.parse()?))
}

fn status_code(error: &serenity::Error) -> Option<u16> {
    match error {
        serenity::Error::Http(error) => error.status_code().map(|status| status.as_u16()),
        _ => None,
    }
}

fn retryable(error: &serenity::Error) -> bool {
    matches!(error, serenity::Error::Http(HttpError::Request(_)))
        || status_code(error).is_some_and(|status| (500..600).contains(&status))
}

fn api_error(error: serenity::Error) -> anyhow::Error {
    let context = match status_code(&error) {
        Some(401) => "Discord rejected the bot token. Check Connection settings.",
        Some(403) => {
            "The bot cannot access this channel. Check View Channel, Send Messages, Attach Files, and Read Message History permissions."
        }
        Some(404) => "The Discord channel or message no longer exists, or the bot cannot see it.",
        Some(413) => {
            "Discord rejected the attachment size. Choose a smaller chunk size and start a new transfer."
        }
        _ => "Could not complete the Discord request.",
    };
    anyhow::Error::new(error).context(context)
}

pub fn parse_message_link(link: &str) -> Result<(String, String)> {
    let url = reqwest::Url::parse(link.trim()).context("Paste a Discord message link.")?;
    ensure!(
        url.scheme() == "https"
            && matches!(
                url.host_str(),
                Some("discord.com" | "canary.discord.com" | "ptb.discord.com")
            )
            && url.username().is_empty()
            && url.password().is_none()
            && url.port().is_none(),
        "Use a Discord HTTPS message link."
    );
    let segments: Vec<_> = url
        .path_segments()
        .context("Invalid message link.")?
        .collect();
    ensure!(
        segments.len() == 4
            && segments[0] == "channels"
            && (segments[1] == "@me" || snowflake(segments[1]))
            && snowflake(segments[2])
            && snowflake(segments[3]),
        "Paste a link to a specific Discord message."
    );
    Ok((segments[2].to_owned(), segments[3].to_owned()))
}
