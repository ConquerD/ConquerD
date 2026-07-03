use snm_core::{Defaults, Instance, PrivilegeMode, ResolvedInstance};

#[derive(Debug, Clone)]
pub struct InstanceLayout {
    pub instance_id: String,
    pub unit_name: String,
    pub data_dir: String,
    pub binary_dir: String,
    pub versioned_binary: String,
    pub current_binary_link: String,
    pub manifest_path: String,
    pub service_user: String,
}

impl InstanceLayout {
    pub fn from_resolved(resolved: &ResolvedInstance<'_>) -> Self {
        let defaults = resolved.defaults;
        let instance = resolved.instance;
        Self {
            instance_id: instance.id.clone(),
            unit_name: format!("conquerd-supernode@{}.service", instance.id),
            data_dir: format!("{}/{}", defaults.data_root, instance.id),
            binary_dir: format!("{}/bin", defaults.install_root),
            versioned_binary: format!(
                "{}/bin/conquerd-supernode-{}",
                defaults.install_root, defaults.version
            ),
            current_binary_link: format!("{}/bin/current", defaults.install_root),
            manifest_path: format!("{}/{}/supernode.toml", defaults.data_root, instance.id),
            service_user: defaults.user.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct NetworkEnv {
    pub relay_port: u16,
    pub ws_port: u16,
    pub web_port: Option<u16>,
    pub public_host: String,
    pub access_mode: String,
}

impl NetworkEnv {
    pub fn from_resolved(resolved: &ResolvedInstance<'_>) -> Self {
        Self {
            relay_port: resolved.relay_port,
            ws_port: resolved.ws_port,
            web_port: resolved.web_port,
            public_host: resolved.instance.public_host.clone(),
            access_mode: resolved.defaults.access_mode.clone(),
        }
    }
}

pub fn privilege_prefix(mode: PrivilegeMode) -> &'static str {
    match mode {
        PrivilegeMode::Sudo => "sudo ",
        PrivilegeMode::Root => "",
        PrivilegeMode::RootlessSystemd => "",
    }
}

pub fn systemctl_command(defaults: &Defaults, args: &str) -> String {
    match defaults.privilege {
        PrivilegeMode::RootlessSystemd => format!("systemctl --user {args}"),
        PrivilegeMode::Sudo => format!("sudo systemctl {args}"),
        PrivilegeMode::Root => format!("systemctl {args}"),
    }
}

pub fn journalctl_command(defaults: &Defaults, unit: &str, follow: bool, lines: u32) -> String {
    let tail = if follow { " -f" } else { "" };
    let cmd = format!("journalctl -u {unit} -n {lines} --no-pager{tail}");
    match defaults.privilege {
        PrivilegeMode::RootlessSystemd => cmd,
        PrivilegeMode::Sudo => format!("sudo {cmd}"),
        PrivilegeMode::Root => cmd,
    }
}

pub fn instance_label(host_name: &str, instance: &Instance) -> String {
    format!("{}/{}", host_name, instance.id)
}
