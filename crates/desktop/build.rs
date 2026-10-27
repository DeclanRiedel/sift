fn main() {
    println!("cargo:rerun-if-changed=assets/windows.rc.in");
    println!("cargo:rerun-if-changed=assets/sift-icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let version = std::env::var("CARGO_PKG_VERSION").expect("Cargo package version");
        let [major, minor, patch] = [
            "CARGO_PKG_VERSION_MAJOR",
            "CARGO_PKG_VERSION_MINOR",
            "CARGO_PKG_VERSION_PATCH",
        ]
        .map(|name| {
            std::env::var(name)
                .expect("Cargo version component")
                .parse::<u16>()
                .expect("Windows version components must fit 16 bits")
        });
        let icon = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap())
            .join("assets/sift-icon.ico")
            .to_string_lossy()
            .replace('\\', "/");
        let resource = include_str!("assets/windows.rc.in")
            .replace("{{SIFT_ICON_FILE}}", &icon)
            .replace(
                "{{SIFT_VERSION_NUMERIC}}",
                &format!("{major},{minor},{patch},0"),
            )
            .replace("{{SIFT_VERSION_TEXT}}", &version);
        let resource_path =
            std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("sift-windows.rc");
        std::fs::write(&resource_path, resource).expect("write Windows version resource");
        embed_resource::compile(resource_path, embed_resource::NONE)
            .manifest_required()
            .expect("compile the Sift Windows application icon and version");
    }
}
