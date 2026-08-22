// src-tauri/src/upnp.rs
//
// Apertura/chiusura porte sul router via UPnP (IGD).
// Tutte le funzioni sono bloccanti (ricerca gateway fino a 6 s): vanno
// chiamate da un thread dedicato, mai direttamente dentro un comando Tauri.
//
// Allineato a `upnp_test`: bind sull'IP dell'interfaccia che esce verso internet
// (evita adattatori VPN/VirtualBox), timeout generoso, lease con fallback.

use igd_next::{search_gateway, Gateway, PortMappingProtocol, SearchOptions};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::time::Duration;

const GATEWAY_TIMEOUT: Duration = Duration::from_secs(6);
/// Alcuni router rifiutano il lease 0 ("permanente"): si riprova con 7 giorni.
const FALLBACK_LEASE_SECS: u32 = 7 * 24 * 3600;

pub const HINT: &str =
    "Controlla che UPnP sia attivo sul router e che la rete in Windows sia impostata su \"Privata\".";

/// IP locale dell'interfaccia usata per uscire verso internet.
/// Non invia dati: `connect` su UDP serve solo al SO per scegliere l'interfaccia.
pub fn local_ip() -> Option<Ipv4Addr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    match socket.local_addr() {
        Ok(SocketAddr::V4(addr)) => Some(*addr.ip()),
        _ => None,
    }
}

fn find_gateway(ip: Ipv4Addr) -> Result<Gateway, String> {
    let options = SearchOptions {
        bind_addr: SocketAddr::V4(SocketAddrV4::new(ip, 0)),
        timeout: Some(GATEWAY_TIMEOUT),
        ..Default::default()
    };
    search_gateway(options).map_err(|e| format!("nessun gateway UPnP trovato ({}). {}", e, HINT))
}

/// IP privato / CGNAT: dall'esterno il mapping potrebbe non essere raggiungibile.
fn is_private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_private() || v4.is_loopback() || v4.is_link_local() || (o[0] == 100 && (64..=127).contains(&o[1]))
        }
        IpAddr::V6(v6) => v6.is_loopback() || v6.is_unique_local(),
    }
}

/// Apre `port` (TCP + UDP) verso questa macchina. Successo se almeno TCP va a buon fine.
/// Il messaggio include l'IP pubblico visto dal router.
pub fn map_port(port: u16) -> Result<String, String> {
    let ip = local_ip().ok_or("impossibile rilevare l'IP locale (sei connesso a internet?)")?;
    let gateway = find_gateway(ip)?;

    let local_addr = SocketAddr::V4(SocketAddrV4::new(ip, port));
    let description = format!("Mineger {}", port);

    let add = |proto: PortMappingProtocol| -> Result<(), String> {
        let attempt = |lease: u32| gateway.add_port(proto, port, local_addr, lease, &description);
        match attempt(0) {
            Ok(()) => Ok(()),
            Err(e0) => {
                let text0 = e0.to_string();
                // 718 ConflictInMappingEntry: mapping stantio (es. app chiusa male): rimuovi e riprova.
                if text0.contains("718") || text0.contains("ConflictInMappingEntry") {
                    let _ = gateway.remove_port(proto, port);
                    if attempt(0).is_ok() {
                        return Ok(());
                    }
                }
                // Alcuni router rifiutano il lease 0: riprova con lease lungo.
                attempt(FALLBACK_LEASE_SECS).map_err(|e1| format!("{} (con lease permanente: {})", e1, text0))
            }
        }
    };

    let tcp = add(PortMappingProtocol::TCP);
    let udp = add(PortMappingProtocol::UDP);

    // 729 ConflictWithOtherMechanisms: la porta è già gestita da una regola del router
    // (port-forward manuale): UPnP non serve e la porta è quasi certamente già raggiungibile.
    if let Err(e) = &tcp {
        if e.contains("729") || e.contains("ConflictWithOtherMechanisms") {
            return Err(format!(
                "la porta {} è già gestita da una regola del router (port-forward manuale?): UPnP non necessario, la porta dovrebbe essere già raggiungibile dall'esterno",
                port
            ));
        }
    }

    let public = match gateway.get_external_ip() {
        Ok(ext) if is_private_ip(ext) => format!("IP pubblico {} (sembra CGNAT/doppio NAT: dall'esterno potrebbe non essere raggiungibile)", ext),
        Ok(ext) => format!("IP pubblico {}", ext),
        Err(_) => "IP pubblico non disponibile".to_string(),
    };

    match (tcp, udp) {
        (Ok(_), Ok(_)) => Ok(format!("porta {} (TCP+UDP) aperta su {} · {}", port, gateway.addr, public)),
        (Ok(_), Err(e)) => Ok(format!("porta {} TCP aperta su {} (UDP fallito: {}) · {}", port, gateway.addr, e, public)),
        (Err(e), Ok(_)) => Ok(format!("porta {} UDP aperta su {} (TCP fallito: {}) · {}", port, gateway.addr, e, public)),
        (Err(e1), Err(e2)) => Err(format!("apertura porta {} fallita. TCP: {}; UDP: {}. {}", port, e1, e2, HINT)),
    }
}

/// IP pubblico visto dal router (richiede un gateway UPnP).
pub fn external_ip() -> Option<IpAddr> {
    let ip = local_ip()?;
    find_gateway(ip).ok()?.get_external_ip().ok()
}

/// Chiude una singola porta.
pub fn unmap_port(port: u16) -> Result<String, String> {
    unmap_ports(&[port])
}

/// Chiude più porte con una sola ricerca del gateway (usato allo shutdown dell'app).
pub fn unmap_ports(ports: &[u16]) -> Result<String, String> {
    if ports.is_empty() {
        return Ok("nessuna porta da chiudere".to_string());
    }
    let ip = local_ip().ok_or("impossibile rilevare l'IP locale")?;
    let gateway = find_gateway(ip)?;

    let mut report = Vec::new();
    for &port in ports {
        let tcp = gateway.remove_port(PortMappingProtocol::TCP, port);
        let udp = gateway.remove_port(PortMappingProtocol::UDP, port);
        match (tcp, udp) {
            (Ok(_), Ok(_)) => report.push(format!("porta {} chiusa", port)),
            (Ok(_), Err(_)) => report.push(format!("porta {} TCP chiusa (UDP non trovata)", port)),
            (Err(_), Ok(_)) => report.push(format!("porta {} UDP chiusa (TCP non trovata)", port)),
            (Err(e1), Err(e2)) => report.push(format!("porta {} non chiusa (TCP {}, UDP {})", port, e1, e2)),
        }
    }
    Ok(report.join("; "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_ip_detection() {
        assert!(is_private_ip("192.168.1.10".parse().unwrap()));
        assert!(is_private_ip("10.0.0.1".parse().unwrap()));
        assert!(is_private_ip("100.64.3.4".parse().unwrap()));
        assert!(is_private_ip("100.127.255.1".parse().unwrap()));
        assert!(!is_private_ip("100.128.0.1".parse().unwrap()));
        assert!(!is_private_ip("93.45.12.1".parse().unwrap()));
    }
}
