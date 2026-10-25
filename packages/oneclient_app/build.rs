fn main() {
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("icons/icon.ico");

        let version = env!("CARGO_PKG_VERSION");
        res.set("CompanyName", "SpiderClient");
        res.set("ProductName", "SpiderClient");
        res.set("FileDescription", "SpiderClient Minecraft Launcher");
        res.set(
            "LegalCopyright",
            "© 2026 SpiderClient Contributors; upstream © Polyfrost and contributors",
        );
        res.set("Comments", "Built on OneLauncher. Licensed GPL-3.0-only.");
        res.set("OriginalFilename", "oneclient_app.exe");
        res.set("InternalName", "oneclient_app");
        res.set("FileVersion", version);
        res.set("ProductVersion", version);

        res.compile()
            .expect("failed to embed Windows icon resource");
    }
}
