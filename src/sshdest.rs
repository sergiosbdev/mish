//! Destino de una invocación de `ssh` escrita a mano: salta opciones (y sus argumentos) como hace
//! getopt en OpenSSH y devuelve el host y si hay comando remoto.

/// Opciones de ssh que llevan argumento (`ssh -p 22 host`, `ssh -p22 host`).
const WITH_ARG: &[u8] = b"bcDEeFIiJLlmOopQRSWwB";

/// `ssh [opciones] destino [comando…]` → `(host, hay_comando_remoto)`. El host va sin `user@`
/// (y sin `ssh://` ni `:puerto` en la forma URI). `None` si no hay destino.
pub fn destination(args: &[String]) -> Option<(String, bool)> {
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a == "--" {
            i += 1;
            break;
        }
        if a.len() < 2 || !a.starts_with('-') {
            break;
        }
        // Grupo de opciones (`-vp22`, `-vp 22`): la primera que lleva argumento se come el resto.
        let bytes = a.as_bytes();
        for (k, c) in bytes.iter().enumerate().skip(1) {
            if WITH_ARG.contains(c) {
                if k + 1 == bytes.len() {
                    i += 1; // el argumento es la palabra siguiente
                }
                break;
            }
        }
        i += 1;
    }
    let dest = args.get(i)?;
    let host = clean_host(dest);
    if host.is_empty() {
        return None;
    }
    Some((host, args.len() > i + 1))
}

/// Quita `ssh://`, `user@` y `:puerto` (solo en la forma URI).
pub fn clean_host(dest: &str) -> String {
    let (uri, d) = match dest.strip_prefix("ssh://") {
        Some(r) => (true, r),
        None => (false, dest),
    };
    let d = d.rsplit_once('@').map_or(d, |(_, h)| h);
    let d = if uri { d.split_once(':').map_or(d, |(h, _)| h) } else { d };
    d.trim_end_matches('/').to_string()
}
