//! Surveillance d'un dossier (inotify) : le descripteur devient lisible quand
//! un fichier y est créé, écrit, renommé ou supprimé. À brancher sur la boucle
//! d'événements ; appeler [`drain()`] à chaque réveil.

use std::{
    ffi::CString,
    os::{
        fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd},
        unix::ffi::OsStrExt,
    },
    path::Path,
};

pub fn dir(path: &Path) -> Option<OwnedFd> {
    std::fs::create_dir_all(path).ok()?;
    let c_dir = CString::new(path.as_os_str().as_bytes()).ok()?;
    unsafe {
        let fd = libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC);
        if fd < 0 {
            return None;
        }
        let fd = OwnedFd::from_raw_fd(fd);
        let mask = libc::IN_CREATE | libc::IN_CLOSE_WRITE | libc::IN_MOVED_TO | libc::IN_DELETE;
        (libc::inotify_add_watch(fd.as_raw_fd(), c_dir.as_ptr(), mask) >= 0).then_some(fd)
    }
}

/// Vide les événements en attente
pub fn drain(fd: BorrowedFd) {
    let mut buf = [0u8; 1024];
    while unsafe { libc::read(fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) } > 0 {}
}
