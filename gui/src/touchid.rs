//! Touch ID para la caja fuerte (vault.rs).
//!
//! Diseño (Mish.app va firmada ad-hoc, sin Team ID ni entitlements):
//!
//! - La clave derivada de la caja fuerte (no la contraseña maestra) se guarda en el llavero
//!   **de inicio de sesión** de macOS (llavero "clásico", basado en fichero) como contraseña
//!   genérica. Su ACL por defecto solo confía en el ejecutable que la creó: cualquier otro
//!   programa (incluido `security find-generic-password`) provoca un diálogo del sistema que
//!   pide la contraseña de macOS.
//! - No se puede usar el llavero "data protection" con `SecAccessControl`
//!   (`kSecAccessControlBiometryCurrentSet`): exige el entitlement `keychain-access-groups`
//!   y por tanto una firma con Team ID y perfil de aprovisionamiento. Sin él, `SecItemAdd`
//!   falla con `errSecMissingEntitlement`. Por eso la huella se exige con
//!   `LAContext.evaluatePolicy(.deviceOwnerAuthenticationWithBiometrics)` *antes* de leer el
//!   llavero: es una puerta que impone esta app, no el llavero (ver el informe de la rama).
//! - Para aproximar `BiometryCurrentSet`, junto a la clave se guarda el
//!   `evaluatedPolicyDomainState` del momento de activarlo: si cambian las huellas
//!   registradas, deja de coincidir y la clave se borra (hay que volver a la contraseña).
//!
//! Todo lo que bloquea (diálogo de Touch ID, diálogo del llavero) corre fuera del event loop.

use std::sync::atomic::AtomicBool;
use zeroize::Zeroizing;

/// Resultado de una operación de Touch ID o del llavero. Sin datos: nunca llevan secretos.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BioError {
    /// El usuario canceló, pulsó "Usar contraseña maestra" o la app canceló.
    Cancel,
    /// Huella no reconocida o Touch ID bloqueado por demasiados intentos.
    Failed,
    /// No hay Touch ID (o no hay huellas, o la tapa está cerrada).
    Unavailable,
    /// No hay clave guardada en el llavero.
    NotFound,
    /// El llavero denegó el acceso (p. ej. el diálogo del llavero se rechazó).
    Keychain,
}

/// Acceso al sistema: Touch ID + llavero. En tests se sustituye por un doble.
pub trait Bio: Send + Sync {
    /// ¿Se puede evaluar la política biométrica ahora mismo? Rápido, sin diálogos.
    fn available(&self) -> bool;
    /// Pide la huella (bloqueante). Devuelve el estado del dominio biométrico (cambia si
    /// se añaden o quitan huellas). `cancel` a true aborta el diálogo.
    fn authenticate(&self, reason: &str, cancel: &AtomicBool) -> Result<Vec<u8>, BioError>;
    /// ¿Hay una clave guardada? Solo atributos: no lee el secreto ni muestra diálogos.
    fn exists(&self) -> bool;
    fn store(&self, data: &[u8]) -> Result<(), BioError>;
    fn load(&self) -> Result<Zeroizing<Vec<u8>>, BioError>;
    /// Borra la clave guardada (si la hay). Bloqueante.
    fn delete(&self);
}

/// Sin Touch ID (otros sistemas o si se desactiva).
pub struct NoBio;

impl Bio for NoBio {
    fn available(&self) -> bool {
        false
    }
    fn authenticate(&self, _: &str, _: &AtomicBool) -> Result<Vec<u8>, BioError> {
        Err(BioError::Unavailable)
    }
    fn exists(&self) -> bool {
        false
    }
    fn store(&self, _: &[u8]) -> Result<(), BioError> {
        Err(BioError::Unavailable)
    }
    fn load(&self) -> Result<Zeroizing<Vec<u8>>, BioError> {
        Err(BioError::NotFound)
    }
    fn delete(&self) {}
}

#[cfg(target_os = "macos")]
pub use mac::System;

#[cfg(target_os = "macos")]
mod mac {
    use super::{Bio, BioError};
    use block2::RcBlock;
    use objc2::rc::autoreleasepool;
    use objc2::runtime::Bool;
    use objc2_foundation::{NSError, NSString};
    use objc2_local_authentication::{LAContext, LAError, LAPolicy};
    use security_framework::item::{ItemClass, ItemSearchOptions};
    use security_framework::passwords::{
        PasswordOptions, delete_generic_password, generic_password, set_generic_password_options,
    };
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    use std::time::Duration;
    use zeroize::Zeroizing;

    const SERVICE: &str = "dev.sergiosbdev.mish.vault";
    const ACCOUNT: &str = "touchid";
    const LABEL: &str = "Mish — caja fuerte (Touch ID)";
    const POLICY: LAPolicy = LAPolicy::DeviceOwnerAuthenticationWithBiometrics;

    const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;
    const ERR_SEC_USER_CANCELED: i32 = -128;

    /// Touch ID real (LocalAuthentication) + llavero de inicio de sesión.
    pub struct System;

    fn map_la(code: isize) -> BioError {
        let c = LAError(code);
        if [LAError::UserCancel, LAError::UserFallback, LAError::SystemCancel, LAError::AppCancel].contains(&c) {
            BioError::Cancel
        } else if [LAError::AuthenticationFailed, LAError::BiometryLockout].contains(&c) {
            BioError::Failed
        } else {
            BioError::Unavailable
        }
    }

    fn map_keychain(code: i32) -> BioError {
        match code {
            ERR_SEC_ITEM_NOT_FOUND => BioError::NotFound,
            ERR_SEC_USER_CANCELED => BioError::Cancel,
            _ => BioError::Keychain,
        }
    }

    impl Bio for System {
        fn available(&self) -> bool {
            autoreleasepool(|_| {
                // SAFETY: LAContext se crea y usa en este hilo; canEvaluatePolicy no bloquea.
                let ctx = unsafe { LAContext::new() };
                unsafe { ctx.canEvaluatePolicy_error(POLICY) }.is_ok()
            })
        }

        fn authenticate(&self, reason: &str, cancel: &AtomicBool) -> Result<Vec<u8>, BioError> {
            autoreleasepool(|_| {
                // SAFETY: llamadas Objective-C con objetos vivos durante todo el bloque; el
                // contexto se mantiene (strong) hasta recibir la respuesta.
                let ctx = unsafe { LAContext::new() };
                unsafe {
                    ctx.setLocalizedFallbackTitle(Some(&NSString::from_str("Usar contraseña maestra")));
                    ctx.setLocalizedCancelTitle(Some(&NSString::from_str("Cancelar")));
                }
                let (tx, rx) = mpsc::channel::<Result<(), isize>>();
                // La respuesta llega en una cola privada del framework: solo se reenvía.
                let reply = RcBlock::new(move |ok: Bool, err: *mut NSError| {
                    let r = if ok.as_bool() {
                        Ok(())
                    } else if err.is_null() {
                        Err(LAError::AuthenticationFailed.0)
                    } else {
                        // SAFETY: el framework pasa un NSError válido durante la llamada.
                        Err(unsafe { (*err).code() })
                    };
                    let _ = tx.send(r);
                });
                let reason = NSString::from_str(reason);
                unsafe { ctx.evaluatePolicy_localizedReason_reply(POLICY, &reason, &reply) };

                let mut invalidated = false;
                let res = loop {
                    match rx.recv_timeout(Duration::from_millis(100)) {
                        Ok(r) => break r,
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            if !invalidated && cancel.load(Ordering::SeqCst) {
                                // Cierra el diálogo; la respuesta llega con LAErrorAppCancel.
                                unsafe { ctx.invalidate() };
                                invalidated = true;
                            }
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => break Err(LAError::AppCancel.0),
                    }
                };
                res.map_err(map_la)?;
                #[allow(deprecated)] // domainState.biometry.stateHash es macOS 15+; mínimo 13.
                let state = unsafe { ctx.evaluatedPolicyDomainState() }.map(|d| d.to_vec()).unwrap_or_default();
                Ok(state)
            })
        }

        fn exists(&self) -> bool {
            ItemSearchOptions::new()
                .class(ItemClass::generic_password())
                .service(SERVICE)
                .account(ACCOUNT)
                .load_attributes(true)
                .limit(1)
                .search()
                .is_ok_and(|r| !r.is_empty())
        }

        fn store(&self, data: &[u8]) -> Result<(), BioError> {
            // Borrar y crear (no actualizar): el elemento nuevo lleva la ACL del ejecutable
            // actual, que tras una actualización de la app es otro (firma ad-hoc).
            let _ = delete_generic_password(SERVICE, ACCOUNT);
            let mut o = PasswordOptions::new_generic_password(SERVICE, ACCOUNT);
            o.set_label(LABEL);
            o.set_comment("Clave de la caja fuerte de Mish. Borrarla solo desactiva Touch ID.");
            set_generic_password_options(data, o).map_err(|e| map_keychain(e.code()))
        }

        fn load(&self) -> Result<Zeroizing<Vec<u8>>, BioError> {
            generic_password(PasswordOptions::new_generic_password(SERVICE, ACCOUNT))
                .map(Zeroizing::new)
                .map_err(|e| map_keychain(e.code()))
        }

        fn delete(&self) {
            let _ = delete_generic_password(SERVICE, ACCOUNT);
        }
    }

}

/// El Touch ID del sistema, o ninguno fuera de macOS.
pub fn system() -> std::sync::Arc<dyn Bio> {
    #[cfg(target_os = "macos")]
    {
        std::sync::Arc::new(System)
    }
    #[cfg(not(target_os = "macos"))]
    {
        std::sync::Arc::new(NoBio)
    }
}
