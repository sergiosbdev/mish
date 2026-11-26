//! Servidores: se leen de ~/.ssh/config y se agrupan por nombre base (web-vpn + web-publico → web).

use std::collections::BTreeMap;
use std::net::{TcpStream, ToSocketAddrs};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug, Default, Clone)]
pub struct Server {
    pub plain: Option<String>,
    pub vpn: Option<String>,
    pub publico: Option<String>,
}

impl Server {
    /// Hosts a probar en orden, según la preferencia.
    pub fn order(&self, prefer: &str) -> Vec<String> {
        let mut v = Vec::new();
        if let Some(p) = &self.plain {
            v.push(p.clone());
        }
        let (a, b) = if prefer == "publico" { (&self.publico, &self.vpn) } else { (&self.vpn, &self.publico) };
        v.extend(a.iter().cloned());
        v.extend(b.iter().cloned());
        v
    }
}

pub type Servers = BTreeMap<String, Server>;

pub fn load(extra: &BTreeMap<String, String>) -> Servers {
    let path = format!("{}/.ssh/config", crate::parse::home());
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let mut map = parse_ssh_config(&text);
    for (name, dest) in extra {
        map.entry(name.clone()).or_default().plain = Some(dest.clone());
    }
    map
}

pub fn parse_ssh_config(text: &str) -> Servers {
    let mut map: Servers = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        let mut parts = line.split_whitespace();
        if !parts.next().is_some_and(|k| k.eq_ignore_ascii_case("host")) {
            continue;
        }
        for host in parts {
            if host.contains(['*', '?', '!']) {
                continue;
            }
            if let Some(base) = host.strip_suffix("-vpn") {
                map.entry(base.into()).or_default().vpn = Some(host.into());
            } else if let Some(base) = host.strip_suffix("-publico") {
                map.entry(base.into()).or_default().publico = Some(host.into());
            } else {
                map.entry(host.into()).or_default().plain = Some(host.into());
            }
        }
    }
    map
}

/// Todos los alias de host (para autocompletar ssh/scp).
pub fn all_hosts(s: &Servers) -> Vec<String> {
    let mut v: Vec<String> = s
        .values()
        .flat_map(|sv| [&sv.plain, &sv.vpn, &sv.publico].into_iter().flatten().cloned())
        .collect();
    v.sort();
    v.dedup();
    v
}

/// Intenta conectar por TCP al puerto SSH del host (resuelto con `ssh -G`). Devuelve la latencia.
pub fn probe(host: &str) -> Option<Duration> {
    let out = Command::new("ssh").args(["-G", host]).stderr(Stdio::null()).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut hostname = host.to_string();
    let mut port = 22u16;
    for l in text.lines() {
        if let Some(h) = l.strip_prefix("hostname ") {
            hostname = h.trim().to_string();
        } else if let Some(p) = l.strip_prefix("port ") {
            port = p.trim().parse().unwrap_or(22);
        }
    }
    // macOS (privacidad de «Red local») devuelve «No route to host» a binarios que no son de
    // Apple al conectar a la LAN (NAS, Raspberry…), pero deja pasar a ssh y nc. Por eso el sondeo
    // va por /usr/bin/nc: mismo criterio que el ssh real. Si no existe, TCP directo.
    let nc = std::path::Path::new("/usr/bin/nc");
    let t = Instant::now();
    if nc.exists() {
        let ok = Command::new(nc)
            .args(["-z", "-G", "2", "-w", "2", &hostname, &port.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .ok()?
            .success();
        return ok.then(|| t.elapsed());
    }
    let addr = (hostname.as_str(), port).to_socket_addrs().ok()?.next()?;
    TcpStream::connect_timeout(&addr, Duration::from_millis(1500)).ok()?;
    Some(t.elapsed())
}
