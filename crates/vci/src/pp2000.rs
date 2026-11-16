//! Transport réel : VCI ACTIA (PP2000, VID 103A) par son pilote Windows.
//!
//! Même accès que `GestComm.dll` (classe `CUSBCommDriver`) :
//! - découverte par SetupAPI, interface `{75a835f4-d77d-4402-8585-c42247f25b76}` ;
//! - envoi : `DeviceIoControl(0x22200C)`, entrée `[Code cal][Code Com][dernier bloc][0][délai u32][taille u32][données]`,
//!   sortie de 8 octets `[statut général u16][statut de communication u16][taille reçue u32]` ;
//! - lecture : `DeviceIoControl(0x222010)`, entrée = la taille reçue (u32), sortie = en-tête de 20 octets + données.
//!
//! Le périphérique est ouvert puis refermé à chaque échange, comme le fait GestComm.

use crate::{Request, Response, Transport, VciError, BLOC_MAX, CAL_SYSTEME};
use std::ptr::{null, null_mut};
use windows_sys::core::GUID;
use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInterfaces, SetupDiGetClassDevsW, SetupDiGetDeviceInterfaceDetailW,
    DIGCF_DEVICEINTERFACE, DIGCF_PRESENT, SP_DEVICE_INTERFACE_DATA, SP_DEVICE_INTERFACE_DETAIL_DATA_W,
};
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING};
use windows_sys::Win32::System::IO::DeviceIoControl;

const INTERFACE: GUID = GUID {
    data1: 0x75a8_35f4,
    data2: 0xd77d,
    data3: 0x4402,
    data4: [0x85, 0x85, 0xc4, 0x22, 0x47, 0xf2, 0x5b, 0x76],
};
const IOCTL_ENVOI: u32 = 0x0022_200C;
const IOCTL_LECTURE: u32 = 0x0022_2010;
/// En-tête des données lues (écho de la requête, Code cal, Code Com, taille, état).
const ENTETE: usize = 20;

/// Délai (ms) d'une commande système absente de la table, comme GestComm.
pub const DELAI_SYSTEME_DEFAUT: u32 = 150;
/// GestComm multiplie le délai de `SUPER.PC` par 55 (constaté : `07` → 550 000, `fe` → 220 000, `12` → 22 000).
pub const FACTEUR_DELAI: u32 = 55;
/// Délai (ms) des commandes vers un calculateur. Valeur de Lexia inconnue : borne large, la VCI
/// rend d'elle-même l'état « pas de réponse » après quelques secondes.
pub const DELAI_ECU_DEFAUT: u32 = 30_000;

fn erreur(quoi: &str) -> VciError {
    VciError::Transport(format!("{quoi} (erreur Windows {})", unsafe { GetLastError() }))
}

/// Chemins des interfaces présentes. Ne fait qu'interroger Windows : rien n'est envoyé à la VCI.
pub fn detecter() -> Result<Vec<String>, VciError> {
    let mut chemins = Vec::new();
    // SAFETY : appels SetupAPI avec des tampons dont la taille est celle annoncée par l'API.
    unsafe {
        let set = SetupDiGetClassDevsW(&INTERFACE, null(), null_mut(), DIGCF_PRESENT | DIGCF_DEVICEINTERFACE);
        if set == INVALID_HANDLE_VALUE as isize {
            return Err(erreur("SetupDiGetClassDevs"));
        }
        for index in 0.. {
            let mut itf: SP_DEVICE_INTERFACE_DATA = std::mem::zeroed();
            itf.cbSize = size_of::<SP_DEVICE_INTERFACE_DATA>() as u32;
            if SetupDiEnumDeviceInterfaces(set, null(), &INTERFACE, index, &mut itf) == 0 {
                break;
            }
            let mut taille = 0u32;
            SetupDiGetDeviceInterfaceDetailW(set, &itf, null_mut(), 0, &mut taille, null_mut());
            if (taille as usize) < size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() {
                continue;
            }
            // tampon de u32 pour l'alignement de la structure
            let mut tampon = vec![0u32; (taille as usize).div_ceil(4)];
            let detail = tampon.as_mut_ptr().cast::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>();
            (*detail).cbSize = size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
            if SetupDiGetDeviceInterfaceDetailW(set, &itf, detail, taille, null_mut(), null_mut()) == 0 {
                continue;
            }
            let debut = std::ptr::addr_of!((*detail).DevicePath).cast::<u16>();
            let max = (taille as usize - 4) / 2;
            let n = (0..max).take_while(|&i| *debut.add(i) != 0).count();
            chemins.push(String::from_utf16_lossy(std::slice::from_raw_parts(debut, n)));
        }
        SetupDiDestroyDeviceInfoList(set);
    }
    Ok(chemins)
}

struct Poignee(HANDLE);

impl Drop for Poignee {
    fn drop(&mut self) {
        // SAFETY : poignée valide, fermée une seule fois.
        unsafe { CloseHandle(self.0) };
    }
}

pub struct Pp2000 {
    chemin: Vec<u16>,
    /// (Code Com, délai de `SUPER.PC`) ; la première ligne d'un code l'emporte.
    delais_systeme: Vec<(u8, u32)>,
    pub delai_ecu: u32,
}

impl Pp2000 {
    /// `chemin` : une valeur de [`detecter`]. `delais_systeme` : (code, délai) des lignes de `SUPER.PC`, dans l'ordre.
    pub fn new(chemin: &str, delais_systeme: Vec<(u8, u32)>) -> Self {
        Self {
            chemin: chemin.encode_utf16().chain(std::iter::once(0)).collect(),
            delais_systeme,
            delai_ecu: DELAI_ECU_DEFAUT,
        }
    }

    /// Délai transmis au pilote pour cette requête, en millisecondes.
    pub fn delai(&self, req: &Request) -> u32 {
        if req.code_cal != CAL_SYSTEME {
            return self.delai_ecu;
        }
        self.delais_systeme
            .iter()
            .find(|(c, _)| *c == req.code_com)
            .map_or(DELAI_SYSTEME_DEFAUT, |(_, d)| d.saturating_mul(FACTEUR_DELAI))
    }

    fn ioctl(&self, h: &Poignee, code: u32, entree: &[u8], sortie: &mut [u8]) -> Result<usize, VciError> {
        let mut recu = 0u32;
        // SAFETY : tampons valides pour les longueurs passées ; appel synchrone.
        let ok = unsafe {
            DeviceIoControl(
                h.0,
                code,
                entree.as_ptr().cast(),
                entree.len() as u32,
                sortie.as_mut_ptr().cast(),
                sortie.len() as u32,
                &mut recu,
                null_mut(),
            )
        };
        if ok == 0 {
            return Err(erreur(&format!("DeviceIoControl {code:#x}")));
        }
        Ok(recu as usize)
    }
}

/// Tampon d'entrée de l'IOCTL d'envoi.
fn tampon_envoi(req: &Request, delai: u32) -> Vec<u8> {
    let mut b = Vec::with_capacity(12 + req.data.len());
    b.extend_from_slice(&[req.code_cal, req.code_com, req.octet4, 0]);
    b.extend_from_slice(&delai.to_le_bytes());
    b.extend_from_slice(&(req.data.len() as u32).to_le_bytes());
    b.extend_from_slice(&req.data);
    b
}

impl Transport for Pp2000 {
    fn exchange(&mut self, req: &Request) -> Result<Response, VciError> {
        if req.data.len() > BLOC_MAX {
            return Err(VciError::Transport(format!(
                "message de {} o > {BLOC_MAX}",
                req.data.len()
            )));
        }
        // SAFETY : chemin terminé par un zéro.
        let h = unsafe {
            CreateFileW(
                self.chemin.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                null(),
                OPEN_EXISTING,
                0,
                null_mut(),
            )
        };
        if h == INVALID_HANDLE_VALUE {
            return Err(erreur("ouverture de la VCI"));
        }
        let h = Poignee(h);

        let mut statut = [0u8; 8];
        let n = self.ioctl(&h, IOCTL_ENVOI, &tampon_envoi(req, self.delai(req)), &mut statut)?;
        if n < 8 {
            return Err(VciError::Transport(format!("statut d'envoi de {n} o au lieu de 8")));
        }
        let general = u16::from_le_bytes([statut[0], statut[1]]);
        let comm = u16::from_le_bytes([statut[2], statut[3]]);
        let taille = u32::from_le_bytes([statut[4], statut[5], statut[6], statut[7]]) as usize;
        if general != 0 {
            let quoi = if matches!(general, 0x65 | 0x34..=0x37) {
                " : câble débranché ?"
            } else {
                ""
            };
            return Err(VciError::Transport(format!(
                "statut général {general:#x}, communication {comm:#x}{quoi}"
            )));
        }
        let mut data = Vec::new();
        if taille != 0 {
            let mut tampon = vec![0u8; taille];
            let n = self.ioctl(&h, IOCTL_LECTURE, &(taille as u32).to_le_bytes(), &mut tampon)?;
            tampon.truncate(n);
            if n > ENTETE {
                data = tampon.split_off(ENTETE);
            }
        }
        Ok(Response { etat: comm as u8, data })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tampon() {
        let b = tampon_envoi(&Request::new(0x8e, 0x07, [0xc2, 0x80, 0x01]), 30_000);
        assert_eq!(b, [0x8e, 0x07, 0xaa, 0, 0x30, 0x75, 0, 0, 3, 0, 0, 0, 0xc2, 0x80, 0x01]);
    }

    #[test]
    fn delais() {
        let p = Pp2000::new("x", vec![(0x07, 10_000), (0x12, 400), (0xfe, 4000), (0x07, 1)]);
        assert_eq!(p.delai(&Request::new(0, 0x07, [])), 550_000);
        assert_eq!(p.delai(&Request::new(0, 0x12, [])), 22_000);
        assert_eq!(p.delai(&Request::new(0, 0xfe, [])), 220_000);
        assert_eq!(p.delai(&Request::new(0, 0x55, [])), DELAI_SYSTEME_DEFAUT);
        assert_eq!(p.delai(&Request::new(0x8e, 0x07, [])), DELAI_ECU_DEFAUT);
    }
}
