mod tui;

use std::io::{self, Write};
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use snm_core::{
    resolve_inventory_path, scaffold_inventory, scaffold_secrets_template, Inventory, Selector,
    DEFAULT_INVENTORY_PATH,
};
use snm_supernode::{
    build_local_binary, cluster_sync_report, connect_host, install_instance, invite_instance,
    lifecycle, logs_instance, ping_host, push_config_instance, resolve_install_binary,
    status_instance, uninstall_instance, ClusterCache, ClusterRoster, InstanceLayout,
    LifecycleAction, LocalBuildTool,
};
use snm_transport::{SshBackend, SshTransport};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(
    name = "supernode-manager",
    about = "Deploy and operate conquerd-supernode fleets over SSH",
    subcommand_required = false,
    arg_required_else_help = false
)]
struct Cli {
    #[arg(long, default_value = DEFAULT_INVENTORY_PATH, global = true)]
    inventory: PathBuf,

    /// SSH client backend: `embedded` (pure Rust) or `openssh` (system ssh/scp).
    /// Embedded auth: `~/.ssh` keys, then `SNM_SSH_PASSWORD`, then interactive keyboard-interactive.
    #[arg(long, global = true, default_value = "embedded")]
    ssh_backend: String,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Scaffold inventory.toml and secrets template
    Init {
        #[arg(long)]
        force: bool,
    },
    /// Verify SSH login (prompts for password when keys are unavailable)
    Connect {
        #[command(flatten)]
        selector: TargetArgs,
    },
    /// SSH reachability and arch/OS detection
    Ping {
        #[command(flatten)]
        selector: TargetArgs,
    },
    /// Provision binary, supernode.toml, and systemd unit
    Install {
        #[command(flatten)]
        selector: TargetArgs,
    },
    /// Re-render and push supernode.toml (+ systemd drop-in); restarts by default
    ConfigPush {
        #[command(flatten)]
        selector: TargetArgs,
        /// Upload config only; do not restart the service
        #[arg(long)]
        no_restart: bool,
    },
    Start {
        #[command(flatten)]
        selector: TargetArgs,
    },
    Stop {
        #[command(flatten)]
        selector: TargetArgs,
    },
    Restart {
        #[command(flatten)]
        selector: TargetArgs,
    },
    /// systemd active state and pinned version
    Status {
        #[command(flatten)]
        selector: TargetArgs,
    },
    /// journalctl tail for instance(s)
    Logs {
        #[command(flatten)]
        selector: TargetArgs,
        #[arg(short, long)]
        follow: bool,
        #[arg(short = 'n', long, default_value_t = 100)]
        lines: u32,
    },
    /// Fetch reusable invite link from the remote data directory
    Invite {
        #[command(flatten)]
        selector: TargetArgs,
    },
    /// Run a shell command on the remote host (first matching instance)
    Exec {
        #[command(flatten)]
        selector: TargetArgs,
        /// Remote command to run (passed to the host shell)
        #[arg(required = true)]
        command: String,
    },
    /// Drop instance from local inventory (does not touch the remote host)
    Remove {
        #[command(flatten)]
        selector: TargetArgs,
        /// Skip interactive confirmation
        #[arg(long)]
        yes: bool,
    },
    /// Stop/disable the remote supernode service (optional data purge)
    Uninstall {
        #[command(flatten)]
        selector: TargetArgs,
        /// Also delete the remote data directory (identity, peers, etc.)
        #[arg(long)]
        purge: bool,
        /// Skip interactive confirmation
        #[arg(long)]
        yes: bool,
    },
    /// Interactive fleet dashboard (default when no subcommand is given)
    Tui,
    /// Collect member identities and push cluster roster to all members
    ClusterSync {
        /// Sync a specific cluster by id; omit to sync all clusters
        #[arg(long)]
        cluster: Option<String>,
    },
    /// Build conquerd-supernode from local source and deploy to instance(s)
    BuildDeploy {
        #[command(flatten)]
        selector: TargetArgs,
        /// Path to the conquerd-supernode Cargo package directory
        #[arg(long)]
        source: PathBuf,
        /// Rust target triple for cross-compilation, e.g. x86_64-unknown-linux-musl.
        /// Omit only when building natively for the same OS/arch as the remote.
        #[arg(long)]
        target: Option<String>,
        /// Build front-end: cargo (default), zigbuild, or cross
        #[arg(long, default_value = "cargo")]
        build_tool: String,
    },
}

#[derive(Debug, Parser)]
struct TargetArgs {
    #[arg(long)]
    host: Option<String>,
    #[arg(long)]
    instance: Option<String>,
    #[arg(long)]
    all: bool,
}

#[tokio::main]
async fn main() {
    let launched_bare = std::env::args_os().len() <= 1;
    if let Err(err) = run().await {
        let _ = writeln!(io::stderr(), "error: {err:#}");
        if launched_bare {
            pause_before_exit();
        }
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();
    let inventory = resolve_inventory_path(cli.inventory);
    let ssh_backend = SshBackend::parse(&cli.ssh_backend).map_err(|e| anyhow::anyhow!(e))?;
    let command = cli.command.unwrap_or(Command::Tui);

    match command {
        Command::Init { force } => cmd_init(&inventory, force),
        Command::Connect { selector } => cmd_connect(&inventory, selector, ssh_backend).await,
        Command::Ping { selector } => cmd_ping(&inventory, selector, ssh_backend).await,
        Command::Install { selector } => cmd_install(&inventory, selector, ssh_backend).await,
        Command::ConfigPush {
            selector,
            no_restart,
        } => cmd_config_push(&inventory, selector, !no_restart, ssh_backend).await,
        Command::Start { selector } => {
            cmd_lifecycle(&inventory, selector, LifecycleAction::Start, ssh_backend).await
        }
        Command::Stop { selector } => {
            cmd_lifecycle(&inventory, selector, LifecycleAction::Stop, ssh_backend).await
        }
        Command::Restart { selector } => {
            cmd_lifecycle(&inventory, selector, LifecycleAction::Restart, ssh_backend).await
        }
        Command::Status { selector } => cmd_status(&inventory, selector, ssh_backend).await,
        Command::Logs {
            selector,
            follow,
            lines,
        } => cmd_logs(&inventory, selector, follow, lines, ssh_backend).await,
        Command::Invite { selector } => cmd_invite(&inventory, selector, ssh_backend).await,
        Command::Exec { selector, command } => {
            cmd_exec(&inventory, selector, &command, ssh_backend).await
        }
        Command::Remove { selector, yes } => cmd_remove(&inventory, selector, yes).await,
        Command::Uninstall {
            selector,
            purge,
            yes,
        } => cmd_uninstall(&inventory, selector, purge, yes, ssh_backend).await,
        Command::Tui => tui::run(inventory, ssh_backend).await,
        Command::ClusterSync { cluster } => {
            cmd_cluster_sync(&inventory, cluster.as_deref(), ssh_backend).await
        }
        Command::BuildDeploy {
            selector,
            source,
            target,
            build_tool,
        } => {
            cmd_build_deploy(
                &inventory,
                selector,
                &source,
                target.as_deref(),
                &build_tool,
                ssh_backend,
            )
            .await
        }
    }
}

fn ssh_transport(target: &str, backend: SshBackend) -> SshTransport {
    SshTransport::new(target, backend)
}

fn pause_before_exit() {
    let _ = writeln!(io::stderr(), "\nPress Enter to close...");
    let _ = io::stderr().flush();
    let mut line = String::new();
    let _ = io::stdin().read_line(&mut line);
}

fn cmd_init(path: &PathBuf, force: bool) -> Result<()> {
    if path.exists() && !force {
        bail!(
            "{} already exists; pass --force to overwrite",
            path.display()
        );
    }
    let inv = scaffold_inventory();
    inv.save(path).context("write inventory")?;
    let secrets_path = path
        .parent()
        .map(|p| p.join("secrets.toml"))
        .unwrap_or_else(|| PathBuf::from("secrets.toml"));
    if !secrets_path.exists() || force {
        std::fs::write(&secrets_path, scaffold_secrets_template())
            .with_context(|| format!("write {}", secrets_path.display()))?;
    }
    println!("wrote {}", path.display());
    println!("wrote {}", secrets_path.display());
    Ok(())
}

async fn cmd_connect(path: &PathBuf, args: TargetArgs, backend: SshBackend) -> Result<()> {
    let inv = Inventory::load(path)?;
    let selector = selector_from(args);
    for resolved in inv.resolve_instances(&selector)? {
        let transport = ssh_transport(&resolved.host.ssh, backend);
        connect_host(&transport).await?;
        println!(
            "{}  ssh={}  connected",
            resolved.host.name, resolved.host.ssh
        );
    }
    Ok(())
}

async fn cmd_ping(path: &PathBuf, args: TargetArgs, backend: SshBackend) -> Result<()> {
    let inv = Inventory::load(path)?;
    let selector = selector_from(args);
    for resolved in inv.resolve_instances(&selector)? {
        let transport = ssh_transport(&resolved.host.ssh, backend);
        let probe = ping_host(&transport).await?;
        println!(
            "{}  ssh={}  os={}  arch={}  platform={}",
            resolved.host.name, resolved.host.ssh, probe.os, probe.arch, probe.platform
        );
    }
    Ok(())
}

async fn cmd_config_push(
    path: &PathBuf,
    args: TargetArgs,
    restart: bool,
    backend: SshBackend,
) -> Result<()> {
    let inv = Inventory::load(path)?;
    let cache = ClusterCache::load(&cache_path_for(path)).unwrap_or_default();
    let selector = selector_from(args);
    for resolved in inv.resolve_instances(&selector)? {
        let transport = ssh_transport(&resolved.host.ssh, backend);
        let roster = resolve_cluster_roster(&inv, &cache, &resolved.host.name, &resolved.instance.id);
        push_config_instance(&transport, &resolved, restart, roster.as_ref()).await?;
    }
    Ok(())
}

async fn cmd_install(path: &PathBuf, args: TargetArgs, backend: SshBackend) -> Result<()> {
    let inv = Inventory::load(path)?;
    let cache = ClusterCache::load(&cache_path_for(path)).unwrap_or_default();
    let selector = selector_from(args);
    for resolved in inv.resolve_instances(&selector)? {
        let transport = ssh_transport(&resolved.host.ssh, backend);
        let download = resolve_install_binary(&transport, &resolved).await?;
        if !download.binary_path.exists() {
            bail!("binary not found: {}", download.binary_path.display());
        }
        let roster = resolve_cluster_roster(&inv, &cache, &resolved.host.name, &resolved.instance.id);
        install_instance(&transport, &resolved, &download.binary_path, roster.as_ref()).await?;
    }
    Ok(())
}

async fn cmd_lifecycle(
    path: &PathBuf,
    args: TargetArgs,
    action: LifecycleAction,
    backend: SshBackend,
) -> Result<()> {
    let inv = Inventory::load(path)?;
    let selector = selector_from(args);
    for resolved in inv.resolve_instances(&selector)? {
        let transport = ssh_transport(&resolved.host.ssh, backend);
        lifecycle(&transport, &resolved, action).await?;
    }
    Ok(())
}

async fn cmd_status(path: &PathBuf, args: TargetArgs, backend: SshBackend) -> Result<()> {
    let inv = Inventory::load(path)?;
    let selector = selector_from(args);
    for resolved in inv.resolve_instances(&selector)? {
        let transport = ssh_transport(&resolved.host.ssh, backend);
        let status = status_instance(&transport, &resolved).await?;
        println!(
            "{:<16} active={:<8} state={:<12} {}",
            status.label,
            status.active,
            status.systemd_state,
            status.version_detail()
        );
    }
    Ok(())
}

async fn cmd_logs(
    path: &PathBuf,
    args: TargetArgs,
    follow: bool,
    lines: u32,
    backend: SshBackend,
) -> Result<()> {
    let inv = Inventory::load(path)?;
    let selector = selector_from(args);
    for resolved in inv.resolve_instances(&selector)? {
        let transport = ssh_transport(&resolved.host.ssh, backend);
        if selector.host.is_some() || selector.instance.is_some() || inv.host.len() == 1 {
            logs_instance(&transport, &resolved, follow, lines).await?;
        } else {
            println!("=== {}/{} ===", resolved.host.name, resolved.instance.id);
            logs_instance(&transport, &resolved, false, lines).await?;
        }
    }
    Ok(())
}

async fn cmd_invite(path: &PathBuf, args: TargetArgs, backend: SshBackend) -> Result<()> {
    let inv = Inventory::load(path)?;
    let selector = selector_from(args);
    for resolved in inv.resolve_instances(&selector)? {
        let transport = ssh_transport(&resolved.host.ssh, backend);
        let invite = invite_instance(&transport, &resolved).await?;
        println!("{}", invite.label);
        println!("  source: {}", invite.source_path);
        println!("  invite: {}", invite.invite_url);
        if let Some(fp) = invite.web_cert_fingerprint {
            println!("  web_cert_fingerprint: {fp}");
        }
    }
    Ok(())
}

async fn cmd_exec(
    path: &PathBuf,
    args: TargetArgs,
    command: &str,
    backend: SshBackend,
) -> Result<()> {
    use snm_transport::Transport;

    let inv = Inventory::load(path)?;
    let selector = selector_from(args);
    for resolved in inv.resolve_instances(&selector)? {
        let transport = ssh_transport(&resolved.host.ssh, backend);
        let output = transport.run(command).await?;
        if !output.stdout.is_empty() {
            print!("{}", output.stdout);
        }
        if !output.stderr.is_empty() {
            eprint!("{}", output.stderr);
        }
        if output.exit_code != 0 {
            bail!(
                "remote command failed (exit {}): {}",
                output.exit_code,
                output.stderr.trim()
            );
        }
    }
    Ok(())
}

async fn cmd_remove(path: &PathBuf, args: TargetArgs, yes: bool) -> Result<()> {
    let mut inv = Inventory::load(path)?;
    let selector = selector_from(args);
    let targets: Vec<(String, String)> = inv
        .resolve_instances(&selector)?
        .into_iter()
        .map(|resolved| {
            let label = format!("{}/{}", resolved.host.name, resolved.instance.id);
            confirm_remove_from_inventory(&label, yes)?;
            Ok((resolved.host.name.clone(), resolved.instance.id.clone()))
        })
        .collect::<Result<Vec<_>>>()?;

    for (host_name, instance_id) in targets {
        let label = format!("{host_name}/{instance_id}");
        inv.remove_instance(&host_name, &instance_id)?;
        println!(
            "removed {label} from inventory — uninstall on the host first if the supernode is still running there"
        );
    }

    inv.save(path).context("save inventory")?;
    Ok(())
}

async fn cmd_uninstall(
    path: &PathBuf,
    args: TargetArgs,
    purge: bool,
    yes: bool,
    backend: SshBackend,
) -> Result<()> {
    let inv = Inventory::load(path)?;
    let selector = selector_from(args);
    let targets: Vec<snm_core::ResolvedInstance<'_>> = inv
        .resolve_instances(&selector)?
        .into_iter()
        .map(|resolved| {
            let label = format!("{}/{}", resolved.host.name, resolved.instance.id);
            let layout = InstanceLayout::from_resolved(&resolved);
            confirm_uninstall(&label, purge, &layout.data_dir, &resolved.instance.id, yes)?;
            Ok(resolved)
        })
        .collect::<Result<Vec<_>>>()?;

    for resolved in targets {
        let transport = ssh_transport(&resolved.host.ssh, backend);
        uninstall_instance(&transport, &resolved, purge).await?;
    }

    Ok(())
}

fn confirm_remove_from_inventory(label: &str, yes: bool) -> Result<()> {
    if yes {
        return Ok(());
    }

    eprintln!(
        "Remove {label} from local inventory only. The remote supernode is not stopped or deleted."
    );
    eprintln!("Uninstall on the host first if you want to tear down the running service.");
    eprint!("Continue? [y/N] ");
    io::stderr().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    match answer.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => Ok(()),
        _ => bail!("remove aborted"),
    }
}

fn confirm_uninstall(
    label: &str,
    purge: bool,
    data_dir: &str,
    instance_id: &str,
    yes: bool,
) -> Result<()> {
    if yes {
        return Ok(());
    }

    if purge {
        eprintln!("WARNING: --purge will permanently delete remote data at {data_dir}");
        eprint!("Type '{instance_id}' to confirm purge uninstall of {label}: ");
        io::stderr().flush()?;
        let mut typed = String::new();
        io::stdin().read_line(&mut typed)?;
        if typed.trim() != instance_id {
            bail!("uninstall aborted: confirmation did not match instance id");
        }
        return Ok(());
    }

    eprint!("Uninstall {label} on the remote host (stop/disable service)? [y/N] ");
    io::stderr().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    match answer.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => Ok(()),
        _ => bail!("uninstall aborted"),
    }
}

async fn cmd_build_deploy(
    path: &PathBuf,
    args: TargetArgs,
    source: &std::path::Path,
    target_triple: Option<&str>,
    build_tool_str: &str,
    backend: SshBackend,
) -> Result<()> {
    let tool = LocalBuildTool::parse(build_tool_str).map_err(|e| anyhow::anyhow!(e))?;

    println!(
        "building conquerd-supernode from {} …",
        source.display()
    );
    if let Some(triple) = target_triple {
        println!("  target: {triple}");
    }
    println!("  tool:   {build_tool_str}");

    let binary = build_local_binary(source, target_triple, tool)?;
    println!("built: {}", binary.display());

    let inv = Inventory::load(path)?;
    let cache = ClusterCache::load(&cache_path_for(path)).unwrap_or_default();
    let selector = selector_from(args);
    for resolved in inv.resolve_instances(&selector)? {
        let transport = ssh_transport(&resolved.host.ssh, backend);
        let roster = resolve_cluster_roster(&inv, &cache, &resolved.host.name, &resolved.instance.id);
        install_instance(&transport, &resolved, &binary, roster.as_ref()).await?;
    }
    Ok(())
}

async fn cmd_cluster_sync(
    path: &PathBuf,
    cluster_id: Option<&str>,
    backend: SshBackend,
) -> Result<()> {
    let inv = Inventory::load(path)?;
    if inv.clusters.is_empty() {
        bail!("no [[cluster]] entries in inventory.toml");
    }
    let clusters: Vec<_> = match cluster_id {
        Some(id) => {
            let c = inv
                .clusters
                .iter()
                .find(|c| c.id == id)
                .ok_or_else(|| anyhow::anyhow!("cluster {id:?} not found in inventory"))?;
            vec![c]
        }
        None => inv.clusters.iter().collect(),
    };
    for cluster in clusters {
        for line in cluster_sync_report(&inv, cluster, backend, &cache_path_for(path)).await? {
            println!("{line}");
        }
    }
    Ok(())
}

fn selector_from(args: TargetArgs) -> Selector {
    Selector::from_flags(args.host, args.instance, args.all)
}

fn cache_path_for(inventory_path: &PathBuf) -> std::path::PathBuf {
    inventory_path.with_file_name("cluster_cache.toml")
}

fn resolve_cluster_roster(
    inv: &Inventory,
    cache: &ClusterCache,
    host_name: &str,
    instance_id: &str,
) -> Option<ClusterRoster> {
    let member_key = format!("{host_name}/{instance_id}");
    inv.clusters
        .iter()
        .find(|c| c.members.iter().any(|m| m == &member_key))
        .and_then(|c| cache.find_roster(&c.id))
}
