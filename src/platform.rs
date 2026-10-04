#[cfg(not(windows))]
pub fn initialize() -> anyhow::Result<()> {
    Ok(())
}

#[cfg(windows)]
pub fn initialize() -> anyhow::Result<()> {
    use anyhow::Context;
    use discord_parcel::parcel;
    use std::{env, fs};

    let executable = env::current_exe().context("Could not locate the application folder.")?;
    let root = executable
        .parent()
        .context("Could not locate the application folder.")?;
    let share = root.join("share");
    if !share.join("discord-parcel/portable.marker").is_file() {
        return Ok(());
    }
    let schemas = share.join("glib-2.0/schemas");
    anyhow::ensure!(
        schemas.join("gschemas.compiled").is_file(),
        "GTK runtime data is missing. Extract the entire ZIP before opening discord-parcel.exe."
    );
    let mut data_paths = vec![share.clone()];
    if let Some(existing) = env::var_os("XDG_DATA_DIRS") {
        data_paths.extend(env::split_paths(&existing));
    }
    let data_paths = env::join_paths(data_paths)?;

    let template = share.join("discord-parcel/loaders.cache.in");
    let module_file = if template.is_file() {
        let root_text = root.to_string_lossy().replace('\\', "/");
        let contents = fs::read_to_string(&template)
            .context("Could not read the bundled image-loader configuration.")?
            .replace("@PARCEL_ROOT@", &root_text);
        let key = parcel::digest(contents.as_bytes());
        let path = parcel::data_dir()
            .join("runtime")
            .join(format!("loaders-{key}.cache"));
        if !path.is_file() {
            parcel::atomic_write(&path, contents.as_bytes())?;
        }
        Some(path)
    } else {
        None
    };

    unsafe {
        env::set_var("GSETTINGS_SCHEMA_DIR", schemas);
        env::set_var("XDG_DATA_DIRS", data_paths);
        if let Some(module_file) = module_file {
            env::set_var("GDK_PIXBUF_MODULE_FILE", module_file);
        }
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn startup_error(message: &str) {
    eprintln!("{message}");
}

#[cfg(windows)]
pub fn startup_error(message: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
    let title: Vec<u16> = "Discord Parcel\0".encode_utf16().collect();
    let message: Vec<u16> = message
        .replace('\0', "")
        .encode_utf16()
        .chain(Some(0))
        .collect();
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            message.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
}
