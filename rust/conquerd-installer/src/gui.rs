use crate::{extract, github, manifest, release_manifest, shortcuts, state};
use eframe::egui;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const LOGO_BYTES: &[u8] = include_bytes!("../../conquerd-client/qml/icons/logo-full.svg");
const ICO_BYTES: &[u8] = include_bytes!("../../../assets/conquerd.ico");

/// Match the native client's Discord-dark palette (`Theme.qml`).
fn apply_dark_theme(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.window_fill = egui::Color32::from_rgb(0x11, 0x12, 0x14);
    visuals.panel_fill = egui::Color32::from_rgb(0x1E, 0x1F, 0x22);
    visuals.faint_bg_color = egui::Color32::from_rgb(0x2B, 0x2D, 0x31);
    visuals.extreme_bg_color = egui::Color32::from_rgb(0x38, 0x3A, 0x40);
    visuals.widgets.noninteractive.fg_stroke.color = egui::Color32::from_rgb(0xDC, 0xDD, 0xDE);
    visuals.widgets.inactive.fg_stroke.color = egui::Color32::from_rgb(0xDC, 0xDD, 0xDE);
    visuals.widgets.inactive.bg_fill = egui::Color32::from_rgb(0x2B, 0x2D, 0x31);
    visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(0x38, 0x3A, 0x40);
    visuals.widgets.active.bg_fill = egui::Color32::from_rgb(0x38, 0x3A, 0x40);
    visuals.widgets.open.bg_fill = egui::Color32::from_rgb(0x38, 0x3A, 0x40);
    visuals.selection.bg_fill = egui::Color32::from_rgba_premultiplied(88, 101, 242, 60);
    visuals.hyperlink_color = egui::Color32::from_rgb(0x58, 0x65, 0xF2);
    visuals.override_text_color = Some(egui::Color32::from_rgb(0xDC, 0xDD, 0xDE));
    ctx.set_visuals(visuals);
}

// ── Public config passed from main ──────────────────────────────────────────

pub struct GuiConfig {
    pub archive: Option<PathBuf>,
    pub base_dir: PathBuf,
    pub no_shortcuts: bool,
    pub repo: String,
    pub kill: bool,
    pub install_state: state::InstallState,
    pub repair: bool,
    /// When true (only for `--launch` / `--update-and-relaunch`), the
    /// Launching page may start the app after an update check. Otherwise the
    /// user must click Launch explicitly.
    pub auto_launch: bool,
    /// When true, open on the animated splash and run a background update
    /// check (install if needed) without auto-launching the app.
    pub splash_update: bool,
    /// When true, copy this installer into the install dir even if the app is
    /// already current (`--update-installer`).
    pub update_installer: bool,
}

// ── Internal types ──────────────────────────────────────────────────────────

#[derive(Clone, PartialEq)]
enum Page {
    /// Animated splash while checking / downloading / installing updates.
    Splash,
    /// Already up-to-date — brief splash then auto-launch
    Launching,
    /// First-time or update-available welcome
    Welcome,
    Checking,
    Downloading,
    Installing,
    /// Verifying / repairing an existing installation
    Repairing,
    Complete,
    Error,
}

#[derive(Clone, PartialEq)]
enum HashStatus {
    NotChecked,
    Verified,
    NoChecksumFile,
    Mismatch(String),
}

#[derive(Clone)]
struct AppState {
    page: Page,
    base_dir: PathBuf,
    archive: Option<PathBuf>,
    no_shortcuts: bool,
    repo: String,
    nightly: bool,
    kill: bool,
    progress_text: String,
    files_extracted: usize,
    files_total: usize,
    error_message: String,
    install_started: bool,
    hash_status: HashStatus,
    // Download state
    download_bytes: u64,
    download_total: u64,
    // Version info
    installed_version: String,
    target_version: String,
    install_state: state::InstallState,
    // Repair mode
    repair_mode: bool,
    repair_damaged: usize,
    repair_checked: usize,
    repair_total: usize,
    auto_launch: bool,
    splash_update: bool,
    up_to_date_only: bool,
    /// Running exe differs from `%LOCALAPPDATA%\ConquerD\conquerd-installer.exe`.
    installer_update_available: bool,
    /// User opt-in to refresh the installed launcher (checkbox / CLI default).
    update_installer_too: bool,
    installer_update_started: bool,
}

// ── Entry point ─────────────────────────────────────────────────────────────

pub fn run_gui(config: GuiConfig) -> anyhow::Result<()> {
    let has_current = config
        .install_state
        .current_path()
        .map(|p| state::find_exe(p).is_some())
        .unwrap_or(false);

    // Validate SHA-256 for local archives
    let hash_status = if let Some(ref arc) = config.archive {
        match crate::validate_sha256(arc) {
            Ok(true) => HashStatus::Verified,
            Ok(false) => HashStatus::NoChecksumFile,
            Err(e) => HashStatus::Mismatch(format!("{e}")),
        }
    } else {
        HashStatus::NotChecked
    };

    // Determine starting page
    let start_page = if config.repair && has_current {
        // Repair mode — go straight to repairing
        Page::Repairing
    } else if config.archive.is_some() {
        // Local archive provided — show install welcome
        Page::Welcome
    } else if config.splash_update {
        Page::Splash
    } else if has_current && config.auto_launch {
        // Runner mode — brief update check then auto-launch when up to date.
        Page::Launching
    } else if has_current {
        // Installed — manage updates without starting the app.
        Page::Welcome
    } else {
        // No install, no archive — show welcome to download
        Page::Welcome
    };

    let installed_version = config.install_state.current_version.clone();
    let nightly = github::is_nightly_installer() || config.install_state.is_nightly_channel();
    let installer_update_available =
        state::needs_installer_update(&config.base_dir, nightly).unwrap_or(false);
    let update_installer_too = config.update_installer || installer_update_available;

    let state = Arc::new(Mutex::new(AppState {
        page: start_page,
        base_dir: config.base_dir,
        archive: config.archive,
        no_shortcuts: config.no_shortcuts,
        repo: config.repo,
        nightly,
        kill: config.kill,
        progress_text: String::new(),
        files_extracted: 0,
        files_total: 0,
        error_message: String::new(),
        install_started: false,
        hash_status,
        download_bytes: 0,
        download_total: 0,
        installed_version: installed_version.clone(),
        target_version: String::new(),
        install_state: config.install_state,
        repair_mode: config.repair,
        repair_damaged: 0,
        repair_checked: 0,
        repair_total: 0,
        auto_launch: config.auto_launch,
        splash_update: config.splash_update,
        up_to_date_only: false,
        installer_update_available,
        update_installer_too,
        installer_update_started: false,
    }));

    let icon = load_icon_data();
    let window_size = if config.splash_update {
        [400.0, 280.0]
    } else {
        [520.0, 400.0]
    };
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size(window_size)
        .with_resizable(false)
        .with_maximize_button(false);
    if let Some(icon) = icon {
        viewport = viewport.with_icon(std::sync::Arc::new(icon));
    }
    let options = eframe::NativeOptions {
        viewport,
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };

    eframe::run_native(
        "ConquerD Installer",
        options,
        Box::new(move |cc| {
            apply_dark_theme(&cc.egui_ctx);
            egui_extras::install_image_loaders(&cc.egui_ctx);
            Ok(Box::new(InstallerApp {
                state: state.clone(),
                launched_check: false,
                launched_repair: false,
                splash_started: false,
            }))
        }),
    )
    .map_err(|e| anyhow::anyhow!("GUI error: {e}"))
}

fn load_icon_data() -> Option<egui::IconData> {
    let img = image::load_from_memory(ICO_BYTES).ok()?.to_rgba8();
    Some(egui::IconData {
        width: img.width(),
        height: img.height(),
        rgba: img.into_raw(),
    })
}

// ── App struct ──────────────────────────────────────────────────────────────

struct InstallerApp {
    state: Arc<Mutex<AppState>>,
    // logo rendered directly via egui_extras SVG loader
    /// Whether we've kicked off the background update check on the Launching page
    launched_check: bool,
    /// Whether we've kicked off the background repair check
    launched_repair: bool,
    /// Whether the splash update worker has been started
    splash_started: bool,
}

impl eframe::App for InstallerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let page = {
            let st = self.state.lock().unwrap();
            st.page.clone()
        };

        let splash_active = {
            let st = self.state.lock().unwrap();
            st.splash_update
                && matches!(
                    st.page,
                    Page::Splash | Page::Checking | Page::Downloading | Page::Installing
                )
        };

        egui::CentralPanel::default().show(ctx, |ui| {
            if splash_active {
                self.show_splash(ui, ctx);
            } else {
                // Logo — SVG rendered via egui_extras loader, capped at 80 px tall
                ui.add_space(16.0);
                ui.vertical_centered(|ui| {
                    ui.add(
                        egui::Image::from_bytes("bytes://logo.svg", LOGO_BYTES)
                            .max_height(80.0)
                            .maintain_aspect_ratio(true),
                    );
                });
                ui.add_space(12.0);

                ui.separator();
                ui.add_space(10.0);

                match page {
                    Page::Launching => self.show_launching(ui, ctx),
                    Page::Welcome => self.show_welcome(ui, ctx),
                    Page::Checking => self.show_checking(ui, ctx),
                    Page::Downloading => self.show_downloading(ui, ctx),
                    Page::Installing => self.show_installing(ui, ctx),
                    Page::Repairing => self.show_repairing(ui, ctx),
                    Page::Complete => self.show_complete(ui, ctx),
                    Page::Error => self.show_error(ui, ctx),
                    Page::Splash => self.show_splash(ui, ctx),
                }
            }
        });
    }
}

impl InstallerApp {
    // ── Splash (animated logo + background update check) ─────────────────

    fn show_splash(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        if !self.splash_started {
            self.splash_started = true;
            let state = self.state.clone();
            let ctx = ctx.clone();
            std::thread::spawn(move || run_splash_update_work(&state, &ctx));
        }

        let (page, progress_text, download_bytes, download_total, files_extracted, files_total) = {
            let st = self.state.lock().unwrap();
            (
                st.page.clone(),
                st.progress_text.clone(),
                st.download_bytes,
                st.download_total,
                st.files_extracted,
                st.files_total,
            )
        };

        ui.add_space(24.0);
        paint_animated_logo(ui, ctx);
        ui.add_space(20.0);

        ui.vertical_centered(|ui| {
            let status = if !progress_text.is_empty() {
                progress_text
            } else {
                match page {
                    Page::Checking => "Checking for updates\u{2026}".to_owned(),
                    Page::Downloading => "Downloading update\u{2026}".to_owned(),
                    Page::Installing => "Installing update\u{2026}".to_owned(),
                    _ => "Preparing\u{2026}".to_owned(),
                }
            };
            ui.label(egui::RichText::new(status).size(15.0));

            ui.add_space(14.0);

            if page == Page::Downloading && download_total > 0 {
                let frac = download_bytes as f32 / download_total as f32;
                ui.add(
                    egui::ProgressBar::new(frac)
                        .desired_width(260.0)
                        .show_percentage(),
                );
            } else if page == Page::Installing && files_total > 0 {
                let frac = files_extracted as f32 / files_total as f32;
                ui.add(
                    egui::ProgressBar::new(frac)
                        .desired_width(260.0)
                        .show_percentage(),
                );
            } else {
                ui.add(egui::Spinner::new().size(28.0));
            }
        });

        ctx.request_repaint();
    }

    // ── Launching page (already installed, checking for updates) ─────────

    fn show_launching(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let version = {
            let st = self.state.lock().unwrap();
            st.installed_version.clone()
        };

        ui.vertical_centered(|ui| {
            ui.heading(format!("ConquerD v{version}"));
            ui.add_space(15.0);
            ui.spinner();
            ui.add_space(10.0);
            ui.label("Checking for updates\u{2026}");
        });

        // Kick off background update check once
        if !self.launched_check {
            self.launched_check = true;
            let state = self.state.clone();
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                check_and_maybe_launch(&state, &ctx);
            });
        }

        ctx.request_repaint();
    }

    // ── Welcome page ────────────────────────────────────────────────────

    fn show_welcome(&self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.vertical_centered(|ui| {
            ui.heading("Welcome to ConquerD");
            ui.add_space(10.0);

            let (
                base_dir,
                archive,
                hash_status,
                installed_ver,
                target_ver,
                installer_update_available,
                mut update_installer_too,
            ) = {
                let st = self.state.lock().unwrap();
                (
                    st.base_dir.display().to_string(),
                    st.archive.clone(),
                    st.hash_status.clone(),
                    st.installed_version.clone(),
                    st.target_version.clone(),
                    st.installer_update_available,
                    st.update_installer_too,
                )
            };

            ui.label(format!("Install location: {base_dir}"));
            ui.add_space(5.0);

            if !installed_ver.is_empty() && !target_ver.is_empty() {
                ui.label(format!(
                    "Update available: v{installed_ver} \u{2192} v{target_ver}"
                ));
                ui.add_space(10.0);
                if ui.button("   Update & Launch   ").clicked() {
                    self.start_download(ctx);
                }
            } else if let Some(ref arc) = archive {
                let name = arc
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| arc.display().to_string());

                match &hash_status {
                    HashStatus::Verified => {
                        ui.label(format!("Archive: {name}"));
                        ui.colored_label(
                            egui::Color32::from_rgb(80, 200, 80),
                            "\u{2714} SHA-256 verified",
                        );
                    }
                    HashStatus::NoChecksumFile => {
                        ui.label(format!("Archive: {name}"));
                        ui.colored_label(
                            egui::Color32::YELLOW,
                            "No .sha256 file \u{2014} skipping verification",
                        );
                    }
                    HashStatus::Mismatch(err) => {
                        ui.label(format!("Archive: {name}"));
                        ui.colored_label(egui::Color32::RED, err);
                    }
                    HashStatus::NotChecked => {
                        ui.label(format!("Archive: {name}"));
                    }
                }

                ui.add_space(5.0);

                if !installed_ver.is_empty() {
                    ui.label(format!("v{installed_ver} is currently installed."));
                    ui.add_space(5.0);
                }

                if matches!(hash_status, HashStatus::Mismatch(_)) {
                    ui.label("Installation blocked due to checksum mismatch.");
                } else if !installed_ver.is_empty() {
                    if installer_update_available {
                        ui.label("A newer launcher was detected (this .exe).");
                        ui.checkbox(&mut update_installer_too, "Also update installed launcher");
                        ui.add_space(5.0);
                        if ui.button("   Update Launcher   ").clicked() {
                            {
                                let mut st = self.state.lock().unwrap();
                                st.update_installer_too = update_installer_too;
                            }
                            self.start_installer_update(ctx);
                        }
                        ui.add_space(10.0);
                    }
                    ui.label("This will reinstall ConquerD and create shortcuts.");
                    ui.add_space(20.0);
                    ui.horizontal(|ui| {
                        if ui.button("   Reinstall   ").clicked() {
                            self.start_install(ctx);
                        }
                        ui.add_space(10.0);
                        if ui.button("   Repair Installation   ").clicked() {
                            self.start_repair(ctx);
                        }
                    });
                    {
                        let mut st = self.state.lock().unwrap();
                        st.update_installer_too = update_installer_too;
                    }
                } else {
                    ui.label("This will install ConquerD and create shortcuts.");
                    ui.add_space(20.0);
                    if ui.button("   Install   ").clicked() {
                        self.start_install(ctx);
                    }
                }
            } else {
                ui.add_space(5.0);
                ui.label("No local archive found.");
                ui.add_space(10.0);
                ui.label("Click below to download the latest release from GitHub.");
                ui.add_space(20.0);

                if ui.button("   Download & Install   ").clicked() {
                    self.start_download(ctx);
                }

                if !installed_ver.is_empty() {
                    ui.add_space(10.0);
                    ui.separator();
                    ui.add_space(5.0);
                    ui.label(format!("v{installed_ver} is currently installed."));
                    ui.add_space(5.0);
                    if installer_update_available {
                        ui.label("A newer launcher was detected (this .exe).");
                        ui.checkbox(&mut update_installer_too, "Also update installed launcher");
                        ui.add_space(5.0);
                        if ui.button("   Update Launcher   ").clicked() {
                            {
                                let mut st = self.state.lock().unwrap();
                                st.update_installer_too = update_installer_too;
                            }
                            self.start_installer_update(ctx);
                        }
                        ui.add_space(5.0);
                    }
                    if ui.button("   Repair Installation   ").clicked() {
                        self.start_repair(ctx);
                    }
                    {
                        let mut st = self.state.lock().unwrap();
                        st.update_installer_too = update_installer_too;
                    }
                }
            }
        });
    }

    // ── Checking / Downloading / Installing / Complete / Error ───────────

    fn show_checking(&self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.vertical_centered(|ui| {
            ui.heading("Checking for latest release\u{2026}");
            ui.add_space(15.0);
            ui.spinner();
            ui.add_space(10.0);
            ui.label("Contacting GitHub\u{2026}");
        });
        ctx.request_repaint();
    }

    fn show_downloading(&self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let (bytes, total, text) = {
            let st = self.state.lock().unwrap();
            (
                st.download_bytes,
                st.download_total,
                st.progress_text.clone(),
            )
        };

        ui.vertical_centered(|ui| {
            ui.heading("Downloading\u{2026}");
            ui.add_space(15.0);

            if total > 0 {
                let frac = bytes as f32 / total as f32;
                ui.add(egui::ProgressBar::new(frac).show_percentage());
                ui.add_space(5.0);
                ui.label(format!(
                    "{:.1} / {:.1} MB",
                    bytes as f64 / 1_048_576.0,
                    total as f64 / 1_048_576.0,
                ));
            } else {
                ui.spinner();
                ui.add_space(5.0);
                ui.label(format!("{:.1} MB downloaded", bytes as f64 / 1_048_576.0));
            }

            if !text.is_empty() {
                ui.add_space(5.0);
                ui.label(&text);
            }
        });
        ctx.request_repaint();
    }

    fn show_installing(&self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let (progress_text, files_extracted, files_total) = {
            let st = self.state.lock().unwrap();
            (st.progress_text.clone(), st.files_extracted, st.files_total)
        };

        ui.vertical_centered(|ui| {
            ui.heading("Installing\u{2026}");
            ui.add_space(15.0);

            if files_total > 0 {
                let frac = files_extracted as f32 / files_total as f32;
                ui.add(egui::ProgressBar::new(frac).show_percentage());
                ui.add_space(5.0);
                ui.label(format!("{files_extracted} / {files_total} files"));
            } else {
                ui.spinner();
            }

            if !progress_text.is_empty() {
                ui.add_space(10.0);
                ui.label(&progress_text);
            }
        });
        ctx.request_repaint();
    }

    fn show_complete(&self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let (
            version,
            repair_mode,
            progress_text,
            up_to_date_only,
            installer_update_available,
            mut update_installer_too,
        ) = {
            let st = self.state.lock().unwrap();
            (
                st.target_version.clone(),
                st.repair_mode,
                st.progress_text.clone(),
                st.up_to_date_only,
                st.installer_update_available,
                st.update_installer_too,
            )
        };

        ui.vertical_centered(|ui| {
            if repair_mode {
                ui.heading("Repair Complete!");
                ui.add_space(15.0);
                ui.label(&progress_text);
            } else if up_to_date_only {
                ui.heading(format!("ConquerD v{version}"));
                ui.add_space(15.0);
                ui.label("You\u{2019}re on the latest version.");
                if !progress_text.is_empty() {
                    ui.add_space(5.0);
                    ui.label(&progress_text);
                } else if installer_update_available {
                    ui.add_space(5.0);
                    ui.label("The installed launcher can be updated to this copy.");
                    ui.checkbox(&mut update_installer_too, "Update installed launcher");
                }
            } else if version.is_empty() {
                ui.heading("Installation Complete!");
                ui.add_space(15.0);
                ui.label("ConquerD has been installed successfully.");
            } else {
                ui.heading(format!("ConquerD v{version} Installed!"));
                ui.add_space(15.0);
                ui.label("ConquerD has been installed successfully.");
            }
            ui.add_space(10.0);

            if installer_update_available && update_installer_too {
                if ui.button("   Update Launcher   ").clicked() {
                    {
                        let mut st = self.state.lock().unwrap();
                        st.update_installer_too = update_installer_too;
                    }
                    self.start_installer_update(ctx);
                    return;
                }
                ui.add_space(5.0);
            } else {
                {
                    let mut st = self.state.lock().unwrap();
                    st.update_installer_too = update_installer_too;
                }
            }

            if ui.button("   Launch ConquerD   ").clicked() {
                let st = self.state.lock().unwrap();
                if let Some(dir) = st.install_state.current_path() {
                    let _ = crate::launch_app(dir);
                }
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }

            ui.add_space(5.0);
            if ui.button("   Close   ").clicked() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        });
    }

    fn show_error(&self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let error_msg = {
            let st = self.state.lock().unwrap();
            st.error_message.clone()
        };

        ui.vertical_centered(|ui| {
            ui.heading("Error");
            ui.add_space(15.0);
            ui.colored_label(egui::Color32::RED, &error_msg);
            ui.add_space(20.0);

            if ui.button("   Close   ").clicked() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        });
    }

    // ── Actions ─────────────────────────────────────────────────────────

    fn start_download(&self, ctx: &egui::Context) {
        {
            let mut st = self.state.lock().unwrap();
            st.page = Page::Checking;
        }

        let state = self.state.clone();
        let ctx = ctx.clone();

        std::thread::spawn(move || {
            let result = run_download_and_install(&state, &ctx);
            let mut st = state.lock().unwrap();
            match result {
                Ok(()) => st.page = Page::Complete,
                Err(e) => {
                    st.error_message = format!("{e:#}");
                    st.page = Page::Error;
                }
            }
            ctx.request_repaint();
        });
    }

    fn start_install(&self, ctx: &egui::Context) {
        {
            let mut st = self.state.lock().unwrap();
            if st.install_started {
                return;
            }
            st.install_started = true;
            st.page = Page::Installing;
            st.progress_text = "Preparing\u{2026}".into();
        }

        let state = self.state.clone();
        let ctx = ctx.clone();

        std::thread::spawn(move || {
            let result = run_local_install(&state, &ctx);
            let mut st = state.lock().unwrap();
            match result {
                Ok(()) => st.page = Page::Complete,
                Err(e) => {
                    st.error_message = format!("{e:#}");
                    st.page = Page::Error;
                }
            }
            ctx.request_repaint();
        });
    }

    fn start_installer_update(&self, ctx: &egui::Context) {
        {
            let mut st = self.state.lock().unwrap();
            if st.installer_update_started {
                return;
            }
            st.installer_update_started = true;
            st.page = Page::Installing;
            st.progress_text = "Updating installed launcher\u{2026}".into();
        }

        let state = self.state.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = run_installer_self_update(&state, &ctx);
            let mut st = state.lock().unwrap();
            match result {
                Ok(msg) => {
                    st.installer_update_available = false;
                    st.progress_text = msg;
                    st.page = Page::Complete;
                }
                Err(e) => {
                    st.error_message = format!("{e:#}");
                    st.page = Page::Error;
                }
            }
            st.installer_update_started = false;
            ctx.request_repaint();
        });
    }

    fn start_repair(&self, _ctx: &egui::Context) {
        {
            let mut st = self.state.lock().unwrap();
            st.page = Page::Repairing;
            st.progress_text = "Preparing repair\u{2026}".into();
            st.repair_mode = true;
        }
        // The show_repairing method kicks off the background thread on first render.
    }

    // ── Repairing page ──────────────────────────────────────────────────

    fn show_repairing(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let (progress_text, checked, total, damaged) = {
            let st = self.state.lock().unwrap();
            (
                st.progress_text.clone(),
                st.repair_checked,
                st.repair_total,
                st.repair_damaged,
            )
        };

        ui.vertical_centered(|ui| {
            ui.heading("Repairing Installation\u{2026}");
            ui.add_space(15.0);

            if total > 0 {
                let frac = checked as f32 / total as f32;
                ui.add(egui::ProgressBar::new(frac).show_percentage());
                ui.add_space(5.0);
                ui.label(format!("{checked} / {total} files checked"));
                if damaged > 0 {
                    ui.add_space(3.0);
                    ui.colored_label(
                        egui::Color32::YELLOW,
                        format!("{damaged} file(s) need repair"),
                    );
                }
            } else {
                ui.spinner();
            }

            if !progress_text.is_empty() {
                ui.add_space(10.0);
                ui.label(&progress_text);
            }
        });

        // Kick off repair once
        if !self.launched_repair {
            self.launched_repair = true;
            let state = self.state.clone();
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                let result = run_repair(&state, &ctx);
                let mut st = state.lock().unwrap();
                match result {
                    Ok(()) => st.page = Page::Complete,
                    Err(e) => {
                        st.error_message = format!("{e:#}");
                        st.page = Page::Error;
                    }
                }
                ctx.request_repaint();
            });
        }

        ctx.request_repaint();
    }
}

// ── Background: check for updates then launch or show update UI ─────────

fn check_and_maybe_launch(app_state: &Arc<Mutex<AppState>>, ctx: &egui::Context) {
    let (repo, nightly, install_state, auto_launch) = {
        let st = app_state.lock().unwrap();
        (
            st.repo.clone(),
            st.nightly,
            st.install_state.clone(),
            st.auto_launch,
        )
    };

    // Try to check GitHub; on failure runner mode still launches what we have.
    let release = match github::fetch_release(&repo, nightly) {
        Ok(r) => r,
        Err(_) => {
            if auto_launch {
                launch_current_and_close(app_state, ctx);
            } else {
                show_welcome_page(app_state, ctx);
            }
            return;
        }
    };

    let needs_update = match github::needs_release_update(&release, &install_state, nightly) {
        Ok(v) => v,
        Err(_) => {
            if auto_launch {
                launch_current_and_close(app_state, ctx);
            } else {
                show_welcome_page(app_state, ctx);
            }
            return;
        }
    };

    if !needs_update {
        if auto_launch {
            launch_current_and_close(app_state, ctx);
        } else {
            show_welcome_page(app_state, ctx);
        }
        return;
    }

    // Update available — switch to Welcome page with update info
    {
        let mut st = app_state.lock().unwrap();
        st.target_version = release.version.clone();
        st.page = Page::Welcome;
    }
    ctx.request_repaint();
}

/// Breathing logo SVG plus orbiting accent dots so the splash feels alive.
fn paint_animated_logo(ui: &mut egui::Ui, ctx: &egui::Context) {
    let t = ctx.input(|i| i.time) as f32;
    let breathe = 1.0 + 0.06 * (t * 2.4).sin();

    ui.vertical_centered(|ui| {
        let width = 300.0;
        let height = 88.0 * breathe;
        let (rect, _) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
        let center = rect.center();
        let painter = ui.painter();

        for i in 0..3 {
            let angle = t * 2.0 + (i as f32) * std::f32::consts::TAU / 3.0;
            let orbit = egui::vec2(angle.cos() * 118.0, angle.sin() * 28.0);
            let alpha = (150.0 + 80.0 * (t * 3.5 + i as f32).sin()).clamp(0.0, 255.0) as u8;
            painter.circle_filled(
                center + orbit,
                4.0,
                egui::Color32::from_rgba_premultiplied(167, 139, 250, alpha),
            );
        }

        let logo_rect = rect.shrink2(egui::vec2(12.0, 10.0));
        let logo = egui::Image::from_bytes("bytes://logo.svg", LOGO_BYTES)
            .fit_to_exact_size(logo_rect.size());
        logo.paint_at(ui, logo_rect);
    });
}

fn run_splash_update_work(app_state: &Arc<Mutex<AppState>>, ctx: &egui::Context) {
    let result = (|| -> anyhow::Result<()> {
        let (repo, nightly, base_dir) = {
            let st = app_state.lock().unwrap();
            (st.repo.clone(), st.nightly, st.base_dir.clone())
        };

        {
            let mut st = app_state.lock().unwrap();
            st.page = Page::Checking;
            st.progress_text = "Checking for updates\u{2026}".into();
        }
        ctx.request_repaint();

        let release = github::fetch_release(&repo, nightly)?;
        let ist = state::read_state(&base_dir)?;
        if !github::needs_release_update(&release, &ist, nightly)? {
            let version = ist.current_version.clone();
            let (update_installer_too, no_shortcuts) = {
                let st = app_state.lock().unwrap();
                (st.update_installer_too, st.no_shortcuts)
            };
            let mut launcher_msg = String::new();
            if update_installer_too && state::needs_installer_update(&base_dir, nightly)? {
                state::update_installed_launcher(&base_dir, nightly, !no_shortcuts)?;
                launcher_msg = "Installed launcher updated.".into();
            }
            let mut st = app_state.lock().unwrap();
            st.target_version = version.clone();
            st.installed_version = version;
            st.up_to_date_only = true;
            st.progress_text = launcher_msg;
            st.installer_update_available = false;
            st.page = Page::Complete;
            return Ok(());
        }

        run_download_and_install(app_state, ctx)
    })();

    let mut st = app_state.lock().unwrap();
    match result {
        Ok(()) => {
            if st.page != Page::Complete {
                st.page = Page::Complete;
            }
        }
        Err(e) => {
            st.error_message = format!("{e:#}");
            st.page = Page::Error;
        }
    }
    ctx.request_repaint();
}

fn show_welcome_page(app_state: &Arc<Mutex<AppState>>, ctx: &egui::Context) {
    let mut st = app_state.lock().unwrap();
    st.page = Page::Welcome;
    ctx.request_repaint();
}

fn launch_current_and_close(app_state: &Arc<Mutex<AppState>>, ctx: &egui::Context) {
    let st = app_state.lock().unwrap();
    if let Some(dir) = st.install_state.current_path() {
        let _ = crate::launch_app(dir);
    }
    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
}

// ── Background: refresh only the installed launcher copy ──────────────────

fn run_installer_self_update(
    app_state: &Arc<Mutex<AppState>>,
    ctx: &egui::Context,
) -> anyhow::Result<String> {
    let (base_dir, nightly, no_shortcuts) = {
        let st = app_state.lock().unwrap();
        (st.base_dir.clone(), st.nightly, st.no_shortcuts)
    };
    {
        let mut st = app_state.lock().unwrap();
        st.progress_text = "Copying launcher\u{2026}".into();
    }
    ctx.request_repaint();
    state::update_installed_launcher(&base_dir, nightly, !no_shortcuts)?;
    Ok("Installed launcher updated.".into())
}

// ── Background: download from GitHub, then install ──────────────────────

fn run_download_and_install(
    app_state: &Arc<Mutex<AppState>>,
    ctx: &egui::Context,
) -> anyhow::Result<()> {
    let (repo, kill, nightly) = {
        let st = app_state.lock().unwrap();
        (st.repo.clone(), st.kill, st.nightly)
    };

    // 1. Fetch latest release info
    let release = github::fetch_release(&repo, nightly)?;

    {
        let mut st = app_state.lock().unwrap();
        st.target_version = release.version.clone();
        st.page = Page::Downloading;
        st.progress_text = format!("Downloading {}\u{2026}", release.archive_name);
    }
    ctx.request_repaint();

    // 2. Download to temp
    let temp_dir = std::env::temp_dir();
    let dest = temp_dir.join(&release.archive_name);

    let dl_state = app_state.clone();
    let dl_ctx = ctx.clone();
    github::download_file(&release.archive_url, &dest, move |bytes, total| {
        if let Ok(mut st) = dl_state.lock() {
            st.download_bytes = bytes;
            st.download_total = total;
        }
        dl_ctx.request_repaint();
    })?;

    // 3. Verify SHA-256
    let mut archive_sha256 = String::new();
    if !release.sha256_url.is_empty() {
        {
            let mut st = app_state.lock().unwrap();
            st.progress_text = "Verifying SHA-256\u{2026}".into();
        }
        ctx.request_repaint();

        let expected = github::fetch_sha256(&release.sha256_url)?;
        github::verify_download(&dest, &expected)?;
        archive_sha256 = expected;
    }

    // 3b. Cross-check against the release manifest when available.
    if !release.manifest_url.is_empty() {
        {
            let mut st = app_state.lock().unwrap();
            st.progress_text = "Verifying release manifest\u{2026}".into();
        }
        ctx.request_repaint();

        let raw_json = github::fetch_release_manifest(&release.manifest_url)?;
        let archive_hash = extract::hash_file(&dest)?;
        release_manifest::verify_archive_hash(
            &raw_json,
            nightly,
            &release.version,
            github::current_platform_id(),
            &archive_hash,
        )?;
    } else if nightly {
        anyhow::bail!("Nightly install requires releases_manifest.json");
    }

    // 4. Kill running instances if requested
    if kill {
        state::kill_running_instances();
    }

    // 5. Extract to versioned directory
    {
        let mut st = app_state.lock().unwrap();
        st.page = Page::Installing;
        st.progress_text = "Extracting files\u{2026}".into();
        st.install_started = true;
    }
    ctx.request_repaint();

    let (base_dir, no_shortcuts) = {
        let st = app_state.lock().unwrap();
        (st.base_dir.clone(), st.no_shortcuts)
    };

    let ver_dir = state::version_dir(&base_dir, &release.version);
    std::fs::create_dir_all(&ver_dir)?;

    let extracted: HashMap<String, String> =
        extract::extract_7z_with_progress(&dest, &ver_dir, |count, total| {
            if let Ok(mut st) = app_state.lock() {
                st.files_extracted = count;
                st.files_total = total;
                st.progress_text = "Hashing files\u{2026}".into();
            }
            ctx.request_repaint();
        })?;

    // 6. Write manifest
    {
        let mut st = app_state.lock().unwrap();
        st.progress_text = "Writing manifest\u{2026}".into();
    }
    ctx.request_repaint();

    let manifest_path = ver_dir.join("manifest.json");
    manifest::write_manifest(&ver_dir, &extracted, &manifest_path)?;

    // 7. Update install state — remove old versions
    {
        let mut st = app_state.lock().unwrap();
        st.progress_text = "Cleaning up old versions\u{2026}".into();
    }
    ctx.request_repaint();

    let mut ist = state::read_state(&base_dir)?;
    let old_versions: Vec<_> = ist.old_versions().iter().map(|v| (*v).clone()).collect();
    for old in &old_versions {
        let _ = std::fs::remove_dir_all(&old.path);
        ist.remove_version(&old.version);
    }

    ist.add_version(&release.version, &ver_dir);
    ist.set_channel(nightly);
    ist.archive_sha256 = archive_sha256;
    state::write_state(&base_dir, &ist)?;
    state::self_copy(&base_dir, nightly)?;

    // 8. Shortcuts
    if !no_shortcuts {
        let mut st = app_state.lock().unwrap();
        st.progress_text = "Creating shortcuts\u{2026}".into();
        drop(st);
        ctx.request_repaint();

        let installer_exe = state::installer_path(&base_dir, nightly);
        shortcuts::create_shortcuts_for_launcher(&installer_exe)?;
    }

    // 9. Update state for the Complete page
    {
        let mut st = app_state.lock().unwrap();
        st.install_state = ist;
        st.installed_version = release.version;
    }

    Ok(())
}

// ── Background: repair an existing installation ─────────────────────────

fn run_repair(app_state: &Arc<Mutex<AppState>>, ctx: &egui::Context) -> anyhow::Result<()> {
    let (base_dir, installed_version) = {
        let st = app_state.lock().unwrap();
        (st.base_dir.clone(), st.installed_version.clone())
    };

    if installed_version.is_empty() {
        anyhow::bail!("No installed version found. Run the installer normally first.");
    }

    let ver_dir = state::version_dir(&base_dir, &installed_version);
    let manifest_path = ver_dir.join("manifest.json");

    // 1. Read the manifest
    {
        let mut st = app_state.lock().unwrap();
        st.progress_text = "Reading manifest\u{2026}".into();
    }
    ctx.request_repaint();

    let m = manifest::read_manifest(&manifest_path)?.ok_or_else(|| {
        anyhow::anyhow!(
            "No manifest.json found in {}. Cannot repair — try a full reinstall.",
            ver_dir.display()
        )
    })?;

    {
        let mut st = app_state.lock().unwrap();
        st.repair_total = m.files.len();
        st.progress_text = "Verifying files\u{2026}".into();
    }
    ctx.request_repaint();

    // 2. Verify all files against the manifest
    let report = extract::verify_install(&ver_dir, &m.files, |checked, total| {
        if let Ok(mut st) = app_state.lock() {
            st.repair_checked = checked;
            st.repair_total = total;
        }
        ctx.request_repaint();
    })?;

    if report.is_ok() {
        {
            let mut st = app_state.lock().unwrap();
            st.progress_text = format!(
                "All {} files verified — installation is intact.",
                report.checked
            );
            st.target_version = installed_version;
        }
        return Ok(());
    }

    // 3. We have damaged files — need an archive to repair from
    let damaged = report.damaged_count();
    {
        let mut st = app_state.lock().unwrap();
        st.repair_damaged = damaged;
        st.progress_text = format!(
            "Found {} changed and {} missing file(s). Re-extracting\u{2026}",
            report.changed.len(),
            report.missing.len(),
        );
    }
    ctx.request_repaint();

    // Look for a local archive or try to download one
    let archive = {
        let st = app_state.lock().unwrap();
        st.archive.clone()
    }
    .or_else(crate::detect_archive);

    let archive = if let Some(arc) = archive {
        arc
    } else {
        // Try downloading the matching version from GitHub
        let (repo, nightly) = {
            let st = app_state.lock().unwrap();
            (st.repo.clone(), st.nightly)
        };
        let release = github::fetch_release(&repo, nightly)?;
        let temp = std::env::temp_dir().join(&release.archive_name);

        {
            let mut st = app_state.lock().unwrap();
            st.progress_text = format!("Downloading {} for repair\u{2026}", release.archive_name);
        }
        ctx.request_repaint();

        let dl_state = app_state.clone();
        let dl_ctx = ctx.clone();
        github::download_file(&release.archive_url, &temp, move |bytes, total| {
            if let Ok(mut st) = dl_state.lock() {
                st.download_bytes = bytes;
                st.download_total = total;
            }
            dl_ctx.request_repaint();
        })?;

        if !release.sha256_url.is_empty() {
            let expected = github::fetch_sha256(&release.sha256_url)?;
            github::verify_download(&temp, &expected)?;
        }

        temp
    };

    // 4. Extract only damaged files from the archive into a temp dir, then
    //    copy them over.  sevenz-rust extracts the full archive, so we
    //    extract to a staging dir and cherry-pick the needed files.
    {
        let mut st = app_state.lock().unwrap();
        st.progress_text = "Extracting archive for repair\u{2026}".into();
    }
    ctx.request_repaint();

    let staging_dir = std::env::temp_dir().join(format!("conquerd_repair_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging_dir); // clean any prior run
    std::fs::create_dir_all(&staging_dir)?;

    extract::extract_7z(&archive, &staging_dir)?;

    // Build the set of files to replace
    let damaged_set: std::collections::HashSet<&str> = report
        .changed
        .iter()
        .chain(report.missing.iter())
        .map(|s| s.as_str())
        .collect();

    let mut repaired = 0usize;
    for rel_path in &damaged_set {
        let src = staging_dir.join(rel_path.replace('/', std::path::MAIN_SEPARATOR_STR));
        let dest = ver_dir.join(rel_path.replace('/', std::path::MAIN_SEPARATOR_STR));

        if src.exists() {
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(&src, &dest)?;
            repaired += 1;

            if let Ok(mut st) = app_state.lock() {
                st.progress_text = format!("Repaired {repaired} / {damaged} files\u{2026}");
            }
            ctx.request_repaint();
        } else {
            eprintln!("Repair: cannot find {rel_path} in archive — skipping");
        }
    }

    // 5. Clean up staging directory
    let _ = std::fs::remove_dir_all(&staging_dir);

    // 6. Update progress
    {
        let mut st = app_state.lock().unwrap();
        st.progress_text = format!("Repair complete — {repaired} file(s) restored.");
        st.target_version = installed_version;
    }

    Ok(())
}

// ── Background: install from a local archive ────────────────────────────

fn run_local_install(app_state: &Arc<Mutex<AppState>>, ctx: &egui::Context) -> anyhow::Result<()> {
    let (archive, base_dir, no_shortcuts, kill) = {
        let st = app_state.lock().unwrap();
        (
            st.archive.clone().unwrap(),
            st.base_dir.clone(),
            st.no_shortcuts,
            st.kill,
        )
    };

    // Detect version from archive filename
    let version = crate::detect_version_from_archive(&archive)
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());

    {
        let mut st = app_state.lock().unwrap();
        st.target_version = version.clone();
    }

    if kill {
        state::kill_running_instances();
    }

    // Extract to versioned directory
    {
        let mut st = app_state.lock().unwrap();
        st.progress_text = "Extracting files\u{2026}".into();
    }
    ctx.request_repaint();

    let ver_dir = state::version_dir(&base_dir, &version);
    std::fs::create_dir_all(&ver_dir)?;

    let extracted: HashMap<String, String> =
        extract::extract_7z_with_progress(&archive, &ver_dir, |count, total| {
            if let Ok(mut st) = app_state.lock() {
                st.files_extracted = count;
                st.files_total = total;
                st.progress_text = "Hashing files\u{2026}".into();
            }
            ctx.request_repaint();
        })?;

    // Write manifest
    {
        let mut st = app_state.lock().unwrap();
        st.progress_text = "Writing manifest\u{2026}".into();
    }
    ctx.request_repaint();

    let manifest_path = ver_dir.join("manifest.json");
    manifest::write_manifest(&ver_dir, &extracted, &manifest_path)?;

    // Update install state
    {
        let mut st = app_state.lock().unwrap();
        st.progress_text = "Cleaning up\u{2026}".into();
    }
    ctx.request_repaint();

    let mut ist = state::read_state(&base_dir)?;
    let old_versions: Vec<_> = ist.old_versions().iter().map(|v| (*v).clone()).collect();
    for old in &old_versions {
        let _ = std::fs::remove_dir_all(&old.path);
        ist.remove_version(&old.version);
    }

    let nightly = {
        let st = app_state.lock().unwrap();
        st.nightly
    };

    ist.add_version(&version, &ver_dir);
    ist.set_channel(nightly);
    state::write_state(&base_dir, &ist)?;
    state::self_copy(&base_dir, nightly)?;

    // Shortcuts
    if !no_shortcuts {
        let mut st = app_state.lock().unwrap();
        st.progress_text = "Creating shortcuts\u{2026}".into();
        drop(st);
        ctx.request_repaint();

        let installer_exe = state::installer_path(&base_dir, nightly);
        shortcuts::create_shortcuts_for_launcher(&installer_exe)?;
    }

    // Update state for Complete page
    {
        let mut st = app_state.lock().unwrap();
        st.install_state = ist;
        st.installed_version = version;
    }

    Ok(())
}
