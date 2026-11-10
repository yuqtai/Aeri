fn main() {
    println!("cargo:rerun-if-changed=ui/zh_CN.po");
    let translations = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("translations");
    let messages = translations.join("zh_CN/LC_MESSAGES");
    std::fs::create_dir_all(&messages).expect("Could not create translation directory");
    std::fs::copy("ui/zh_CN.po", messages.join("aeri.po")).expect("Could not copy Chinese translation");
    let config = slint_build::CompilerConfiguration::new()
        .with_default_translation_context(slint_build::DefaultTranslationContext::None)
        .with_bundled_translations(translations);
    slint_build::compile_with_config("ui/app.slint", config).expect("Could not compile Slint UI");
    println!("cargo:rerun-if-changed=assets/app.ico");
    println!("cargo:rerun-if-changed=assets/app.rc");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("app.res");
        let status = std::process::Command::new("rc.exe")
            .arg("/nologo").arg("/I").arg("assets").arg("/fo").arg(&out).arg("assets/app.rc")
            .status().expect("找不到 Windows 资源编译器，请使用 build.ps1 编译");
        assert!(status.success(), "编译 Windows 图标资源失败");
        println!("cargo:rustc-link-arg={}", out.display());
    }
}

