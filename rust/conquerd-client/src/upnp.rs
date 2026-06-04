//! UPnP port mapping manager.
//!
//! Discovers an IGD (Internet Gateway Device) on the LAN and adds/removes
//! port forwarding rules to improve P2P direct-connect rates.
//! Falls back gracefully when UPnP is unavailable or discovery fails.

use std::net::Ipv4Addr;
use std::time::Duration;

use rupnp::ssdp::{SearchTarget, URN};
use rupnp::Service;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

const WAN_IP_URN: &str = "WANIPConnection";
const WAN_PPP_URN: &str = "WANPPPConnection";
const DISCOVER_TIMEOUT: Duration = Duration::from_secs(3);
const MAPPING_LEASE_SECS: u32 = 3600; // 1-hour lease; renewed on restart

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Events emitted by the UPnP manager.
#[derive(Debug, Clone)]
pub enum UpnpEvent {
    /// UPnP gateway discovered.
    GatewayDiscovered { external_ip: String },
    /// Port mapping added successfully.
    MappingAdded {
        external_port: u16,
        protocol: Protocol,
    },
    /// Port mapping removed.
    MappingRemoved {
        external_port: u16,
        protocol: Protocol,
    },
    /// Discovery or mapping operation failed (non-fatal).
    Error(String),
    /// UPnP not available on this network.
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Protocol {
    Tcp,
    Udp,
}

impl Protocol {
    fn as_str(self) -> &'static str {
        match self {
            Self::Tcp => "TCP",
            Self::Udp => "UDP",
        }
    }
}

/// Commands sent to the UPnP manager task.
#[derive(Debug)]
pub enum UpnpCommand {
    /// Add a port mapping.
    AddMapping {
        internal_port: u16,
        external_port: u16,
        protocol: Protocol,
        description: String,
    },
    /// Remove a specific mapping.
    RemoveMapping {
        external_port: u16,
        protocol: Protocol,
    },
    /// Remove all mappings added in this session (call before shutdown).
    RemoveAll,
    Shutdown,
}

// ---------------------------------------------------------------------------
// Internal state
// ---------------------------------------------------------------------------

struct ActiveMapping {
    external_port: u16,
    protocol: Protocol,
    service: Service,
    device_url: rupnp::http::Uri,
}

struct GatewayState {
    service: Service,
    device_url: rupnp::http::Uri,
    _external_ip: String,
}

// ---------------------------------------------------------------------------
// UPnPManager
// ---------------------------------------------------------------------------

pub struct UPnPManager {
    local_ip: Option<Ipv4Addr>,
    gateway: Option<GatewayState>,
    active: Vec<ActiveMapping>,
    event_tx: mpsc::Sender<UpnpEvent>,
    cmd_rx: mpsc::Receiver<UpnpCommand>,
}

impl UPnPManager {
    /// Create the manager and split into `(cmd_tx, event_rx, task_future)`.
    pub fn split() -> (
        mpsc::Sender<UpnpCommand>,
        mpsc::Receiver<UpnpEvent>,
        impl std::future::Future<Output = ()> + Send,
    ) {
        let (event_tx, event_rx) = mpsc::channel::<UpnpEvent>(16);
        let (cmd_tx, cmd_rx) = mpsc::channel::<UpnpCommand>(16);
        let mgr = Self {
            local_ip: None,
            gateway: None,
            active: Vec::new(),
            event_tx,
            cmd_rx,
        };
        (cmd_tx, event_rx, mgr.run())
    }

    // -----------------------------------------------------------------------
    // Discovery
    // -----------------------------------------------------------------------

    async fn discover(&mut self) {
        let search = SearchTarget::RootDevice;
        let devices = match rupnp::discover(&search, DISCOVER_TIMEOUT).await {
            Ok(d) => d,
            Err(e) => {
                debug!("UPnP discovery error: {e}");
                let _ = self.event_tx.try_send(UpnpEvent::Unavailable);
                return;
            }
        };

        use futures_util::{pin_mut, StreamExt};
        pin_mut!(devices);

        while let Some(Ok(device)) = devices.next().await {
            let wan_ip_urn = URN::service("schemas-upnp-org", WAN_IP_URN, 1);
            let wan_ppp_urn = URN::service("schemas-upnp-org", WAN_PPP_URN, 1);

            let service = if let Some(s) = device.find_service(&wan_ip_urn) {
                s.clone()
            } else if let Some(s) = device.find_service(&wan_ppp_urn) {
                s.clone()
            } else {
                continue;
            };

            let device_url = device.url().clone();
            let ext_ip = service
                .action(&device_url, "GetExternalIPAddress", "")
                .await
                .ok()
                .and_then(|r| r.get("NewExternalIPAddress").cloned())
                .unwrap_or_default();

            info!(
                "UPnP gateway: {} (WAN IP: {})",
                device.friendly_name(),
                ext_ip
            );
            let _ = self.event_tx.try_send(UpnpEvent::GatewayDiscovered {
                external_ip: ext_ip.clone(),
            });
            self.gateway = Some(GatewayState {
                service,
                device_url,
                _external_ip: ext_ip,
            });
            return;
        }

        debug!("No UPnP IGD found");
        let _ = self.event_tx.try_send(UpnpEvent::Unavailable);
    }

    // -----------------------------------------------------------------------
    // Mapping management
    // -----------------------------------------------------------------------

    async fn add_mapping(
        &mut self,
        internal_port: u16,
        external_port: u16,
        protocol: Protocol,
        description: &str,
    ) {
        let gw = match &self.gateway {
            Some(g) => g,
            None => {
                warn!("UPnP: no gateway, skipping AddPortMapping");
                let _ = self
                    .event_tx
                    .try_send(UpnpEvent::Error("No UPnP gateway".into()));
                return;
            }
        };

        let local_ip = self
            .local_ip
            .map(|ip| ip.to_string())
            .unwrap_or_else(|| "0.0.0.0".into());

        let payload = format!(
            "<NewRemoteHost></NewRemoteHost>\
             <NewExternalPort>{external_port}</NewExternalPort>\
             <NewProtocol>{proto}</NewProtocol>\
             <NewInternalPort>{internal_port}</NewInternalPort>\
             <NewInternalClient>{local_ip}</NewInternalClient>\
             <NewEnabled>1</NewEnabled>\
             <NewPortMappingDescription>{description}</NewPortMappingDescription>\
             <NewLeaseDuration>{MAPPING_LEASE_SECS}</NewLeaseDuration>",
            proto = protocol.as_str(),
        );

        let service = gw.service.clone();
        let device_url = gw.device_url.clone();

        match service
            .action(&device_url, "AddPortMapping", &payload)
            .await
        {
            Ok(_) => {
                info!(
                    "UPnP mapping added: {:?} ext:{external_port} → int:{internal_port}",
                    protocol
                );
                self.active.push(ActiveMapping {
                    external_port,
                    protocol,
                    service,
                    device_url,
                });
                let _ = self.event_tx.try_send(UpnpEvent::MappingAdded {
                    external_port,
                    protocol,
                });
            }
            Err(e) => {
                warn!("UPnP AddPortMapping failed: {e}");
                let _ = self.event_tx.try_send(UpnpEvent::Error(e.to_string()));
            }
        }
    }

    async fn remove_mapping(&mut self, external_port: u16, protocol: Protocol) {
        let idx = self
            .active
            .iter()
            .position(|m| m.external_port == external_port && m.protocol == protocol);
        let Some(idx) = idx else { return };
        let m = self.active.remove(idx);

        let payload = format!(
            "<NewRemoteHost></NewRemoteHost>\
             <NewExternalPort>{}</NewExternalPort>\
             <NewProtocol>{}</NewProtocol>",
            m.external_port,
            m.protocol.as_str(),
        );
        let _ = m
            .service
            .action(&m.device_url, "DeletePortMapping", &payload)
            .await;
        debug!(
            "UPnP mapping removed: {:?} ext:{}",
            m.protocol, m.external_port
        );
        let _ = self.event_tx.try_send(UpnpEvent::MappingRemoved {
            external_port,
            protocol,
        });
    }

    async fn remove_all(&mut self) {
        let mappings: Vec<ActiveMapping> = self.active.drain(..).collect();
        for m in mappings {
            let payload = format!(
                "<NewRemoteHost></NewRemoteHost>\
                 <NewExternalPort>{}</NewExternalPort>\
                 <NewProtocol>{}</NewProtocol>",
                m.external_port,
                m.protocol.as_str(),
            );
            let _ = m
                .service
                .action(&m.device_url, "DeletePortMapping", &payload)
                .await;
            debug!("UPnP removed {:?} ext:{}", m.protocol, m.external_port);
        }
    }

    // -----------------------------------------------------------------------
    // Event loop
    // -----------------------------------------------------------------------

    async fn run(mut self) {
        info!("UPnP manager started — discovering gateway…");
        self.discover().await;

        loop {
            match self.cmd_rx.recv().await {
                None | Some(UpnpCommand::Shutdown) => {
                    self.remove_all().await;
                    break;
                }
                Some(UpnpCommand::RemoveAll) => self.remove_all().await,
                Some(UpnpCommand::AddMapping {
                    internal_port,
                    external_port,
                    protocol,
                    description,
                }) => {
                    self.add_mapping(internal_port, external_port, protocol, &description)
                        .await;
                }
                Some(UpnpCommand::RemoveMapping {
                    external_port,
                    protocol,
                }) => {
                    self.remove_mapping(external_port, protocol).await;
                }
            }
        }
        info!("UPnP manager stopped");
    }
}
