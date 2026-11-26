//! Llamadas Rust → JS sin interpolar texto: el argumento siempre viaja como JSON.

use serde::Serialize;

/// Genera `window.F&&window.F(<json>)`. `func` debe ser un identificador fijo del código
/// (nunca datos externos). JSON es un literal JS válido salvo U+2028/U+2029, que se escapan.
pub fn call(func: &str, v: &impl Serialize) -> String {
    debug_assert!(
        !func.is_empty() && func.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$'),
        "js::call: nombre de función no válido"
    );
    let json = serde_json::to_string(v).unwrap_or_else(|_| "null".into());
    let json = json.replace('\u{2028}', "\\u2028").replace('\u{2029}', "\\u2029");
    format!("window.{func}&&window.{func}({json})")
}
