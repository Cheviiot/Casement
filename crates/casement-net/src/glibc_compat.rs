//! Lets binaries linked against a new glibc (the Fedora build container) start on ALT p11
//! (glibc 2.38).
//!
//! Rust's std refers weakly to `pidfd_spawnp` and `pidfd_getpid` (glibc 2.39). When some
//! dependency keeps process spawning in the binary (zbus does), the linker binds those
//! references to GLIBC_2.39 and the loader then refuses the binary on older systems.
//! These local definitions satisfy the references instead. std calls them only through
//! the unstable pidfd API (`CommandExt::create_pidfd`), which Casement never uses; normal
//! spawning goes through posix_spawn. scripts/check-glibc guards this.

#[no_mangle]
pub extern "C" fn pidfd_spawnp(
    _pidfd: *mut libc::c_int,
    _path: *const libc::c_char,
    _file_actions: *const libc::c_void,
    _attrp: *const libc::c_void,
    _argv: *const *mut libc::c_char,
    _envp: *const *mut libc::c_char,
) -> libc::c_int {
    libc::ENOSYS
}

#[no_mangle]
pub extern "C" fn pidfd_getpid(_pidfd: libc::c_int) -> libc::pid_t {
    unsafe { *libc::__errno_location() = libc::ENOSYS };
    -1
}
