fn main() {
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set("ProductName", "ConquerD");
        res.set("FileDescription", "ConquerD Supernode (QUIC Relay + SFU)");
        res.set("LegalCopyright", "ConquerD Project");
        // Keep ProductVersion in sync with conquerd-client/Cargo.toml version.
        res.set("ProductVersion", env!("CARGO_PKG_VERSION"));
        res.set("FileVersion", env!("CARGO_PKG_VERSION"));
        if let Err(e) = res.compile() {
            eprintln!("cargo:warning=Failed to set Windows resource: {e}");
        }
    }
}
