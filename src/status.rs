//! Comprobaciones en segundo plano: pantalla de bienvenida y vigilancia de la VPN.

use crate::servers::{self, Servers};
use crate::theme::Theme;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::Arc;
use std::time::Duration;

/// 0 = desconocido, 1 = VPN accesible, 2 = sin VPN.
pub type VpnFlag = Arc<AtomicU8>;

pub fn vpn_down(f: &VpnFlag) -> bool {
    f.load(Ordering::Relaxed) == 2
}

fn probe_all(hosts: Vec<String>) -> Vec<(String, bool)> {
    let hs: Vec<_> = hosts
        .into_iter()
        .map(|h| std::thread::spawn(move || (h.clone(), servers::probe(&h).is_some())))
        .collect();
    hs.into_iter().filter_map(|h| h.join().ok()).collect()
}

/// Cabecera de bienvenida (lo que no requiere red).
///
/// ```text
///   ❯ mish   lunes 23 nov  20:16 · mi-mac
///     z      api · docs · notas · proyecto · web            ← carpetas recientes de verdad
///     ssh    ●●●●●●●●●●○●  11/12 · sin respuesta: db        ← llega después (spawn)
/// ```
/// El ❯ cuelga en el margen; etiquetas bajo «mish» y valores en la columna 11.
pub fn welcome_header(t: &Theme, dirs: &[String], jobs: usize) {
    let fecha = fecha_larga();
    let (dia, hora) = fecha.split_once(" · ").unwrap_or((fecha.as_str(), ""));
    let sep = t.muted.s().paint(" · ").to_string();
    println!();
    println!(
        "  {} {}   {}  {}{sep}{}",
        t.accent.b().paint("❯"),
        t.text.b().paint("mish"),
        t.text.s().paint(dia),
        t.cwd.s().paint(hora),
        t.muted.s().paint(crate::prompt::hostname())
    );
    let d = fit_names(dirs, ANCHO - 4 - 7);
    if !d.is_empty() {
        let d: Vec<String> = d.iter().map(|d| t.cwd.s().paint(d.as_str()).to_string()).collect();
        println!("    {}{}", label(t, "z"), d.join(&sep));
    }
    if jobs > 0 {
        println!("    {}{}", label(t, "jobs"), t.warn.s().paint(format!("{jobs} detenidos")));
    }
}

/// Los nombres que caben (en orden) en `ancho` columnas, separados por " · ".
fn fit_names(names: &[String], ancho: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut usado = 0;
    for n in names {
        let w = n.chars().count() + if out.is_empty() { 0 } else { 3 };
        if usado + w > ancho {
            break;
        }
        usado += w;
        out.push(n.clone());
    }
    out
}

/// Etiqueta de la bienvenida: 7 columnas, en gris.
fn label(t: &Theme, s: &str) -> String {
    t.muted.s().paint(format!("{s:<7}")).to_string()
}

/// Ancho visible máximo de la línea de servidores.
const ANCHO: usize = 110;

/// Línea de servidores. Si caben, todos con nombre; si no, una tira de puntos (uno por
/// servidor, en orden) con el recuento y sólo los nombres de los caídos.
fn servers_line(t: &Theme, estado: &[(String, bool)]) -> String {
    let up = estado.iter().filter(|(_, u)| *u).count();
    let total = estado.len();
    let caidos: Vec<&str> = estado.iter().filter(|(_, u)| !*u).map(|(n, _)| n.as_str()).collect();
    let cuenta = format!("{up}/{total}");
    let margen = 4 + 7; // "    " + etiqueta
    let dot = |u: bool| if u { t.ok.s().paint("●") } else { t.err.s().paint("○") };

    // Forma larga: ● db  ● web  …  11/12
    let largo: usize = estado.iter().map(|(n, _)| n.chars().count() + 4).sum::<usize>() + cuenta.len();
    let cuerpo = if margen + largo <= ANCHO {
        let parts: Vec<String> = estado
            .iter()
            .map(|(n, u)| {
                let nombre = if *u { t.text.s().paint(n.as_str()) } else { t.err.s().paint(n.as_str()) };
                format!("{} {nombre}", dot(*u))
            })
            .collect();
        format!("{}  {}", parts.join("  "), t.muted.s().paint(&cuenta))
    } else {
        let tira: String = estado.iter().map(|(_, u)| dot(*u).to_string()).collect();
        let mut s = format!("{tira}  {}", if caidos.is_empty() { t.ok.s() } else { t.text.s() }.paint(&cuenta));
        let mut ancho = margen + total + 2 + cuenta.len();
        if caidos.is_empty() {
            s.push_str(&format!("{}", t.muted.s().paint(" · todos responden")));
        } else if up == 0 {
            s.push_str(&format!("{}", t.err.s().paint(" · ninguno responde")));
        } else {
            let pre = " · sin respuesta: ";
            s.push_str(&t.muted.s().paint(pre).to_string());
            ancho += pre.chars().count();
            let mut puestos = Vec::new();
            for (i, n) in caidos.iter().enumerate() {
                let resto = caidos.len() - i - 1;
                let extra = if resto > 0 { format!(" +{resto}").len() } else { 0 };
                let w = n.chars().count() + if i > 0 { 2 } else { 0 };
                if ancho + w + extra > ANCHO {
                    break;
                }
                ancho += w;
                puestos.push(t.err.s().paint(*n).to_string());
            }
            s.push_str(&puestos.join(", "));
            let faltan = caidos.len() - puestos.len();
            if faltan > 0 {
                s.push_str(&t.muted.s().paint(format!(" +{faltan}")).to_string());
            }
        }
        s
    };
    format!("    {}{cuerpo}", label(t, "ssh"))
}

/// Lanza las comprobaciones: línea de servidores para la bienvenida + vigilancia periódica de la VPN.
pub fn spawn(servers: Servers, prefer: String, t: Theme, printer: Option<SyncSender<String>>, vpn: VpnFlag) {
    std::thread::spawn(move || {
        // Primera ronda: todos los servidores, para la bienvenida.
        let hosts: Vec<String> = servers.values().filter_map(|s| s.order(&prefer).into_iter().next()).collect();
        let res = probe_all(hosts);
        let ok = |h: &str| res.iter().any(|(x, up)| x == h && *up);
        let vpn_hosts: Vec<String> = servers.values().filter_map(|s| s.vpn.clone()).collect();
        update_vpn(&vpn, &vpn_hosts, &res);

        if let Some(p) = &printer {
            let estado: Vec<(String, bool)> = servers
                .iter()
                .filter_map(|(name, sv)| sv.order(&prefer).into_iter().next().map(|h| (name.clone(), ok(&h))))
                .collect();
            // Línea en blanco detrás: separa el bloque de bienvenida del primer prompt.
            let _ = p.send(format!("{}\n\n", servers_line(&t, &estado)));
        }

        // Después: sólo la VPN, cada 30 s.
        if vpn_hosts.is_empty() {
            return;
        }
        loop {
            std::thread::sleep(Duration::from_secs(30));
            let res = probe_all(vpn_hosts.iter().take(2).cloned().collect());
            update_vpn(&vpn, &vpn_hosts, &res);
        }
    });
}

fn update_vpn(flag: &VpnFlag, vpn_hosts: &[String], res: &[(String, bool)]) {
    let tested: Vec<&(String, bool)> = res.iter().filter(|(h, _)| vpn_hosts.contains(h)).collect();
    if tested.is_empty() {
        return;
    }
    let any_up = tested.iter().any(|(_, u)| *u);
    flag.store(if any_up { 1 } else { 2 }, Ordering::Relaxed);
}

fn fecha_larga() -> String {
    const DIAS: [&str; 7] = ["domingo", "lunes", "martes", "miércoles", "jueves", "viernes", "sábado"];
    const MESES: [&str; 12] = ["ene", "feb", "mar", "abr", "may", "jun", "jul", "ago", "sep", "oct", "nov", "dic"];
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&now, &mut tm);
        format!(
            "{} {} {} · {:02}:{:02}",
            DIAS[tm.tm_wday as usize % 7],
            tm.tm_mday,
            MESES[tm.tm_mon as usize % 12],
            tm.tm_hour,
            tm.tm_min
        )
    }
}
