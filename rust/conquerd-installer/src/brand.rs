//! User-facing product names for the installer.
//!
//! Crate names stay `conquerd-installer`; only the Windows bundle, shortcuts,
//! and GUI strings use DoubleSlash.

pub const PRODUCT_NAME: &str = "DoubleSlash";
pub const WINDOWS_EXE: &str = "DoubleSlash.exe";
pub const WINDOWS_EXE_LEGACY: &str = "ConquerD.exe";
pub const WINDOWS_INSTALL_DIR: &str = "DoubleSlash";
pub const WINDOWS_INSTALL_DIR_LEGACY: &str = "ConquerD";

/// True when `dir` contains a DoubleSlash or legacy ConquerD client exe.
pub fn exe_in(dir: &std::path::Path) -> bool {
    dir.join(WINDOWS_EXE).is_file() || dir.join(WINDOWS_EXE_LEGACY).is_file()
}
