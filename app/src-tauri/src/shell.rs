//! Explorer integration on Windows: the .nax file icon and the right-click menu.
//!
//! Everything is written under HKEY_CURRENT_USER\Software\Classes, so it needs
//! no admin rights, only affects the current user, and `unregister` removes it.

use serde::{Deserialize, Serialize};

/// Menu texts in the user's language (sent by the UI).
#[derive(Deserialize)]
pub struct Labels {
    pub file_type: String,
    pub open: String,
    pub extract_here: String,
    pub extract_to: String,
    pub test: String,
    pub menu: String,
    pub add: String,
    pub add_quick: String,
}

#[derive(Serialize)]
pub struct Status {
    /// The menu and file type are registered.
    pub registered: bool,
    /// ... and they point at this exe.
    pub current: bool,
}

#[cfg(windows)]
mod imp {
    use super::{Labels, Status};
    use std::io;
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    const PROGID: &str = "Nanarchive.nax";
    /// Other archive types that get "Open / Extract with Nanarchive" entries.
    const OTHER: &[&str] = &[".zip", ".jar"];
    /// Name of our cascade menu and prefix of our verbs.
    const KEY: &str = "Nanarchive";

    fn classes() -> io::Result<RegKey> {
        Ok(RegKey::predef(HKEY_CURRENT_USER).create_subkey("Software\\Classes")?.0)
    }

    fn exe() -> io::Result<String> {
        Ok(std::env::current_exe()?.to_string_lossy().into_owned())
    }

    /// One menu entry: `<parent>\<name>` with a label, icon and command.
    fn verb(parent: &RegKey, name: &str, label: &str, icon: &str, cmd: &str) -> io::Result<()> {
        let (k, _) = parent.create_subkey(name)?;
        k.set_value("MUIVerb", &label)?;
        k.set_value("Icon", &icon)?;
        // Keep the entry when many files are selected (Explorer hides it after 15 otherwise).
        k.set_value("MultiSelectModel", &"Player")?;
        k.create_subkey("command")?.0.set_value("", &cmd)?;
        Ok(())
    }

    pub fn register(l: &Labels) -> io::Result<()> {
        let c = classes()?;
        let exe = exe()?;
        let icon = format!("\"{exe}\",0");
        let cmd = |flag: &str| format!("\"{exe}\" {flag} \"%1\"");

        // .nax files: our icon, double-click opens, extract / test from the menu.
        c.create_subkey(".nax")?.0.set_value("", &PROGID)?;
        let (p, _) = c.create_subkey(PROGID)?;
        p.set_value("", &l.file_type)?;
        p.create_subkey("DefaultIcon")?.0.set_value("", &icon)?;
        let (sh, _) = p.create_subkey("shell")?;
        sh.set_value("", &"open")?;
        verb(&sh, "open", &l.open, &icon, &format!("\"{exe}\" \"%1\""))?;
        verb(&sh, "nax.extracthere", &l.extract_here, &icon, &cmd("--extract-here"))?;
        verb(&sh, "nax.extractto", &l.extract_to, &icon, &cmd("--extract-to"))?;
        verb(&sh, "nax.test", &l.test, &icon, &cmd("--test"))?;

        // ZIP and friends keep their own program; we only add menu entries.
        for ext in OTHER {
            let (sh, _) = c.create_subkey(format!("SystemFileAssociations\\{ext}\\shell"))?;
            verb(&sh, &format!("{KEY}.open"), &l.open, &icon, &format!("\"{exe}\" \"%1\""))?;
            verb(&sh, &format!("{KEY}.extracthere"), &l.extract_here, &icon, &cmd("--extract-here"))?;
            verb(&sh, &format!("{KEY}.extractto"), &l.extract_to, &icon, &cmd("--extract-to"))?;
        }

        // Any file or folder: "Nanarchive >" with Add to archive... / Compress to .nax.
        for base in ["*\\shell", "Directory\\shell"] {
            let (m, _) = c.create_subkey(format!("{base}\\{KEY}"))?;
            m.set_value("MUIVerb", &l.menu)?;
            m.set_value("Icon", &icon)?;
            m.set_value("SubCommands", &"")?;
            m.set_value("MultiSelectModel", &"Player")?;
            let (sub, _) = m.create_subkey("shell")?;
            verb(&sub, "1add", &l.add, &icon, &cmd("--add"))?;
            verb(&sub, "2quick", &l.add_quick, &icon, &cmd("--add-quick"))?;
        }
        notify();
        Ok(())
    }

    fn delete(c: &RegKey, path: &str) {
        let _ = c.delete_subkey_all(path);
    }

    pub fn unregister() -> io::Result<()> {
        let c = classes()?;
        if let Ok(k) = c.open_subkey(".nax") {
            if k.get_value::<String, _>("").map_or(false, |v| v == PROGID) {
                delete(&c, ".nax");
            }
        }
        delete(&c, PROGID);
        for ext in OTHER {
            for v in ["open", "extracthere", "extractto"] {
                delete(&c, &format!("SystemFileAssociations\\{ext}\\shell\\{KEY}.{v}"));
            }
        }
        delete(&c, &format!("*\\shell\\{KEY}"));
        delete(&c, &format!("Directory\\shell\\{KEY}"));
        notify();
        Ok(())
    }

    pub fn status() -> Status {
        let read = || -> io::Result<String> {
            let k = RegKey::predef(HKEY_CURRENT_USER).open_subkey(format!("Software\\Classes\\{PROGID}\\shell\\open\\command"))?;
            k.get_value("")
        };
        match read() {
            Ok(cmd) => {
                let current = exe().map_or(false, |e| cmd.to_lowercase().contains(&e.to_lowercase()));
                Status { registered: true, current }
            }
            Err(_) => Status { registered: false, current: false },
        }
    }

    /// Tells Explorer to reload icons and menus.
    fn notify() {
        use windows_sys::Win32::UI::Shell::{SHChangeNotify, SHCNE_ASSOCCHANGED, SHCNF_IDLIST};
        unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED as _, SHCNF_IDLIST, std::ptr::null(), std::ptr::null()) };
    }
}

#[cfg(not(windows))]
mod imp {
    use super::{Labels, Status};
    pub fn register(_: &Labels) -> std::io::Result<()> {
        Err(std::io::Error::other("Explorer integration is only on Windows"))
    }
    pub fn unregister() -> std::io::Result<()> {
        Ok(())
    }
    pub fn status() -> Status {
        Status { registered: false, current: false }
    }
}

pub use imp::{register, status, unregister};
