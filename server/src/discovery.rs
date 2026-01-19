use mdns_sd::{ServiceDaemon, ServiceInfo};
use std::collections::HashMap;

pub struct DiscoveryServer {
    daemon: ServiceDaemon,
}

impl DiscoveryServer {
    pub fn new() -> Self {
        let daemon = ServiceDaemon::new().expect("Failed to create mDNS daemon");
        Self { daemon }
    }

    pub fn start_broadcast(&self, port: u16) -> Result<(), String> {
        let service_type = "_as2p._udp.local.";
        let instance_name = "as2p-pc";
        let host_name = "as2p-pc.local.";
        let properties: HashMap<String, String> = HashMap::new();
        
        let my_service = ServiceInfo::new(
            service_type,
            instance_name,
            host_name,
            "", // IP will be auto-detected
            port,
            properties,
        ).map_err(|e| e.to_string())?;

        self.daemon.register(my_service).map_err(|e| e.to_string())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_discovery_server_creation() {
        let _server = DiscoveryServer::new();
        assert!(true);
    }
}
