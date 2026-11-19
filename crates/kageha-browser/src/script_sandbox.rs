//! Apply the worker policy after program loading and before evaluating JS.
#[cfg(target_os = "macos")]
pub fn restrict() -> bool {
    use std::ffi::{c_char, c_int};
    #[link(name = "sandbox")]
    extern "C" {
        fn sandbox_init(profile: *const c_char, flags: u64, error: *mut *mut c_char) -> c_int;
        fn sandbox_free_error(error: *mut c_char);
    }
    let mut error = std::ptr::null_mut();
    // Stdio was opened by the trusted parent. No filesystem, network, or
    // child-process rules are granted to the interpreter.
    let policy = c"(version 1)(deny default)";
    let success = unsafe { sandbox_init(policy.as_ptr(), 0, &mut error) == 0 };
    if !error.is_null() {
        unsafe { sandbox_free_error(error) };
    }
    success
}

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
pub fn restrict() -> bool {
    use libc::{sock_filter, sock_fprog};
    let op = |code, jt, jf, k| sock_filter { code, jt, jf, k };
    #[cfg(target_arch = "x86_64")]
    let arch = 0xc000_003e;
    #[cfg(target_arch = "aarch64")]
    let arch = 0xc000_00b7;
    let mut rules = vec![
        op(0x20, 0, 0, 4), // seccomp_data.arch
        op(0x15, 1, 0, arch),
        op(0x06, 0, 0, 0x8000_0000), // SECCOMP_RET_KILL_PROCESS
        op(0x20, 0, 0, 0),           // seccomp_data.nr
    ];
    // The worker starts single-threaded. Its only I/O descriptors are stdio
    // pipes, with stderr attached to /dev/null by the trusted parent.
    for syscall in [
        libc::SYS_read,
        libc::SYS_write,
        libc::SYS_close,
        libc::SYS_fstat,
        libc::SYS_brk,
        libc::SYS_mmap,
        libc::SYS_mprotect,
        libc::SYS_munmap,
        libc::SYS_mremap,
        libc::SYS_madvise,
        libc::SYS_futex,
        libc::SYS_clock_gettime,
        libc::SYS_getrandom,
        libc::SYS_rt_sigaction,
        libc::SYS_rt_sigprocmask,
        libc::SYS_rt_sigreturn,
        libc::SYS_sigaltstack,
        libc::SYS_exit,
        libc::SYS_exit_group,
    ] {
        rules.push(op(0x15, 0, 1, syscall as u32));
        rules.push(op(0x06, 0, 0, 0x7fff_0000)); // SECCOMP_RET_ALLOW
    }
    rules.push(op(0x06, 0, 0, 0x0005_0000 | libc::EPERM as u32));
    let program = sock_fprog {
        len: rules.len() as u16,
        filter: rules.as_mut_ptr(),
    };
    unsafe {
        libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) == 0
            && libc::prctl(libc::PR_SET_SECCOMP, 2, &program) == 0
    }
}

#[cfg(not(any(
    target_os = "macos",
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    )
)))]
pub fn restrict() -> bool {
    false
}
