use std::path::PathBuf;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use ratatui::layout::Rect;
use snm_core::{
    apply_room_policy_to_features, default_instance_features, default_relay_port, default_ws_port,
    features_from_csv, resolve_supernode_config, room_policy_from_features, FeatureSpec,
    FirewallMode, Instance, Inventory, PrivilegeMode, KNOWN_FEATURES,
};
use snm_supernode::{HostProbe, InstanceStatus, LifecycleAction};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Panel {
    #[default]
    Fleet,
    Logs,
    Help,
    NodeForm,
    NodeConfig,
    Settings,
    ConfirmRemove,
    ConfirmUninstall,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LogsView {
    #[default]
    Journal,
    Invite,
    Operation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClickTarget {
    SelectRow(usize),
    Button(ButtonId),
    FormField(FormField),
    ConfigField(usize),
    SettingsField(SettingsField),
    UninstallConfirmField,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonId {
    AddNode,
    EditNode,
    Refresh,
    Connect,
    Ping,
    Start,
    Stop,
    Restart,
    Install,
    Uninstall,
    Logs,
    Invite,
    Remove,
    Settings,
    NodeConfig,
    SaveNode,
    CancelNode,
    SaveNodeConfig,
    CancelNodeConfig,
    SaveSettings,
    CancelSettings,
    ConfigPush,
    ClusterSync,
    BuildDeploy,
    UninstallPurgeToggle,
    ConfirmUninstall,
    CancelUninstall,
    ConfirmRemove,
    CancelRemove,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormField {
    HostName = 0,
    Ssh = 1,
    InstanceId = 2,
    PublicHost = 3,
    RelayPort = 4,
    WsPort = 5,
}

pub const NODE_FORM_FIELDS: [FormField; 6] = [
    FormField::HostName,
    FormField::Ssh,
    FormField::InstanceId,
    FormField::PublicHost,
    FormField::RelayPort,
    FormField::WsPort,
];

pub const CONFIG_TEXT_FIELDS: usize = 4;
pub const CONFIG_ROOM_POLICY_FIELDS: usize = 2;
pub const CONFIG_FEATURE_TOGGLES_START: usize = CONFIG_TEXT_FIELDS + CONFIG_ROOM_POLICY_FIELDS;

pub fn config_field_count() -> usize {
    CONFIG_FEATURE_TOGGLES_START + KNOWN_FEATURES.len()
}

pub fn config_field_label(index: usize) -> &'static str {
    match index {
        0 => "Listen bind (optional)",
        1 => "Access mode (optional)",
        2 => "Identity file (optional)",
        3 => "Extra features (comma-separated)",
        4 => "Allow public room creation",
        5 => "Allow private room creation",
        i if i < config_field_count() => KNOWN_FEATURES[i - CONFIG_FEATURE_TOGGLES_START],
        _ => "—",
    }
}

pub fn config_field_is_toggle(index: usize) -> bool {
    index >= CONFIG_TEXT_FIELDS && index < config_field_count()
}

pub fn config_field_is_room_policy(index: usize) -> bool {
    index >= CONFIG_TEXT_FIELDS && index < CONFIG_FEATURE_TOGGLES_START
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsField {
    Version = 0,
    AccessMode = 1,
    User = 2,
    InstallRoot = 3,
    DataRoot = 4,
    Privilege = 5,
    Firewall = 6,
    ReleaseRepo = 7,
    BinaryPath = 8,
    ListenBind = 9,
    IdentityFile = 10,
    BuildSource = 11,
    BuildTarget = 12,
    BuildTool = 13,
}

pub const SETTINGS_FIELDS: [SettingsField; 14] = [
    SettingsField::Version,
    SettingsField::AccessMode,
    SettingsField::User,
    SettingsField::InstallRoot,
    SettingsField::DataRoot,
    SettingsField::Privilege,
    SettingsField::Firewall,
    SettingsField::ReleaseRepo,
    SettingsField::BinaryPath,
    SettingsField::ListenBind,
    SettingsField::IdentityFile,
    SettingsField::BuildSource,
    SettingsField::BuildTarget,
    SettingsField::BuildTool,
];

impl ButtonId {
    pub fn label(self) -> &'static str {
        match self {
            Self::AddNode => "+ Add Node",
            Self::EditNode => "Edit",
            Self::Refresh => "Refresh",
            Self::Connect => "Connect",
            Self::Ping => "Ping",
            Self::Start => "Start",
            Self::Stop => "Stop",
            Self::Restart => "Restart",
            Self::Install => "Install",
            Self::Uninstall => "Uninstall",
            Self::Logs => "Logs",
            Self::Invite => "Invite",
            Self::Remove => "Remove",
            Self::Settings => "Settings",
            Self::NodeConfig => "Configs",
            Self::SaveNode => "Save",
            Self::CancelNode => "Cancel",
            Self::SaveNodeConfig => "Save",
            Self::CancelNodeConfig => "Cancel",
            Self::SaveSettings => "Save",
            Self::CancelSettings => "Cancel",
            Self::ConfigPush => "Push",
            Self::ClusterSync => "Cluster Sync",
            Self::BuildDeploy => "Build & Deploy",
            Self::UninstallPurgeToggle => "Purge",
            Self::ConfirmUninstall => "Confirm Uninstall",
            Self::CancelUninstall => "Cancel",
            Self::ConfirmRemove => "Confirm Remove",
            Self::CancelRemove => "Cancel",
        }
    }

    pub fn shortcut(self) -> Option<&'static str> {
        match self {
            Self::AddNode => Some("n"),
            Self::EditNode => Some("e"),
            Self::NodeConfig => Some("C"),
            Self::Refresh => Some("r"),
            Self::Connect => Some("c"),
            Self::Ping => Some("p"),
            Self::Start => Some("s"),
            Self::Stop => Some("x"),
            Self::Restart => Some("R"),
            Self::Install => Some("i"),
            Self::Uninstall => Some("u"),
            Self::Logs => Some("l"),
            Self::Invite => Some("v"),
            Self::Remove => Some("d"),
            Self::Settings => Some("G"),
            Self::ConfigPush => Some("P"),
            Self::ClusterSync => Some("Z"),
            Self::BuildDeploy => Some("B"),
            _ => None,
        }
    }

    pub fn blocked_while_busy(self) -> bool {
        matches!(
            self,
            Self::EditNode
                | Self::Refresh
                | Self::Connect
                | Self::Ping
                | Self::Start
                | Self::Stop
                | Self::Restart
                | Self::Install
                | Self::Uninstall
                | Self::Logs
                | Self::Invite
                | Self::Remove
                | Self::ConfigPush
                | Self::ClusterSync
                | Self::BuildDeploy
        )
    }
}

impl Panel {
    pub fn title(self) -> &'static str {
        match self {
            Self::Fleet => "Fleet",
            Self::Logs => "Logs",
            Self::Help => "Help",
            Self::NodeForm => "Node",
            Self::NodeConfig => "Configs",
            Self::Settings => "Settings",
            Self::ConfirmRemove => "Remove",
            Self::ConfirmUninstall => "Uninstall",
        }
    }
}

impl FormField {
    pub fn label(self) -> &'static str {
        match self {
            Self::HostName => "Host name",
            Self::Ssh => "SSH target",
            Self::InstanceId => "Instance ID",
            Self::PublicHost => "Public host",
            Self::RelayPort => "Relay port (optional)",
            Self::WsPort => "WS port (optional)",
        }
    }

    pub fn next(self) -> Self {
        let idx = self as usize;
        NODE_FORM_FIELDS[(idx + 1) % NODE_FORM_FIELDS.len()]
    }

    pub fn prev(self) -> Self {
        let idx = self as usize;
        NODE_FORM_FIELDS[(idx + NODE_FORM_FIELDS.len() - 1) % NODE_FORM_FIELDS.len()]
    }

    pub fn from_index(index: usize) -> Self {
        NODE_FORM_FIELDS[index.min(NODE_FORM_FIELDS.len() - 1)]
    }
}

impl SettingsField {
    pub fn label(self) -> &'static str {
        match self {
            Self::Version => "Version",
            Self::AccessMode => "Access mode",
            Self::User => "Service user",
            Self::InstallRoot => "Install root",
            Self::DataRoot => "Data root",
            Self::Privilege => "Privilege (sudo/root/rootless-systemd)",
            Self::Firewall => "Firewall (off/ufw/report)",
            Self::ReleaseRepo => "GitHub release repo",
            Self::BinaryPath => "Local binary path (version=local)",
            Self::ListenBind => "Listen bind",
            Self::IdentityFile => "Identity file",
            Self::BuildSource => "Build source dir (build-deploy)",
            Self::BuildTarget => "Build target triple (e.g. x86_64-unknown-linux-musl)",
            Self::BuildTool => "Build tool: cargo / zigbuild / cross",
        }
    }

    pub fn next(self) -> Self {
        let idx = self as usize;
        SETTINGS_FIELDS[(idx + 1) % SETTINGS_FIELDS.len()]
    }

    pub fn prev(self) -> Self {
        let idx = self as usize;
        SETTINGS_FIELDS[(idx + SETTINGS_FIELDS.len() - 1) % SETTINGS_FIELDS.len()]
    }

    pub fn from_index(index: usize) -> Self {
        SETTINGS_FIELDS[index.min(SETTINGS_FIELDS.len() - 1)]
    }
}

#[derive(Debug, Clone)]
pub struct HitZone {
    pub rect: Rect,
    pub target: ClickTarget,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeFormMode {
    Add,
    Edit {
        original_host: String,
        original_instance_id: String,
    },
}

#[derive(Debug, Clone)]
pub struct NodeForm {
    pub mode: NodeFormMode,
    pub host_name: String,
    pub ssh: String,
    pub instance_id: String,
    pub public_host: String,
    pub relay_port: String,
    pub ws_port: String,
    pub focused_field: usize,
    pub scroll: u16,
    pub error: Option<String>,
}

impl NodeForm {
    pub fn new_add() -> Self {
        Self {
            mode: NodeFormMode::Add,
            host_name: String::new(),
            ssh: String::new(),
            instance_id: "a".into(),
            public_host: String::new(),
            relay_port: String::new(),
            ws_port: String::new(),
            focused_field: 0,
            scroll: 0,
            error: None,
        }
    }

    pub fn is_edit(&self) -> bool {
        matches!(self.mode, NodeFormMode::Edit { .. })
    }

    pub fn title(&self) -> &'static str {
        if self.is_edit() {
            " Edit Node "
        } else {
            " Add Node "
        }
    }

    pub fn field_mut(&mut self, field: FormField) -> &mut String {
        match field {
            FormField::HostName => &mut self.host_name,
            FormField::Ssh => &mut self.ssh,
            FormField::InstanceId => &mut self.instance_id,
            FormField::PublicHost => &mut self.public_host,
            FormField::RelayPort => &mut self.relay_port,
            FormField::WsPort => &mut self.ws_port,
        }
    }

    pub fn focus(&mut self, field: FormField) {
        self.focused_field = field as usize;
        self.error = None;
    }

    pub fn insert_char(&mut self, ch: char) {
        self.field_mut(FormField::from_index(self.focused_field))
            .push(ch);
    }

    pub fn backspace(&mut self) {
        let field = FormField::from_index(self.focused_field);
        self.field_mut(field).pop();
    }
}

#[derive(Debug, Clone)]
pub struct SettingsForm {
    pub version: String,
    pub access_mode: String,
    pub user: String,
    pub install_root: String,
    pub data_root: String,
    pub privilege: String,
    pub firewall: String,
    pub release_repo: String,
    pub binary_path: String,
    pub listen_bind: String,
    pub identity_file: String,
    pub build_source: String,
    pub build_target: String,
    pub build_tool: String,
    pub focused_field: usize,
    pub scroll: u16,
    pub error: Option<String>,
}

impl SettingsForm {
    pub fn from_inventory(inv: &Inventory) -> Self {
        let d = &inv.defaults;
        Self {
            version: d.version.clone(),
            access_mode: d.access_mode.clone(),
            user: d.user.clone(),
            install_root: d.install_root.clone(),
            data_root: d.data_root.clone(),
            privilege: privilege_label(d.privilege).into(),
            firewall: firewall_label(d.firewall).into(),
            release_repo: d.release_repo.clone(),
            binary_path: d
                .binary_path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            listen_bind: d.supernode.listen_bind.clone(),
            identity_file: d.supernode.identity_file.clone(),
            build_source: d
                .build_source
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            build_target: d.build_target.clone().unwrap_or_default(),
            build_tool: d.build_tool.clone().unwrap_or_default(),
            focused_field: 0,
            scroll: 0,
            error: None,
        }
    }

    pub fn field_mut(&mut self, field: SettingsField) -> &mut String {
        match field {
            SettingsField::Version => &mut self.version,
            SettingsField::AccessMode => &mut self.access_mode,
            SettingsField::User => &mut self.user,
            SettingsField::InstallRoot => &mut self.install_root,
            SettingsField::DataRoot => &mut self.data_root,
            SettingsField::Privilege => &mut self.privilege,
            SettingsField::Firewall => &mut self.firewall,
            SettingsField::ReleaseRepo => &mut self.release_repo,
            SettingsField::BinaryPath => &mut self.binary_path,
            SettingsField::ListenBind => &mut self.listen_bind,
            SettingsField::IdentityFile => &mut self.identity_file,
            SettingsField::BuildSource => &mut self.build_source,
            SettingsField::BuildTarget => &mut self.build_target,
            SettingsField::BuildTool => &mut self.build_tool,
        }
    }

    pub fn focus(&mut self, field: SettingsField) {
        self.focused_field = field as usize;
        self.error = None;
    }

    pub fn insert_char(&mut self, ch: char) {
        self.field_mut(SettingsField::from_index(self.focused_field))
            .push(ch);
    }

    pub fn backspace(&mut self) {
        let field = SettingsField::from_index(self.focused_field);
        self.field_mut(field).pop();
    }
}

#[derive(Debug, Clone)]
pub struct NodeConfigForm {
    pub host_name: String,
    pub instance_id: String,
    pub label: String,
    pub listen_bind: String,
    pub access_mode: String,
    pub identity_file: String,
    pub extra_features: String,
    pub allow_public_rooms: bool,
    pub allow_private_rooms: bool,
    pub feature_toggles: Vec<bool>,
    pub effective_hint: String,
    pub focused_field: usize,
    pub scroll: u16,
    pub error: Option<String>,
}

impl NodeConfigForm {
    pub fn from_instance(
        inv: &Inventory,
        host_name: &str,
        instance_id: &str,
        label: &str,
        relay_port: u16,
        ws_port: u16,
    ) -> Option<Self> {
        let host = inv.host.iter().find(|h| h.name == host_name)?;
        let instance = host.instances.iter().find(|i| i.id == instance_id)?;
        let mut toggles = vec![false; KNOWN_FEATURES.len()];
        let mut extra_ids = Vec::new();
        for feature in &instance.features {
            if let Some(idx) = KNOWN_FEATURES.iter().position(|id| *id == feature.id) {
                toggles[idx] = feature.enabled;
            } else {
                extra_ids.push(feature.id.clone());
            }
        }
        if instance.features.is_empty() {
            for (idx, id) in KNOWN_FEATURES.iter().enumerate() {
                toggles[idx] = default_instance_features().iter().any(|f| f.id == *id);
            }
        }
        let (mut allow_public, mut allow_private) = room_policy_from_features(&instance.features);
        if let Some(v) = instance.allow_public_rooms {
            allow_public = v;
        } else if let Some(v) = inv.defaults.supernode.allow_public_rooms {
            allow_public = v;
        }
        if let Some(v) = instance.allow_private_rooms {
            allow_private = v;
        } else if let Some(v) = inv.defaults.supernode.allow_private_rooms {
            allow_private = v;
        }
        let resolved = resolve_supernode_config(&inv.defaults, instance, relay_port, ws_port);
        let effective_hint = format!(
            "Effective: listen {}:{}  ws {}:{}  access {}",
            resolved.listen_bind,
            resolved.relay_port,
            resolved.listen_bind,
            resolved.ws_port,
            resolved.access_mode,
        );
        Some(Self {
            host_name: host_name.into(),
            instance_id: instance_id.into(),
            label: label.into(),
            listen_bind: instance.listen_bind.clone().unwrap_or_default(),
            access_mode: instance.access_mode.clone().unwrap_or_default(),
            identity_file: instance.identity_file.clone().unwrap_or_default(),
            extra_features: extra_ids.join(", "),
            allow_public_rooms: allow_public,
            allow_private_rooms: allow_private,
            feature_toggles: toggles,
            effective_hint,
            focused_field: 0,
            scroll: 0,
            error: None,
        })
    }

    pub fn text_field_mut(&mut self, index: usize) -> Option<&mut String> {
        match index {
            0 => Some(&mut self.listen_bind),
            1 => Some(&mut self.access_mode),
            2 => Some(&mut self.identity_file),
            3 => Some(&mut self.extra_features),
            _ => None,
        }
    }

    pub fn focus(&mut self, index: usize) {
        self.focused_field = index.min(config_field_count().saturating_sub(1));
        self.error = None;
    }

    pub fn next_field(&mut self) {
        let n = config_field_count();
        if n == 0 {
            return;
        }
        self.focused_field = (self.focused_field + 1) % n;
        self.error = None;
    }

    pub fn prev_field(&mut self) {
        let n = config_field_count();
        if n == 0 {
            return;
        }
        self.focused_field = (self.focused_field + n - 1) % n;
        self.error = None;
    }

    pub fn insert_char(&mut self, ch: char) {
        if let Some(field) = self.text_field_mut(self.focused_field) {
            field.push(ch);
        }
    }

    pub fn backspace(&mut self) {
        if let Some(field) = self.text_field_mut(self.focused_field) {
            field.pop();
        }
    }

    pub fn toggle_focused_feature(&mut self) {
        if !config_field_is_toggle(self.focused_field) {
            return;
        }
        if config_field_is_room_policy(self.focused_field) {
            match self.focused_field {
                4 => self.allow_public_rooms = !self.allow_public_rooms,
                5 => self.allow_private_rooms = !self.allow_private_rooms,
                _ => {}
            }
            return;
        }
        let idx = self.focused_field - CONFIG_FEATURE_TOGGLES_START;
        if let Some(slot) = self.feature_toggles.get_mut(idx) {
            *slot = !*slot;
        }
    }

    pub fn build_features(&self) -> Vec<FeatureSpec> {
        let mut features = Vec::new();
        for (idx, enabled) in self.feature_toggles.iter().enumerate() {
            if *enabled {
                if let Some(id) = KNOWN_FEATURES.get(idx) {
                    features.push(FeatureSpec::enabled(*id));
                }
            }
        }
        for id in features_from_csv(&self.extra_features) {
            if KNOWN_FEATURES.iter().any(|known| known == &id.id) {
                continue;
            }
            features.push(id);
        }
        apply_room_policy_to_features(
            &mut features,
            self.allow_public_rooms,
            self.allow_private_rooms,
        );
        features
    }

    pub fn room_policy_for_save(&self) -> (Option<bool>, Option<bool>) {
        (
            (!self.allow_public_rooms).then_some(false),
            (!self.allow_private_rooms).then_some(false),
        )
    }
}

#[derive(Debug, Clone)]
pub enum RowStatus {
    Unknown,
    Loading,
    Active(InstanceStatus),
    Inactive(InstanceStatus),
    Error(String),
}

impl RowStatus {
    pub fn state_label(&self) -> &str {
        match self {
            Self::Unknown => "-",
            Self::Loading => "...",
            Self::Active(s) | Self::Inactive(s) => s.systemd_state.as_str(),
            Self::Error(_) => "error",
        }
    }

    pub fn version_label(&self) -> String {
        match self {
            Self::Active(s) | Self::Inactive(s) => s.version_display(),
            _ => "-".into(),
        }
    }

    pub fn version_detail_label(&self) -> String {
        match self {
            Self::Active(s) | Self::Inactive(s) => {
                let mut label = s.version_display();
                if let Some(ref build_id) = s.build_id {
                    label.push_str(&format!(" build={build_id}"));
                }
                label
            }
            _ => "-".into(),
        }
    }

    pub fn glyph(&self) -> &'static str {
        match self {
            Self::Active(_) => "*",
            Self::Inactive(_) => "o",
            Self::Loading => "...",
            Self::Error(_) => "x",
            Self::Unknown => ".",
        }
    }
}

#[derive(Debug, Clone)]
pub struct InstanceRow {
    pub host_name: String,
    pub instance_id: String,
    pub ssh: String,
    pub public_host: String,
    pub relay_port: u16,
    pub ws_port: u16,
    pub status: RowStatus,
    pub platform: Option<String>,
    /// Cluster this instance belongs to, if any.
    pub cluster_id: Option<String>,
}

impl InstanceRow {
    pub fn label(&self) -> String {
        format!("{}/{}", self.host_name, self.instance_id)
    }
}

#[derive(Debug, Clone)]
pub enum WorkerMsg {
    Status {
        row: usize,
        result: Result<InstanceStatus, String>,
    },
    Ping {
        row: usize,
        result: Result<HostProbe, String>,
    },
    Logs {
        row: usize,
        content: Result<String, String>,
    },
    Invite {
        row: usize,
        content: Result<String, String>,
    },
    OperationLog {
        title: String,
        content: String,
        status: String,
    },
    Notice {
        message: String,
    },
}

#[derive(Debug, Clone)]
pub enum WorkerCmd {
    RefreshAll,
    Ping(usize),
    Lifecycle(usize, LifecycleAction),
    Install(usize),
    ConfigPush(usize),
    FetchLogs(usize),
    FetchInvite(usize),
    Uninstall { row: usize, purge: bool },
    ClusterSync,
    BuildDeploy(usize),
}

pub struct App {
    pub inventory_path: PathBuf,
    pub inventory: Inventory,
    pub rows: Vec<InstanceRow>,
    pub selected: usize,
    pub panel: Panel,
    pub logs_text: String,
    pub logs_view: LogsView,
    pub logs_scroll: u16,
    pub help_scroll: u16,
    pub anim_frame: u8,
    pub status_line: String,
    pub busy: bool,
    pub refresh_pending: usize,
    pub last_refresh: Option<Instant>,
    pub auto_refresh_secs: u64,
    pub hit_zones: Vec<HitZone>,
    pub hover_target: Option<ClickTarget>,
    pub node_form: NodeForm,
    pub node_config_form: NodeConfigForm,
    pub settings_form: SettingsForm,
    pub remove_confirm: RemoveConfirmState,
    pub uninstall_confirm: UninstallConfirmState,
    /// `(ssh target, label)` — handled on the main thread with terminal suspend.
    pub connect_pending: Option<(String, String)>,
    /// Plain-text view for mouse selection (logs/invite panel).
    pub logs_open_pending: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct RemoveConfirmState {
    pub row: usize,
    pub label: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct UninstallConfirmState {
    pub row: usize,
    pub label: String,
    pub data_dir: String,
    pub instance_id: String,
    pub purge: bool,
    pub purge_confirm: String,
    pub purge_field_focused: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfirmRemoveOutcome {
    Ready,
    BlockedBusy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfirmUninstallOutcome {
    Proceed {
        row: usize,
        purge: bool,
        label: String,
    },
    BlockedBusy,
    BlockedValidation {
        hint: String,
    },
}

impl App {
    pub fn new(inventory_path: PathBuf, inventory: Inventory) -> Self {
        let rows = build_rows(&inventory);
        let settings_form = SettingsForm::from_inventory(&inventory);
        Self {
            inventory_path,
            inventory,
            rows,
            selected: 0,
            panel: Panel::Fleet,
            logs_text: String::new(),
            logs_view: LogsView::default(),
            logs_scroll: 0,
            help_scroll: 0,
            anim_frame: 0,
            status_line: "Press ? for help  ·  n to add a node".into(),
            busy: false,
            refresh_pending: 0,
            last_refresh: None,
            auto_refresh_secs: 30,
            hit_zones: Vec::new(),
            hover_target: None,
            node_form: NodeForm::new_add(),
            node_config_form: NodeConfigForm {
                host_name: String::new(),
                instance_id: String::new(),
                label: String::new(),
                listen_bind: String::new(),
                access_mode: String::new(),
                identity_file: String::new(),
                extra_features: String::new(),
                allow_public_rooms: snm_core::DEFAULT_ALLOW_PUBLIC_ROOMS,
                allow_private_rooms: snm_core::DEFAULT_ALLOW_PRIVATE_ROOMS,
                feature_toggles: vec![false; KNOWN_FEATURES.len()],
                effective_hint: String::new(),
                focused_field: 0,
                scroll: 0,
                error: None,
            },
            settings_form,
            remove_confirm: RemoveConfirmState::default(),
            uninstall_confirm: UninstallConfirmState::default(),
            connect_pending: None,
            logs_open_pending: None,
        }
    }

    pub fn queue_connect(&mut self) {
        let Some(row) = self.rows.get(self.selected) else {
            self.status_line = "no host selected".into();
            return;
        };
        self.connect_pending = Some((row.ssh.clone(), row.label()));
        self.status_line = format!("connecting to {}…", row.label());
    }

    pub fn selected_row(&self) -> Option<&InstanceRow> {
        self.rows.get(self.selected)
    }

    pub fn open_add_node(&mut self) {
        let prefill = self.selected_row().map(|row| {
            (
                row.host_name.clone(),
                row.ssh.clone(),
                row.public_host.clone(),
            )
        });
        self.node_form = NodeForm::new_add();
        if let Some((host_name, ssh, public_host)) = prefill {
            self.node_form.host_name = host_name;
            self.node_form.ssh = ssh;
            self.node_form.public_host = public_host;
        }
        self.node_form.focus(FormField::HostName);
        self.panel = Panel::NodeForm;
        self.status_line = "add a node — click fields, type, then Save".into();
    }

    pub fn open_edit_node(&mut self) {
        let Some(row) = self.selected_row().cloned() else {
            self.status_line = "no node selected to edit".into();
            return;
        };
        self.node_form = NodeForm {
            mode: NodeFormMode::Edit {
                original_host: row.host_name.clone(),
                original_instance_id: row.instance_id.clone(),
            },
            host_name: row.host_name,
            ssh: row.ssh,
            instance_id: row.instance_id,
            public_host: row.public_host,
            relay_port: row.relay_port.to_string(),
            ws_port: row.ws_port.to_string(),
            focused_field: 0,
            scroll: 0,
            error: None,
        };
        self.node_form.focus(FormField::HostName);
        self.panel = Panel::NodeForm;
        self.status_line = format!(
            "edit {} — click fields, type, then Save",
            self.node_form.host_name
        );
    }

    pub fn open_node_config(&mut self) {
        let Some(row) = self.selected_row().cloned() else {
            self.status_line = "no node selected for configs".into();
            return;
        };
        let Some(form) = NodeConfigForm::from_instance(
            &self.inventory,
            &row.host_name,
            &row.instance_id,
            &row.label(),
            row.relay_port,
            row.ws_port,
        ) else {
            self.status_line = "could not load supernode config for selected node".into();
            return;
        };
        self.node_config_form = form;
        self.node_config_form.focus(0);
        self.panel = Panel::NodeConfig;
        self.status_line = format!(
            "supernode config for {} — Space toggles features, P to push after save",
            row.label()
        );
    }

    pub fn open_settings(&mut self) {
        self.settings_form = SettingsForm::from_inventory(&self.inventory);
        self.settings_form.focus(SettingsField::Version);
        self.panel = Panel::Settings;
        self.status_line = "fleet defaults — edit [defaults] and supernode settings".into();
    }

    pub fn open_confirm_remove(&mut self) {
        let Some(row) = self.selected_row().cloned() else {
            self.status_line = "no node selected to remove".into();
            return;
        };
        self.remove_confirm = RemoveConfirmState {
            row: self.selected,
            label: row.label(),
            error: None,
        };
        self.panel = Panel::ConfirmRemove;
        self.status_line = format!(
            "confirm remove {} from inventory",
            self.remove_confirm.label
        );
    }

    pub fn open_confirm_uninstall(&mut self) {
        let Some(row) = self.selected_row().cloned() else {
            self.status_line = "no node selected to uninstall".into();
            return;
        };
        let data_dir = format!("{}/{}", self.inventory.defaults.data_root, row.instance_id);
        self.uninstall_confirm = UninstallConfirmState {
            row: self.selected,
            label: row.label(),
            data_dir,
            instance_id: row.instance_id.clone(),
            purge: false,
            purge_confirm: String::new(),
            purge_field_focused: false,
            error: None,
        };
        self.panel = Panel::ConfirmUninstall;
        self.status_line = format!("confirm uninstall {}", self.uninstall_confirm.label);
    }

    pub fn toggle_uninstall_purge(&mut self) {
        self.uninstall_confirm.purge = !self.uninstall_confirm.purge;
        self.uninstall_confirm.purge_confirm.clear();
        self.uninstall_confirm.error = None;
        self.uninstall_confirm.purge_field_focused = self.uninstall_confirm.purge;
    }

    pub fn can_confirm_uninstall(&self) -> bool {
        if !self.uninstall_confirm.purge {
            return true;
        }
        let typed = self
            .uninstall_confirm
            .purge_confirm
            .trim()
            .to_ascii_lowercase();
        let id = self.uninstall_confirm.instance_id.to_ascii_lowercase();
        let label = self.uninstall_confirm.label.to_ascii_lowercase();
        typed == id || typed == label
    }

    pub fn uninstall_purge_hint(&self) -> String {
        if self.uninstall_confirm.purge_confirm.trim().is_empty() {
            format!(
                "type '{}' or '{}' above, then click Confirm Uninstall",
                self.uninstall_confirm.instance_id, self.uninstall_confirm.label
            )
        } else {
            format!(
                "confirmation must be '{}' or '{}'",
                self.uninstall_confirm.instance_id, self.uninstall_confirm.label
            )
        }
    }

    pub fn try_confirm_remove(&self) -> ConfirmRemoveOutcome {
        if self.busy {
            ConfirmRemoveOutcome::BlockedBusy
        } else {
            ConfirmRemoveOutcome::Ready
        }
    }

    pub fn try_confirm_uninstall(&mut self) -> ConfirmUninstallOutcome {
        if self.busy {
            return ConfirmUninstallOutcome::BlockedBusy;
        }
        if !self.can_confirm_uninstall() {
            let hint = self.uninstall_purge_hint();
            self.uninstall_confirm.error = Some(hint.clone());
            return ConfirmUninstallOutcome::BlockedValidation { hint };
        }
        self.uninstall_confirm.error = None;
        self.busy = true;
        ConfirmUninstallOutcome::Proceed {
            row: self.uninstall_confirm.row,
            purge: self.uninstall_confirm.purge,
            label: self.uninstall_confirm.label.clone(),
        }
    }

    pub fn commit_remove_from_inventory(&mut self) -> Result<String> {
        let row = self.remove_confirm.row;
        let Some(instance_row) = self.rows.get(row).cloned() else {
            bail!("node not found");
        };
        let label = instance_row.label();
        self.inventory
            .remove_instance(&instance_row.host_name, &instance_row.instance_id)
            .context("remove from inventory")?;
        self.inventory
            .save(&self.inventory_path)
            .context("save inventory")?;
        self.reload_inventory()?;
        Ok(format!(
            "removed {label} from inventory — uninstall on the host first if the supernode is still running there"
        ))
    }

    pub fn reload_inventory(&mut self) -> Result<()> {
        self.inventory = Inventory::load(&self.inventory_path)?;
        self.rows = build_rows(&self.inventory);
        if self.rows.is_empty() {
            self.selected = 0;
        } else {
            self.selected = self.selected.min(self.rows.len() - 1);
        }
        Ok(())
    }

    pub fn commit_node_form(&mut self) -> Result<()> {
        let form = self.node_form.clone();
        let host_name = form.host_name.trim();
        let ssh = form.ssh.trim();
        let instance_id = form.instance_id.trim();
        let public_host = form.public_host.trim();

        if host_name.is_empty()
            || ssh.is_empty()
            || instance_id.is_empty()
            || public_host.is_empty()
        {
            bail!("host name, SSH target, instance ID, and public host are required");
        }

        let (relay_port, ws_port) = match &form.mode {
            NodeFormMode::Add => {
                let index = self.inventory.instance_count();
                (
                    parse_optional_port(&form.relay_port, default_relay_port(index))?,
                    parse_optional_port(&form.ws_port, default_ws_port(index))?,
                )
            }
            NodeFormMode::Edit {
                original_host,
                original_instance_id,
            } => {
                let row = self
                    .rows
                    .iter()
                    .find(|r| {
                        r.host_name == *original_host && r.instance_id == *original_instance_id
                    })
                    .ok_or_else(|| anyhow::anyhow!("edited node no longer exists"))?;
                (
                    parse_optional_port(&form.relay_port, row.relay_port)?,
                    parse_optional_port(&form.ws_port, row.ws_port)?,
                )
            }
        };

        let preserved = match &form.mode {
            NodeFormMode::Edit {
                original_host,
                original_instance_id,
            } => self
                .inventory
                .host
                .iter()
                .find(|h| h.name == *original_host)
                .and_then(|h| h.instances.iter().find(|i| i.id == *original_instance_id)),
            NodeFormMode::Add => None,
        };

        let instance = Instance {
            id: instance_id.into(),
            public_host: public_host.into(),
            relay_port: Some(relay_port),
            ws_port: Some(ws_port),
            listen_bind: preserved.and_then(|i| i.listen_bind.clone()),
            access_mode: preserved.and_then(|i| i.access_mode.clone()),
            identity_file: preserved.and_then(|i| i.identity_file.clone()),
            allow_public_rooms: preserved.and_then(|i| i.allow_public_rooms),
            allow_private_rooms: preserved.and_then(|i| i.allow_private_rooms),
            cluster_port: preserved.and_then(|i| i.cluster_port),
            features: preserved
                .map(|i| i.features.clone())
                .unwrap_or_else(default_instance_features),
        };

        match form.mode {
            NodeFormMode::Add => {
                self.inventory
                    .push_instance(host_name, ssh, instance)
                    .context("add instance")?;
                self.inventory
                    .save(&self.inventory_path)
                    .context("save inventory")?;
                self.rows = build_rows(&self.inventory);
                self.selected = self
                    .rows
                    .iter()
                    .position(|r| r.host_name == host_name && r.instance_id == instance_id)
                    .unwrap_or(self.rows.len().saturating_sub(1));
                self.panel = Panel::Fleet;
                self.status_line = format!("added node {host_name}/{instance_id}");
            }
            NodeFormMode::Edit {
                original_host,
                original_instance_id,
            } => {
                self.inventory
                    .update_instance(
                        &original_host,
                        &original_instance_id,
                        host_name,
                        ssh,
                        instance,
                    )
                    .context("update instance")?;
                self.inventory
                    .save(&self.inventory_path)
                    .context("save inventory")?;
                self.rows = build_rows(&self.inventory);
                self.selected = self
                    .rows
                    .iter()
                    .position(|r| r.host_name == host_name && r.instance_id == instance_id)
                    .unwrap_or(self.rows.len().saturating_sub(1));
                self.panel = Panel::Fleet;
                self.status_line = format!("updated node {host_name}/{instance_id}");
            }
        }
        Ok(())
    }

    pub fn commit_node_config_form(&mut self) -> Result<()> {
        let form = self.node_config_form.clone();
        let features = form.build_features();
        if features.is_empty() {
            bail!("enable at least one feature");
        }

        let (ssh, existing) = {
            let host = self
                .inventory
                .host
                .iter()
                .find(|h| h.name == form.host_name)
                .ok_or_else(|| anyhow::anyhow!("host {} not found", form.host_name))?;
            let existing = host
                .instances
                .iter()
                .find(|i| i.id == form.instance_id)
                .ok_or_else(|| anyhow::anyhow!("instance {} not found", form.instance_id))?
                .clone();
            (host.ssh.clone(), existing)
        };

        let (allow_public, allow_private) = form.room_policy_for_save();
        let updated = Instance {
            listen_bind: optional_string(&form.listen_bind),
            access_mode: optional_string(&form.access_mode),
            identity_file: optional_string(&form.identity_file),
            allow_public_rooms: allow_public,
            allow_private_rooms: allow_private,
            features,
            ..existing
        };

        self.inventory
            .update_instance(
                &form.host_name,
                &form.instance_id,
                &form.host_name,
                &ssh,
                updated,
            )
            .context("update supernode config")?;
        self.inventory
            .save(&self.inventory_path)
            .context("save inventory")?;
        self.rows = build_rows(&self.inventory);
        self.panel = Panel::Fleet;
        self.status_line = format!("saved supernode config for {}", form.label);
        Ok(())
    }

    pub fn commit_settings_form(&mut self) -> Result<()> {
        let form = self.settings_form.clone();
        let version = form.version.trim();
        let access_mode = form.access_mode.trim();
        let user = form.user.trim();
        let install_root = form.install_root.trim();
        let data_root = form.data_root.trim();
        let release_repo = form.release_repo.trim();
        let listen_bind = form.listen_bind.trim();
        let identity_file = form.identity_file.trim();

        if version.is_empty()
            || access_mode.is_empty()
            || user.is_empty()
            || install_root.is_empty()
            || data_root.is_empty()
            || release_repo.is_empty()
            || listen_bind.is_empty()
            || identity_file.is_empty()
        {
            bail!("version, access mode, user, paths, release repo, listen bind, and identity file are required");
        }

        let privilege = parse_privilege(form.privilege.trim())?;
        let firewall = parse_firewall(form.firewall.trim())?;
        let binary_path = if form.binary_path.trim().is_empty() {
            None
        } else {
            Some(PathBuf::from(form.binary_path.trim()))
        };

        self.inventory.defaults.version = version.into();
        self.inventory.defaults.access_mode = access_mode.into();
        self.inventory.defaults.user = user.into();
        self.inventory.defaults.install_root = install_root.into();
        self.inventory.defaults.data_root = data_root.into();
        self.inventory.defaults.release_repo = release_repo.into();
        self.inventory.defaults.privilege = privilege;
        self.inventory.defaults.firewall = firewall;
        self.inventory.defaults.binary_path = binary_path;
        self.inventory.defaults.supernode.listen_bind = listen_bind.into();
        self.inventory.defaults.supernode.identity_file = identity_file.into();
        self.inventory.defaults.build_source = if form.build_source.trim().is_empty() {
            None
        } else {
            Some(PathBuf::from(form.build_source.trim()))
        };
        self.inventory.defaults.build_target = if form.build_target.trim().is_empty() {
            None
        } else {
            Some(form.build_target.trim().into())
        };
        self.inventory.defaults.build_tool = if form.build_tool.trim().is_empty() {
            None
        } else {
            Some(form.build_tool.trim().into())
        };

        self.inventory
            .save(&self.inventory_path)
            .context("save inventory")?;
        self.settings_form = SettingsForm::from_inventory(&self.inventory);
        self.panel = Panel::Fleet;
        self.status_line = "saved fleet defaults".into();
        Ok(())
    }

    pub fn apply_worker_msg(&mut self, msg: WorkerMsg) {
        match msg {
            WorkerMsg::Status { row, result } => {
                if let Some(r) = self.rows.get_mut(row) {
                    r.status = match result {
                        Ok(s) if s.active => RowStatus::Active(s),
                        Ok(s) => RowStatus::Inactive(s),
                        Err(e) => RowStatus::Error(e),
                    };
                }
                if self.refresh_pending > 0 {
                    self.refresh_pending -= 1;
                    if self.refresh_pending == 0 {
                        self.busy = false;
                        self.last_refresh = Some(Instant::now());
                        self.status_line = "fleet refreshed".into();
                    }
                } else if !self.busy {
                    self.last_refresh = Some(Instant::now());
                } else {
                    self.busy = false;
                    self.last_refresh = Some(Instant::now());
                }
            }
            WorkerMsg::Ping { row, result } => {
                if let Some(r) = self.rows.get_mut(row) {
                    match result {
                        Ok(p) => r.platform = Some(p.platform),
                        Err(e) => r.status = RowStatus::Error(e),
                    }
                }
                self.busy = false;
            }
            WorkerMsg::Logs { row, content } => {
                self.logs_text = match content {
                    Ok(text) => text,
                    Err(e) => format!("failed to fetch logs: {e}"),
                };
                let label = self
                    .rows
                    .get(row)
                    .map(InstanceRow::label)
                    .unwrap_or_else(|| format!("row {row}"));
                self.logs_view = LogsView::Journal;
                self.logs_scroll = 0;
                self.panel = Panel::Logs;
                self.status_line = format!(
                    "logs: {} — press y to copy, o to open for mouse selection",
                    label
                );
                self.busy = false;
            }
            WorkerMsg::Invite { row, content } => {
                self.logs_text = match content {
                    Ok(text) => text,
                    Err(e) => format!("failed to fetch invite: {e}"),
                };
                let label = self
                    .rows
                    .get(row)
                    .map(InstanceRow::label)
                    .unwrap_or_else(|| format!("row {row}"));
                self.logs_view = LogsView::Invite;
                self.logs_scroll = 0;
                self.panel = Panel::Logs;
                self.status_line = format!(
                    "invite: {} — press y to copy link, o to open for mouse selection",
                    label
                );
                self.busy = false;
            }
            WorkerMsg::OperationLog {
                title,
                content,
                status,
            } => {
                self.logs_text = if content.trim().is_empty() {
                    title
                } else {
                    format!("{title}\n\n{content}")
                };
                self.logs_view = LogsView::Operation;
                self.logs_scroll = 0;
                self.panel = Panel::Logs;
                self.status_line = status;
                self.busy = false;
            }
            WorkerMsg::Notice { message } => {
                self.status_line = message;
                self.busy = false;
            }
        }
    }

    pub fn move_selection(&mut self, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        let len = self.rows.len() as isize;
        let next = (self.selected as isize + delta).rem_euclid(len);
        self.selected = next as usize;
    }

    pub fn scroll_logs(&mut self, delta: i16) {
        let next = (self.logs_scroll as i32 + delta as i32).max(0);
        self.logs_scroll = next as u16;
    }

    pub fn scroll_logs_home(&mut self) {
        self.logs_scroll = 0;
    }

    pub fn scroll_logs_end(&mut self, visible_lines: u16) {
        let total = self.logs_line_count();
        self.logs_scroll = total.saturating_sub(visible_lines);
    }

    pub fn scroll_node_form(&mut self, delta: i16) {
        let max = NODE_FORM_FIELDS.len().saturating_sub(1) as u16;
        let next = (self.node_form.scroll as i32 + delta as i32).clamp(0, max as i32);
        self.node_form.scroll = next as u16;
    }

    pub fn scroll_node_config_form(&mut self, delta: i16) {
        let max = config_field_count().saturating_sub(1) as u16;
        let next = (self.node_config_form.scroll as i32 + delta as i32).clamp(0, max as i32);
        self.node_config_form.scroll = next as u16;
    }

    pub fn scroll_settings_form(&mut self, delta: i16) {
        let max = SETTINGS_FIELDS.len().saturating_sub(1) as u16;
        let next = (self.settings_form.scroll as i32 + delta as i32).clamp(0, max as i32);
        self.settings_form.scroll = next as u16;
    }

    pub fn scroll_help(&mut self, delta: i16) {
        let next = (self.help_scroll as i32 + delta as i32).max(0);
        self.help_scroll = next as u16;
    }

    pub fn logs_line_count(&self) -> u16 {
        self.logs_text.lines().count().max(1) as u16
    }

    pub fn busy_spinner(&self) -> &'static str {
        const FRAMES: [&str; 4] = ["⠋", "⠙", "⠹", "⠸"];
        FRAMES[(self.anim_frame as usize) % FRAMES.len()]
    }

    pub fn button_enabled(&self, id: ButtonId) -> bool {
        if id.blocked_while_busy() && self.busy {
            return false;
        }
        if self.panel != Panel::Fleet && matches!(id, ButtonId::AddNode | ButtonId::EditNode) {
            return false;
        }
        true
    }

    pub fn register_hit_zone(&mut self, rect: Rect, target: ClickTarget) {
        if rect.width > 0 && rect.height > 0 {
            self.hit_zones.push(HitZone { rect, target });
        }
    }

    pub fn hit_test(&self, col: u16, row: u16) -> Option<ClickTarget> {
        self.hit_zones
            .iter()
            .rev()
            .find(|zone| rect_contains(zone.rect, col, row))
            .map(|zone| zone.target)
    }
}

fn rect_contains(rect: Rect, col: u16, row: u16) -> bool {
    col >= rect.x
        && col < rect.x.saturating_add(rect.width)
        && row >= rect.y
        && row < rect.y.saturating_add(rect.height)
}

fn parse_optional_port(raw: &str, default: u16) -> Result<u16> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(default);
    }
    trimmed
        .parse()
        .with_context(|| format!("invalid port: {trimmed}"))
}

fn optional_string(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.into())
    }
}

fn privilege_label(mode: PrivilegeMode) -> &'static str {
    match mode {
        PrivilegeMode::Sudo => "sudo",
        PrivilegeMode::Root => "root",
        PrivilegeMode::RootlessSystemd => "rootless-systemd",
    }
}

fn firewall_label(mode: FirewallMode) -> &'static str {
    match mode {
        FirewallMode::Off => "off",
        FirewallMode::Ufw => "ufw",
        FirewallMode::Report => "report",
    }
}

fn parse_privilege(raw: &str) -> Result<PrivilegeMode> {
    match raw {
        "sudo" => Ok(PrivilegeMode::Sudo),
        "root" => Ok(PrivilegeMode::Root),
        "rootless-systemd" => Ok(PrivilegeMode::RootlessSystemd),
        other => bail!("invalid privilege '{other}' (use sudo, root, or rootless-systemd)"),
    }
}

fn parse_firewall(raw: &str) -> Result<FirewallMode> {
    match raw {
        "off" => Ok(FirewallMode::Off),
        "ufw" => Ok(FirewallMode::Ufw),
        "report" => Ok(FirewallMode::Report),
        other => bail!("invalid firewall '{other}' (use off, ufw, or report)"),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use snm_core::scaffold_inventory;
    use snm_supernode::InstanceStatus;

    use super::*;

    #[test]
    fn refresh_pending_clears_busy_when_complete() {
        let inv = scaffold_inventory();
        let mut app = App::new(PathBuf::from("inventory.toml"), inv);
        app.busy = true;
        app.refresh_pending = 2;

        app.apply_worker_msg(WorkerMsg::Status {
            row: 0,
            result: Ok(InstanceStatus {
                label: "edge-1/a".into(),
                active: true,
                systemd_state: "active".into(),
                binary_path: "/opt/conquerd/bin/current".into(),
                pinned_version: "local".into(),
                binary_sha256: None,
                binary_modified: None,
                build_id: None,
            }),
        });
        assert!(app.busy);
        assert_eq!(app.refresh_pending, 1);

        app.apply_worker_msg(WorkerMsg::Status {
            row: 0,
            result: Ok(InstanceStatus {
                label: "edge-1/a".into(),
                active: true,
                systemd_state: "active".into(),
                binary_path: "/opt/conquerd/bin/current".into(),
                pinned_version: "local".into(),
                binary_sha256: None,
                binary_modified: None,
                build_id: None,
            }),
        });
        assert!(!app.busy);
        assert_eq!(app.refresh_pending, 0);
    }

    #[test]
    fn build_rows_from_inventory() {
        let inv = scaffold_inventory();
        let rows = build_rows(&inv);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].host_name, "edge-1");
        assert_eq!(rows[0].relay_port, 3478);
    }

    #[test]
    fn uninstall_purge_confirm_accepts_label_or_instance_id() {
        let inv = scaffold_inventory();
        let mut app = App::new(PathBuf::from("inventory.toml"), inv);
        app.uninstall_confirm.label = "edge-1/a".into();
        app.uninstall_confirm.instance_id = "a".into();
        app.uninstall_confirm.purge = true;

        app.uninstall_confirm.purge_confirm = "edge-1/a".into();
        assert!(app.can_confirm_uninstall());

        app.uninstall_confirm.purge_confirm = "a".into();
        assert!(app.can_confirm_uninstall());

        app.uninstall_confirm.purge_confirm = "EDGE-1/A".into();
        assert!(app.can_confirm_uninstall());

        app.uninstall_confirm.purge_confirm = "wrong".into();
        assert!(!app.can_confirm_uninstall());
    }

    #[test]
    fn try_confirm_uninstall_proceeds_when_purge_typed() {
        let inv = scaffold_inventory();
        let mut app = App::new(PathBuf::from("inventory.toml"), inv);
        app.uninstall_confirm.row = 0;
        app.uninstall_confirm.label = "edge-1/a".into();
        app.uninstall_confirm.instance_id = "a".into();
        app.uninstall_confirm.purge = true;
        app.uninstall_confirm.purge_confirm = "edge-1/a".into();

        match app.try_confirm_uninstall() {
            ConfirmUninstallOutcome::Proceed { row, purge, label } => {
                assert_eq!(row, 0);
                assert!(purge);
                assert_eq!(label, "edge-1/a");
                assert!(app.busy);
            }
            other => panic!("expected Proceed, got {other:?}"),
        }
    }

    #[test]
    fn commit_remove_from_inventory_drops_node() {
        let inv = scaffold_inventory();
        let mut app = App::new(PathBuf::from("inventory.toml"), inv);
        app.remove_confirm.row = 0;
        app.remove_confirm.label = "edge-1/a".into();

        let path = std::env::temp_dir().join(format!("snm-remove-test-{}", std::process::id()));
        app.inventory.save(&path).unwrap();
        app.inventory_path = path.clone();

        let message = app.commit_remove_from_inventory().unwrap();
        assert!(message.contains("removed edge-1/a from inventory"));
        assert!(message.contains("uninstall on the host first"));
        assert_eq!(app.rows.len(), 0);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn commit_node_form_adds_inventory() {
        let inv = scaffold_inventory();
        let mut app = App::new(PathBuf::from("inventory.toml"), inv);
        app.node_form.host_name = "edge-2".into();
        app.node_form.ssh = "root@198.51.100.7".into();
        app.node_form.instance_id = "a".into();
        app.node_form.public_host = "edge2.example.net".into();

        app.commit_node_form().unwrap();
        assert_eq!(app.rows.len(), 2);
        assert_eq!(app.inventory.instance_count(), 2);
    }

    #[test]
    fn commit_node_form_edits_inventory() {
        let inv = scaffold_inventory();
        let path = std::env::temp_dir().join(format!("snm-edit-test-{}", std::process::id()));
        inv.save(&path).unwrap();
        let mut app = App::new(path.clone(), inv);
        app.node_form = NodeForm {
            mode: NodeFormMode::Edit {
                original_host: "edge-1".into(),
                original_instance_id: "a".into(),
            },
            host_name: "edge-1".into(),
            ssh: "conquerd@203.0.113.99".into(),
            instance_id: "a".into(),
            public_host: "edge1-new.example.net".into(),
            relay_port: "3479".into(),
            ws_port: "34936".into(),
            focused_field: 0,
            scroll: 0,
            error: None,
        };

        app.commit_node_form().unwrap();
        assert_eq!(app.rows[0].ssh, "conquerd@203.0.113.99");
        assert_eq!(app.rows[0].public_host, "edge1-new.example.net");
        assert_eq!(app.rows[0].relay_port, 3479);
        let _ = std::fs::remove_file(path);
    }
}

pub fn build_rows(inventory: &Inventory) -> Vec<InstanceRow> {
    let selector = snm_core::Selector::default();
    inventory
        .resolve_instances(&selector)
        .unwrap_or_default()
        .into_iter()
        .map(|resolved| {
            let cluster_id = inventory
                .clusters_for_instance(&resolved.host.name, &resolved.instance.id)
                .first()
                .map(|c| c.id.clone());
            InstanceRow {
                host_name: resolved.host.name.clone(),
                instance_id: resolved.instance.id.clone(),
                ssh: resolved.host.ssh.clone(),
                public_host: resolved.instance.public_host.clone(),
                relay_port: resolved.relay_port,
                ws_port: resolved.ws_port,
                status: RowStatus::Unknown,
                platform: resolved.host.arch.clone(),
                cluster_id,
            }
        })
        .collect()
}
