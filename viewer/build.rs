use shadow_rs::ShadowBuilder;

fn main() {
    println!("cargo:rerun-if-env-changed=EXDVIEWER_VERSION");
    let version = std::env::var("EXDVIEWER_VERSION")
        .ok()
        .filter(|version| !version.is_empty())
        .unwrap_or_else(|| {
            format!(
                "CN-{}.0",
                std::env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION 环境变量不可用")
            )
        });
    println!("cargo:rustc-env=EXDVIEWER_VERSION={version}");

    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|target_os| target_os == "windows") {
        println!("cargo:rerun-if-changed=assets/icon.ico");
        winres::WindowsResource::new()
            .set_icon("assets/icon.ico")
            .compile()
            .unwrap();
    }

    ShadowBuilder::builder().build().unwrap();
}
