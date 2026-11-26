//! Temas de color. Todos los colores de mish salen de aquí.

use nu_ansi_term::{Color, Style};

#[derive(Clone, Copy)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    pub fn hex(s: &str) -> Option<Rgb> {
        let s = s.trim().trim_start_matches('#');
        if s.len() != 6 {
            return None;
        }
        let p = |i: usize| u8::from_str_radix(&s[i..i + 2], 16).ok();
        Some(Rgb(p(0)?, p(2)?, p(4)?))
    }

    /// Color ANSI: truecolor si el terminal lo soporta, si no el más cercano de 256.
    pub fn c(self) -> Color {
        if truecolor() {
            Color::Rgb(self.0, self.1, self.2)
        } else {
            let q = |v: u8| ((v as u16 * 5 + 127) / 255) as u8;
            Color::Fixed(16 + 36 * q(self.0) + 6 * q(self.1) + q(self.2))
        }
    }

    pub fn s(self) -> Style {
        Style::new().fg(self.c())
    }

    pub fn b(self) -> Style {
        self.s().bold()
    }
}

fn truecolor() -> bool {
    use std::sync::OnceLock;
    static T: OnceLock<bool> = OnceLock::new();
    *T.get_or_init(|| {
        std::env::var("COLORTERM").map(|v| v == "truecolor" || v == "24bit").unwrap_or(false)
    })
}

#[derive(Clone)]
pub struct Theme {
    pub name: &'static str,
    // prompt
    pub cwd: Rgb,
    pub git: Rgb,
    pub git_dirty: Rgb,
    pub git_sync: Rgb,
    pub ok: Rgb,
    pub err: Rgb,
    pub warn: Rgb,
    pub muted: Rgb,
    pub accent: Rgb,
    pub text: Rgb,
    // resaltado de sintaxis
    pub cmd: Rgb,
    pub cmd_bad: Rgb,
    pub builtin: Rgb,
    pub alias: Rgb,
    pub flag: Rgb,
    pub string: Rgb,
    pub var: Rgb,
    pub op: Rgb,
    pub path: Rgb,
    pub server: Rgb,
    // ls
    pub dir: Rgb,
    pub exec: Rgb,
    pub link: Rgb,
    pub file: Rgb,
    /// Fondos de pestaña para servidores (se elige uno por nombre si no hay color en la config).
    pub server_bgs: &'static [&'static str],
}

/// Cobre: el del icono. Chevron cobre (#f6ad74 → #ad4a1f), cursor crema (#fdf4e8)
/// y pizarra azul de fondo. El cobre marca lo tuyo (ruta, builtins, ❯), los fríos
/// derivados de la pizarra (azul, lavanda) dan la estructura. Todo ≥ 5:1 sobre el
/// fondo de "Clear Dark" (#191d27); `muted` ronda 5,3:1 para que siga leyéndose.
pub const COBRE: Theme = Theme {
    name: "cobre",
    cwd: Rgb(0xf6, 0xad, 0x74),       // cobre claro, el brillo del chevron
    git: Rgb(0xa9, 0xb6, 0xdc),       // pizarra aclarada
    git_dirty: Rgb(0xec, 0xc4, 0x6e), // latón
    git_sync: Rgb(0x7c, 0xc3, 0xd9),
    ok: Rgb(0x9d, 0xd0, 0x8c),        // salvia
    err: Rgb(0xf2, 0x67, 0x5f),       // coral: más rojo que el cobre para no confundirse
    warn: Rgb(0xec, 0xc4, 0x6e),
    muted: Rgb(0x85, 0x90, 0xa8),     // gris pizarra legible
    accent: Rgb(0xe8, 0x85, 0x4c),    // cobre medio
    text: Rgb(0xfd, 0xf4, 0xe8),      // crema del cursor
    cmd: Rgb(0x9d, 0xd0, 0x8c),
    cmd_bad: Rgb(0xf2, 0x67, 0x5f),
    builtin: Rgb(0xf6, 0xad, 0x74),
    alias: Rgb(0xe6, 0xa2, 0xc4),
    flag: Rgb(0x8a, 0xb8, 0xea),
    string: Rgb(0xe9, 0xcf, 0x96),
    var: Rgb(0xc0, 0xa8, 0xec),
    op: Rgb(0xe8, 0x85, 0x4c),
    path: Rgb(0xfd, 0xf4, 0xe8),
    server: Rgb(0x76, 0xd3, 0xbf),
    dir: Rgb(0x8a, 0xb8, 0xea),
    exec: Rgb(0x9d, 0xd0, 0x8c),
    link: Rgb(0xc0, 0xa8, 0xec),
    file: Rgb(0xd9, 0xd0, 0xc3),
    // Misma luminosidad (OKLCH L≈0,31), tonos repartidos: óxido, cobre, latón, musgo,
    // verdín, mar, índigo, ciruela, vino (cada 40°). Crema sobre ellos ≥ 10:1.
    server_bgs: &[
        "#4e211a", "#462900", "#333300", "#103a1c", "#003b38", "#00364d", "#232d54", "#3b254a", "#4a2035",
    ],
};

/// Pizarra: tiza de colores sobre encerado. Pasteles luminosos y fríos, ruta en tiza
/// amarilla y acento azul cielo. Para quien quiere menos calidez y más claridad.
pub const PIZARRA: Theme = Theme {
    name: "pizarra",
    cwd: Rgb(0xff, 0xe0, 0x8a),       // tiza amarilla
    git: Rgb(0xc9, 0xb2, 0xff),
    git_dirty: Rgb(0xff, 0xc2, 0x7a),
    git_sync: Rgb(0x8f, 0xd6, 0xff),
    ok: Rgb(0xa8, 0xe6, 0xa3),
    err: Rgb(0xff, 0x8a, 0x8a),
    warn: Rgb(0xff, 0xc2, 0x7a),
    muted: Rgb(0x8b, 0x96, 0xad),
    accent: Rgb(0x8f, 0xc8, 0xff),    // tiza azul
    text: Rgb(0xee, 0xf1, 0xf6),      // tiza blanca
    cmd: Rgb(0xa8, 0xe6, 0xa3),
    cmd_bad: Rgb(0xff, 0x8a, 0x8a),
    builtin: Rgb(0x8f, 0xc8, 0xff),
    alias: Rgb(0xff, 0xb3, 0xd9),
    flag: Rgb(0xc9, 0xb2, 0xff),
    string: Rgb(0xff, 0xe0, 0x8a),
    var: Rgb(0x8f, 0xe3, 0xe0),
    op: Rgb(0xff, 0x9f, 0x7a),
    path: Rgb(0xee, 0xf1, 0xf6),
    server: Rgb(0x8f, 0xe3, 0xe0),
    dir: Rgb(0x8f, 0xc8, 0xff),
    exec: Rgb(0xa8, 0xe6, 0xa3),
    link: Rgb(0x8f, 0xe3, 0xe0),
    file: Rgb(0xd5, 0xdb, 0xe6),
    // Encerados de colores (OKLCH L≈0,32): azul, verdín, verde, oliva, mostaza, teja, frambuesa, violeta, añil.
    server_bgs: &[
        "#0b3750", "#003c40", "#0a3d27", "#2f370c", "#442e01", "#4e2618", "#4d2331", "#412747", "#2d2e53",
    ],
};

/// Bosque: musgo, liquen y corteza. Verdes y ocres terrosos; la ruta en liquen,
/// los ejecutables en ámbar (resina) para que no se confundan con las carpetas verdes.
pub const BOSQUE: Theme = Theme {
    name: "bosque",
    cwd: Rgb(0xcf, 0xe0, 0x8f),       // liquen
    git: Rgb(0xd4, 0xa8, 0x74),       // corteza
    git_dirty: Rgb(0xe8, 0xc3, 0x5f),
    git_sync: Rgb(0x7f, 0xcd, 0xb8),
    ok: Rgb(0x8f, 0xd1, 0x8a),
    err: Rgb(0xe9, 0x82, 0x6a),       // hoja de otoño
    warn: Rgb(0xe8, 0xc3, 0x5f),
    muted: Rgb(0x86, 0x95, 0x7f),     // gris salvia
    accent: Rgb(0x7f, 0xbf, 0x6a),    // musgo
    text: Rgb(0xec, 0xf0, 0xe2),
    cmd: Rgb(0x8f, 0xd1, 0x8a),
    cmd_bad: Rgb(0xe9, 0x82, 0x6a),
    builtin: Rgb(0xcf, 0xe0, 0x8f),
    alias: Rgb(0xe0, 0xa8, 0xb8),     // brezo
    flag: Rgb(0x7f, 0xcd, 0xb8),
    string: Rgb(0xe8, 0xc3, 0x5f),
    var: Rgb(0xd4, 0xa8, 0x74),
    op: Rgb(0x7f, 0xbf, 0x6a),
    path: Rgb(0xec, 0xf0, 0xe2),
    server: Rgb(0x7f, 0xcd, 0xb8),
    dir: Rgb(0x9b, 0xd3, 0x7f),
    exec: Rgb(0xe8, 0xb8, 0x5a),      // resina
    link: Rgb(0x7f, 0xcd, 0xb8),
    file: Rgb(0xd4, 0xdc, 0xcb),
    // De la tierra (OKLCH L≈0,30): abeto, oliva, ocre, otoño, arcilla, brezo, endrina, lago, río.
    server_bgs: &[
        "#18361a", "#322f04", "#412805", "#472215", "#472027", "#3e223c", "#2e284a", "#003346", "#003732",
    ],
};

/// Papel: tinta sobre papel crema. TEMA CLARO: requiere un perfil de Terminal claro
/// (fondo ≈ #f7f3ec, p. ej. "Basic" o "Novel"); sobre "Clear Dark" no se lee bien.
/// Sus server_bgs son pasteles claros, pensados para el texto oscuro de ese perfil.
pub const PAPEL: Theme = Theme {
    name: "papel",
    cwd: Rgb(0xa4, 0x46, 0x1a),       // cobre oscuro
    git: Rgb(0x5b, 0x4a, 0x9e),
    git_dirty: Rgb(0x94, 0x62, 0x00),
    git_sync: Rgb(0x1f, 0x6d, 0x8a),
    ok: Rgb(0x2f, 0x7a, 0x2f),
    err: Rgb(0xc0, 0x32, 0x2b),
    warn: Rgb(0x94, 0x62, 0x00),
    muted: Rgb(0x6f, 0x66, 0x5d),
    accent: Rgb(0xb4, 0x52, 0x1f),
    text: Rgb(0x2a, 0x24, 0x20),      // tinta
    cmd: Rgb(0x2f, 0x7a, 0x2f),
    cmd_bad: Rgb(0xc0, 0x32, 0x2b),
    builtin: Rgb(0xa4, 0x46, 0x1a),
    alias: Rgb(0x9a, 0x3a, 0x7a),
    flag: Rgb(0x1f, 0x6d, 0x8a),
    string: Rgb(0x7a, 0x5a, 0x00),
    var: Rgb(0x5b, 0x4a, 0x9e),
    op: Rgb(0xb4, 0x52, 0x1f),
    path: Rgb(0x2a, 0x24, 0x20),
    server: Rgb(0x0f, 0x74, 0x68),
    dir: Rgb(0x24, 0x56, 0xa0),
    exec: Rgb(0x2f, 0x7a, 0x2f),
    link: Rgb(0x7a, 0x4a, 0xa0),
    file: Rgb(0x3d, 0x36, 0x31),
    server_bgs: &[
        "#ffdedb", "#fbe3cd", "#ebeacc", "#d8efd8", "#cbf1eb", "#cdeefc", "#dbe8ff", "#eee2fe", "#fddeee",
    ],
};

pub const THEMES: &[&Theme] = &[&COBRE, &PIZARRA, &BOSQUE, &PAPEL];

pub fn by_name(name: &str) -> Theme {
    THEMES.iter().find(|t| t.name == name).map(|t| (*t).clone()).unwrap_or_else(|| COBRE.clone())
}

/// Fondo de pestaña para un servidor: el de la config o uno estable elegido por su nombre.
pub fn server_bg(theme: &Theme, name: &str, cfg: Option<&String>) -> Rgb {
    if let Some(c) = cfg.and_then(|h| Rgb::hex(h)) {
        return c;
    }
    let h = name.bytes().fold(0u32, |a, b| a.wrapping_mul(31).wrapping_add(b as u32));
    Rgb::hex(theme.server_bgs[h as usize % theme.server_bgs.len()]).unwrap_or(Rgb(0x20, 0x10, 0x10))
}
