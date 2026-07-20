use shadow_rs::ShadowBuilder;

fn main() {
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
