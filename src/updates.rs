use anyhow::{Context, Result, bail, ensure};
use reqwest::{StatusCode, blocking::Client};
use semver::Version;
use serde::Deserialize;
use std::{io::Read, time::Duration};

pub const RELEASES_URL: &str = "https://github.com/ak4duy/discord-parcel/releases";
const LATEST_RELEASE_API: &str =
    "https://api.github.com/repos/ak4duy/discord-parcel/releases/latest";
const MAX_RESPONSE_SIZE: u64 = 2 * 1024 * 1024;

#[derive(Deserialize)]
struct ReleaseResponse {
    tag_name: String,
    draft: bool,
    prerelease: bool,
}

pub struct Release {
    pub version: Version,
    pub url: String,
    pub newer: bool,
}

pub fn check() -> Result<Option<Release>> {
    let client = Client::builder()
        .user_agent(concat!("DiscordParcel/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;

    let response = client
        .get(LATEST_RELEASE_API)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .context("Could not reach GitHub. Check your internet connection and try again.")?;

    match response.status() {
        StatusCode::NOT_FOUND => return Ok(None),
        StatusCode::FORBIDDEN | StatusCode::TOO_MANY_REQUESTS => {
            bail!(
                "GitHub declined the update check or its request limit was reached. Try again later, or open Releases."
            );
        }
        status => ensure!(
            status.is_success(),
            "GitHub could not complete the update check (HTTP {status}). Try again later."
        ),
    }

    ensure!(
        response
            .content_length()
            .is_none_or(|size| size <= MAX_RESPONSE_SIZE),
        "GitHub returned an unexpectedly large release response."
    );
    let mut bytes = Vec::new();
    response
        .take(MAX_RESPONSE_SIZE + 1)
        .read_to_end(&mut bytes)
        .context("The update check was interrupted. Try again.")?;
    ensure!(
        bytes.len() as u64 <= MAX_RESPONSE_SIZE,
        "GitHub returned an unexpectedly large release response."
    );
    let release: ReleaseResponse =
        serde_json::from_slice(&bytes).context("Could not read GitHub’s release information.")?;
    if release.draft || release.prerelease {
        return Ok(None);
    }

    let version = Version::parse(release.tag_name.trim_start_matches(['v', 'V'])).context(
        "The latest release tag is not a version number. Open Releases to check manually.",
    )?;
    let current = Version::parse(env!("CARGO_PKG_VERSION"))?;
    let newer = version.cmp_precedence(&current).is_gt();
    let mut url = reqwest::Url::parse(RELEASES_URL)?;
    url.path_segments_mut()
        .map_err(|_| anyhow::anyhow!("Invalid release URL."))?
        .push("tag")
        .push(&release.tag_name);

    Ok(Some(Release {
        version,
        url: url.into(),
        newer,
    }))
}
