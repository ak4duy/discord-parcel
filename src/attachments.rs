use crate::parcel::{Cancel, validate_attachment_url};
use anyhow::{Context, Result, bail, ensure};
use reqwest::{
    StatusCode,
    blocking::{Client, Response},
};
use std::{
    io::Read,
    thread,
    time::{Duration, Instant},
};

pub(crate) struct Attachments {
    client: Client,
}

impl Attachments {
    pub(crate) fn new() -> Result<Self> {
        Ok(Self {
            client: Client::builder()
                .user_agent("DiscordParcel/0.1.0")
                .connect_timeout(Duration::from_secs(15))
                .timeout(Duration::from_secs(180))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
        })
    }
    pub(crate) fn download(&self, url: &str, limit: u64, cancel: &Cancel) -> Result<Vec<u8>> {
        validate_attachment_url(url)?;
        for attempt in 0..4 {
            cancel.check()?;
            let response = match self.client.get(url).send() {
                Ok(response) => response,
                Err(_) if attempt < 3 => {
                    wait_for(Duration::from_secs(1 << attempt), cancel)?;
                    continue;
                }
                Err(_) => bail!("Could not download a part. Check your connection and resume."),
            };
            if response.status().is_server_error() && attempt < 3 {
                wait_for(Duration::from_secs(1 << attempt), cancel)?;
                continue;
            }
            if response.status() == StatusCode::TOO_MANY_REQUESTS {
                let delay = response
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<f64>().ok())
                    .unwrap_or(5.0);
                wait_for(retry_delay(delay)?, cancel)?;
                continue;
            }
            ensure!(
                response.status().is_success(),
                "Attachment unavailable (HTTP {}). Its link may have expired. Connect a bot with access to the source channel to refresh it.",
                response.status().as_u16()
            );
            return read_response(response, limit, cancel);
        }
        bail!("Discord's attachment server is busy. Try again later.")
    }
}

fn read_response(mut response: Response, limit: u64, cancel: &Cancel) -> Result<Vec<u8>> {
    ensure!(
        response.content_length().is_none_or(|size| size <= limit),
        "Discord response exceeds the expected size."
    );
    let mut result = Vec::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        cancel.check()?;
        let count = response
            .read(&mut buffer)
            .context("The download was interrupted. Resume to retry.")?;
        if count == 0 {
            break;
        }
        ensure!(
            result.len() as u64 + count as u64 <= limit,
            "Discord response exceeds the expected size."
        );
        result.extend_from_slice(&buffer[..count]);
    }
    Ok(result)
}

fn retry_delay(seconds: f64) -> Result<Duration> {
    ensure!(
        seconds.is_finite() && (0.0..=3600.0).contains(&seconds),
        "Discord requested a long pause. Try the transfer again later."
    );
    Ok(Duration::from_secs_f64(seconds + 0.1))
}

fn wait_for(duration: Duration, cancel: &Cancel) -> Result<()> {
    wait_until(Instant::now() + duration, cancel)
}

fn wait_until(until: Instant, cancel: &Cancel) -> Result<()> {
    while Instant::now() < until {
        cancel.check()?;
        thread::sleep(
            until
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(100)),
        );
    }
    cancel.check()
}
