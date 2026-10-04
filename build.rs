fn main() {
    println!("cargo:rerun-if-changed=data/windows/app.ico");
    println!("cargo:rerun-if-changed=data/windows/app.manifest");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && std::env::var_os("CARGO_FEATURE_DESKTOP").is_some()
    {
        winresource::WindowsResource::new()
            .set_icon("data/windows/app.ico")
            .set_manifest_file("data/windows/app.manifest")
            .set("ProductName", "Discord Parcel")
            .set("FileDescription", "Discord Parcel")
            .set("OriginalFilename", "discord-parcel.exe")
            .compile()
            .expect(
                "could not compile Windows resources; install MinGW binutils or the Windows SDK",
            );
    }
}
