use std::path::PathBuf;

use snm_core::Inventory;
use snm_supernode::{
    build_local_binary, cluster_sync_report, install_instance_report, invite_instance,
    lifecycle_report, logs_instance_text, ping_host, push_config_instance_report,
    resolve_install_binary, status_instance, uninstall_instance_report, ClusterCache,
    ClusterRoster, LifecycleAction, LocalBuildTool,
};
use snm_transport::{SshBackend, SshTransport};
use tokio::sync::mpsc;

use super::app::{WorkerCmd, WorkerMsg};

pub fn spawn_worker(
    inventory_path: PathBuf,
    mut inventory: Inventory,
    ssh_backend: SshBackend,
    mut cmd_rx: mpsc::UnboundedReceiver<WorkerCmd>,
    msg_tx: mpsc::UnboundedSender<WorkerMsg>,
) {
    tokio::spawn(async move {
        while let Some(cmd) = cmd_rx.recv().await {
            if let Err(e) = reload_inventory(&inventory_path, &mut inventory) {
                let _ = msg_tx.send(WorkerMsg::Notice {
                    message: format!("reload inventory failed: {e}"),
                });
                continue;
            }
            match cmd {
                WorkerCmd::RefreshAll => {
                    let count = inventory
                        .resolve_instances(&snm_core::Selector::default())
                        .map(|v| v.len())
                        .unwrap_or(0);
                    for row in 0..count {
                        refresh_one(&inventory, row, ssh_backend, &msg_tx).await;
                    }
                }
                WorkerCmd::Ping(row) => {
                    ping_one(&inventory, row, ssh_backend, &msg_tx).await;
                }
                WorkerCmd::Lifecycle(row, action) => {
                    lifecycle_one(&inventory, row, action, ssh_backend, &msg_tx).await;
                }
                WorkerCmd::Install(row) => {
                    install_one(&inventory_path, &mut inventory, row, ssh_backend, &msg_tx).await;
                }
                WorkerCmd::ConfigPush(row) => {
                    config_push_one(&inventory_path, &inventory, row, ssh_backend, &msg_tx).await;
                }
                WorkerCmd::FetchLogs(row) => {
                    fetch_logs_one(&inventory, row, ssh_backend, &msg_tx).await;
                }
                WorkerCmd::FetchInvite(row) => {
                    fetch_invite_one(&inventory, row, ssh_backend, &msg_tx).await;
                }
                WorkerCmd::Uninstall { row, purge } => {
                    uninstall_one(&inventory, row, purge, ssh_backend, &msg_tx).await;
                }
                WorkerCmd::ClusterSync => {
                    cluster_sync_one(&inventory_path, &mut inventory, ssh_backend, &msg_tx).await;
                }
                WorkerCmd::BuildDeploy(row) => {
                    build_deploy_one(&inventory_path, &mut inventory, row, ssh_backend, &msg_tx)
                        .await;
                }
            }
        }
    });
}

fn reload_inventory(inventory_path: &PathBuf, inventory: &mut Inventory) -> anyhow::Result<()> {
    *inventory = Inventory::load(inventory_path)?;
    Ok(())
}

async fn refresh_one(
    inventory: &Inventory,
    row: usize,
    backend: SshBackend,
    msg_tx: &mpsc::UnboundedSender<WorkerMsg>,
) {
    let result = match resolve_row(inventory, row).await {
        Ok(resolved) => {
            let transport = SshTransport::new(&resolved.host.ssh, backend);
            status_instance(&transport, &resolved)
                .await
                .map_err(|e| e.to_string())
        }
        Err(e) => Err(e.to_string()),
    };
    let _ = msg_tx.send(WorkerMsg::Status { row, result });
}

async fn ping_one(
    inventory: &Inventory,
    row: usize,
    backend: SshBackend,
    msg_tx: &mpsc::UnboundedSender<WorkerMsg>,
) {
    let result = match resolve_row(inventory, row).await {
        Ok(resolved) => {
            let transport = SshTransport::new(&resolved.host.ssh, backend);
            ping_host(&transport).await.map_err(|e| e.to_string())
        }
        Err(e) => Err(e.to_string()),
    };
    let _ = msg_tx.send(WorkerMsg::Ping { row, result });
}

async fn lifecycle_one(
    inventory: &Inventory,
    row: usize,
    action: LifecycleAction,
    backend: SshBackend,
    msg_tx: &mpsc::UnboundedSender<WorkerMsg>,
) {
    let label = inventory
        .resolve_instances(&snm_core::Selector::default())
        .ok()
        .and_then(|v| {
            v.get(row)
                .map(|r| format!("{}/{}", r.host.name, r.instance.id))
        })
        .unwrap_or_else(|| format!("row {row}"));

    let result = match resolve_row(inventory, row).await {
        Ok(resolved) => {
            let transport = SshTransport::new(&resolved.host.ssh, backend);
            lifecycle_report(&transport, &resolved, action).await
        }
        Err(e) => Err(e),
    };

    match result {
        Ok(_) => {
            refresh_one(inventory, row, backend, msg_tx).await;
            let _ = msg_tx.send(WorkerMsg::Notice {
                message: format!("{} {label} ok", action.as_str()),
            });
        }
        Err(e) => {
            let _ = msg_tx.send(WorkerMsg::Notice {
                message: format!("{} {label} failed: {e}", action.as_str()),
            });
        }
    }
}

async fn config_push_one(
    inventory_path: &PathBuf,
    inventory: &Inventory,
    row: usize,
    backend: SshBackend,
    msg_tx: &mpsc::UnboundedSender<WorkerMsg>,
) {
    let label = inventory
        .resolve_instances(&snm_core::Selector::default())
        .ok()
        .and_then(|v| {
            v.get(row)
                .map(|r| format!("{}/{}", r.host.name, r.instance.id))
        })
        .unwrap_or_else(|| format!("row {row}"));

    let cache_path = inventory_path.with_file_name("cluster_cache.toml");
    let cache = ClusterCache::load(&cache_path).unwrap_or_default();

    let result = match resolve_row(inventory, row).await {
        Ok(resolved) => {
            let transport = SshTransport::new(&resolved.host.ssh, backend);
            let roster = resolve_worker_roster(
                inventory,
                &cache,
                &resolved.host.name,
                &resolved.instance.id,
            );
            push_config_instance_report(&transport, &resolved, true, roster.as_ref()).await
        }
        Err(e) => Err(e),
    };

    match result {
        Ok(report) => {
            refresh_one(inventory, row, backend, msg_tx).await;
            send_operation_log(
                msg_tx,
                format!("Push Config {label}"),
                report,
                format!("config pushed {label}"),
            );
        }
        Err(e) => {
            send_operation_error(
                msg_tx,
                format!("Push Config {label}"),
                format!("config push {label} failed: {e}"),
                e,
            );
        }
    }
}

async fn install_one(
    inventory_path: &PathBuf,
    inventory: &mut Inventory,
    row: usize,
    backend: SshBackend,
    msg_tx: &mpsc::UnboundedSender<WorkerMsg>,
) {
    let label = inventory
        .resolve_instances(&snm_core::Selector::default())
        .ok()
        .and_then(|v| {
            v.get(row)
                .map(|r| format!("{}/{}", r.host.name, r.instance.id))
        })
        .unwrap_or_else(|| format!("row {row}"));

    let result = match resolve_row(inventory, row).await {
        Ok(resolved) => {
            let transport = SshTransport::new(&resolved.host.ssh, backend);
            let cache_path = inventory_path.with_file_name("cluster_cache.toml");
            let cache = ClusterCache::load(&cache_path).unwrap_or_default();
            let roster = resolve_worker_roster(
                inventory,
                &cache,
                &resolved.host.name,
                &resolved.instance.id,
            );
            match resolve_install_binary(&transport, &resolved).await {
                Ok(download) if download.binary_path.exists() => {
                    install_instance_report(
                        &transport,
                        &resolved,
                        &download.binary_path,
                        roster.as_ref(),
                    )
                    .await
                }
                Ok(download) => Err(anyhow::anyhow!(
                    "binary not found at {}",
                    download.binary_path.display()
                )),
                Err(e) => Err(e),
            }
        }
        Err(e) => Err(e),
    };

    match result {
        Ok(report) => {
            refresh_one(inventory, row, backend, msg_tx).await;
            send_operation_log(
                msg_tx,
                format!("Install {label}"),
                report,
                format!("installed {label}"),
            );
        }
        Err(e) => {
            send_operation_error(
                msg_tx,
                format!("Install {label}"),
                format!("install {label} failed: {e}"),
                e,
            );
        }
    }

    if let Ok(inv) = Inventory::load(inventory_path) {
        *inventory = inv;
    }
}

async fn fetch_logs_one(
    inventory: &Inventory,
    row: usize,
    backend: SshBackend,
    msg_tx: &mpsc::UnboundedSender<WorkerMsg>,
) {
    let content = match resolve_row(inventory, row).await {
        Ok(resolved) => {
            let transport = SshTransport::new(&resolved.host.ssh, backend);
            logs_instance_text(&transport, &resolved, false, 200)
                .await
                .map_err(|e| e.to_string())
        }
        Err(e) => Err(e.to_string()),
    };
    let _ = msg_tx.send(WorkerMsg::Logs { row, content });
}

async fn fetch_invite_one(
    inventory: &Inventory,
    row: usize,
    backend: SshBackend,
    msg_tx: &mpsc::UnboundedSender<WorkerMsg>,
) {
    let content = match resolve_row(inventory, row).await {
        Ok(resolved) => {
            let transport = SshTransport::new(&resolved.host.ssh, backend);
            invite_instance(&transport, &resolved)
                .await
                .map(|invite| {
                    let text =
                        format!("source: {}\n\n{}", invite.source_path, invite.invite_url);

                    text
                })
                .map_err(|e| e.to_string())
        }
        Err(e) => Err(e.to_string()),
    };
    let _ = msg_tx.send(WorkerMsg::Invite { row, content });
}

async fn uninstall_one(
    inventory: &Inventory,
    row: usize,
    purge: bool,
    backend: SshBackend,
    msg_tx: &mpsc::UnboundedSender<WorkerMsg>,
) {
    let resolved = match resolve_row(inventory, row).await {
        Ok(resolved) => resolved,
        Err(e) => {
            send_operation_error(
                msg_tx,
                format!("Uninstall row {row}"),
                format!("uninstall row {row} failed: {e}"),
                e,
            );
            return;
        }
    };

    let label = format!("{}/{}", resolved.host.name, resolved.instance.id);

    let result = {
        let transport = SshTransport::new(&resolved.host.ssh, backend);
        uninstall_instance_report(&transport, &resolved, purge).await
    };

    match result {
        Ok(report) => {
            refresh_one(inventory, row, backend, msg_tx).await;
            send_operation_log(
                msg_tx,
                format!("Uninstall {label}"),
                report,
                if purge {
                    format!("uninstalled and purged {label}")
                } else {
                    format!("uninstalled {label}")
                },
            );
        }
        Err(e) => {
            send_operation_error(
                msg_tx,
                format!("Uninstall {label}"),
                format!("uninstall {label} failed: {e}"),
                e,
            );
        }
    }
}

async fn cluster_sync_one(
    inventory_path: &PathBuf,
    inventory: &mut Inventory,
    backend: SshBackend,
    msg_tx: &mpsc::UnboundedSender<WorkerMsg>,
) {
    if let Ok(inv) = Inventory::load(inventory_path) {
        *inventory = inv;
    }
    if inventory.clusters.is_empty() {
        let _ = msg_tx.send(WorkerMsg::Notice {
            message: "no [[cluster]] entries in inventory.toml".into(),
        });
        return;
    }
    let clusters = inventory.clusters.clone();
    let cache_path = inventory_path.with_file_name("cluster_cache.toml");
    let mut all_lines = Vec::new();
    let mut had_error = false;
    for cluster in &clusters {
        match cluster_sync_report(inventory, cluster, backend, &cache_path).await {
            Ok(lines) => all_lines.extend(lines),
            Err(e) => {
                had_error = true;
                all_lines.push(format!("cluster {}: error: {e:#}", cluster.id));
            }
        }
    }
    let status = if had_error {
        "cluster sync completed with errors".into()
    } else {
        format!("cluster sync ok ({} cluster(s))", clusters.len())
    };
    send_operation_log(msg_tx, "Cluster Sync".into(), all_lines, status);
}

async fn build_deploy_one(
    inventory_path: &PathBuf,
    inventory: &mut Inventory,
    row: usize,
    backend: SshBackend,
    msg_tx: &mpsc::UnboundedSender<WorkerMsg>,
) {
    if let Ok(inv) = Inventory::load(inventory_path) {
        *inventory = inv;
    }

    let source = match &inventory.defaults.build_source {
        Some(p) => p.clone(),
        None => {
            send_operation_error(
                msg_tx,
                "Build & Deploy".into(),
                "build_source not set — configure it in Settings".into(),
                anyhow::anyhow!("defaults.build_source is required for build-deploy"),
            );
            return;
        }
    };
    let target = inventory.defaults.build_target.clone();
    let tool_str = inventory
        .defaults
        .build_tool
        .clone()
        .unwrap_or_else(|| "cargo".into());
    let tool = match LocalBuildTool::parse(&tool_str) {
        Ok(t) => t,
        Err(e) => {
            send_operation_error(
                msg_tx,
                "Build & Deploy".into(),
                format!("invalid build_tool: {e}"),
                e,
            );
            return;
        }
    };

    let label = inventory
        .resolve_instances(&snm_core::Selector::default())
        .ok()
        .and_then(|v| {
            v.get(row)
                .map(|r| format!("{}/{}", r.host.name, r.instance.id))
        })
        .unwrap_or_else(|| format!("row {row}"));

    // Build is blocking/CPU-bound; run on the blocking thread pool to avoid
    // stalling the async runtime.
    let source_clone = source.clone();
    let target_clone = target.clone();
    let binary_result = tokio::task::spawn_blocking(move || {
        build_local_binary(&source_clone, target_clone.as_deref(), tool)
    })
    .await;

    let binary_path = match binary_result {
        Ok(Ok(p)) => p,
        Ok(Err(e)) => {
            send_operation_error(
                msg_tx,
                format!("Build & Deploy {label}"),
                format!("build failed: {e}"),
                e,
            );
            return;
        }
        Err(e) => {
            send_operation_error(
                msg_tx,
                format!("Build & Deploy {label}"),
                format!("build task panicked: {e}"),
                anyhow::anyhow!("{e}"),
            );
            return;
        }
    };

    let build_msg = format!("built: {}", binary_path.display());

    let result = match resolve_row(inventory, row).await {
        Ok(resolved) => {
            let transport = SshTransport::new(&resolved.host.ssh, backend);
            let cache_path = inventory_path.with_file_name("cluster_cache.toml");
            let cache = ClusterCache::load(&cache_path).unwrap_or_default();
            let roster = resolve_worker_roster(
                inventory,
                &cache,
                &resolved.host.name,
                &resolved.instance.id,
            );
            install_instance_report(&transport, &resolved, &binary_path, roster.as_ref()).await
        }
        Err(e) => Err(e),
    };

    match result {
        Ok(mut report) => {
            report.insert(0, build_msg);
            refresh_one(inventory, row, backend, msg_tx).await;
            send_operation_log(
                msg_tx,
                format!("Build & Deploy {label}"),
                report,
                format!("built and deployed {label}"),
            );
        }
        Err(e) => {
            send_operation_error(
                msg_tx,
                format!("Build & Deploy {label}"),
                format!("deploy {label} failed: {e}"),
                e,
            );
        }
    }

    if let Ok(inv) = Inventory::load(inventory_path) {
        *inventory = inv;
    }
}

fn send_operation_log(
    msg_tx: &mpsc::UnboundedSender<WorkerMsg>,
    title: String,
    lines: Vec<String>,
    status: String,
) {
    let content = if lines.is_empty() {
        "completed without additional output".into()
    } else {
        lines.join("\n")
    };
    let _ = msg_tx.send(WorkerMsg::OperationLog {
        title,
        content,
        status,
    });
}

fn send_operation_error(
    msg_tx: &mpsc::UnboundedSender<WorkerMsg>,
    title: String,
    status: String,
    error: anyhow::Error,
) {
    let _ = msg_tx.send(WorkerMsg::OperationLog {
        title,
        content: format!("error:\n{error:#}"),
        status,
    });
}

async fn resolve_row<'a>(
    inventory: &'a Inventory,
    row: usize,
) -> Result<snm_core::ResolvedInstance<'a>, anyhow::Error> {
    inventory
        .resolve_instances(&snm_core::Selector::default())?
        .into_iter()
        .nth(row)
        .ok_or_else(|| anyhow::anyhow!("row {row} not found"))
}

fn resolve_worker_roster(
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
