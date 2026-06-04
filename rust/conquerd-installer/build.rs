fn main() {
    #[cfg(windows)]
    {
        // Embed an explicit "asInvoker" execution-level manifest so that Windows
        // UAC heuristics (which auto-elevate binaries whose names contain words
        // like "install", "setup", or "update") do not trigger a UAC prompt.
        // The installer writes only to %LOCALAPPDATA%\ConquerD and user-owned
        // shortcuts, so no elevation is required or desired.
        const MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false"/>
      </requestedPrivileges>
    </security>
  </trustInfo>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <!-- Windows 10 and 11 -->
      <supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"/>
    </application>
  </compatibility>
</assembly>"#;

        let mut res = winresource::WindowsResource::new();
        res.set_icon("../../assets/conquerd.ico");
        res.set("ProductName", "ConquerD");
        res.set("FileDescription", "ConquerD Installer / Updater");
        res.set("LegalCopyright", "ConquerD Project");
        // Keep ProductVersion in sync with conquerd-client/Cargo.toml version.
        res.set("ProductVersion", env!("CARGO_PKG_VERSION"));
        res.set("FileVersion", env!("CARGO_PKG_VERSION"));
        res.set_manifest(MANIFEST);
        if let Err(e) = res.compile() {
            eprintln!("cargo:warning=Failed to set Windows resource: {e}");
        }
    }
}
