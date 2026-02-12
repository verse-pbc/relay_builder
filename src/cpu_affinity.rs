//! CPU affinity helpers with Linux implementation and no-op fallbacks elsewhere.
//!
//! Usage:
//!   `affinity::pin_current_thread_to(core_index)`?;
//!   `affinity::allow_all_but_current_thread`(&[`excluded_core`])?;
//!
//! Notes:
//! - Affinity is set per-thread. New threads inherit the creator's mask at creation time.
//! - For very large CPU counts beyond `CPU_SETSIZE`, consider cpuset cgroups or `CPU_ALLOC` APIs.

use std::io;
#[cfg(target_os = "linux")]
use std::mem;

/// Pin the current thread to a specific CPU
///
/// # Errors
/// Returns an error if the system call to set CPU affinity fails
#[cfg(target_os = "linux")]
pub fn pin_current_thread_to(cpu: usize) -> io::Result<()> {
    // SAFETY: cpu_set_t is a plain-data type safe to zero-init. CPU_ZERO/CPU_SET
    // only write within the set, and sched_setaffinity with pid=0 targets the
    // current thread. We check the return value for errors.
    unsafe {
        let mut set: libc::cpu_set_t = mem::zeroed();
        libc::CPU_ZERO(&mut set);
        libc::CPU_SET(cpu, &mut set);
        if libc::sched_setaffinity(0, mem::size_of::<libc::cpu_set_t>(), &raw const set) != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    tracing::info!("Pinned thread to CPU {}", cpu);
    Ok(())
}

/// Pin the current thread to a specific CPU (no-op on non-Linux)
#[cfg(not(target_os = "linux"))]
pub fn pin_current_thread_to(cpu: usize) -> io::Result<()> {
    // No-op on non-Linux; consider using the `core_affinity` crate or platform APIs.
    tracing::debug!(
        "CPU pinning not supported on this platform (would pin to CPU {})",
        cpu
    );
    Ok(())
}

/// Allow the current thread to run on all CPUs except the excluded ones
///
/// # Errors
/// Returns an error if the system call to set CPU affinity fails
#[cfg(target_os = "linux")]
pub fn allow_all_but_current_thread(excluded: &[usize]) -> io::Result<()> {
    let cpu_count = num_cpus::get();
    // SAFETY: Same invariants as pin_current_thread_to - cpu_set_t is safe to
    // zero-init, CPU_SET only writes within bounds, and sched_setaffinity with
    // pid=0 targets the current thread.
    unsafe {
        let mut set: libc::cpu_set_t = mem::zeroed();
        libc::CPU_ZERO(&mut set);
        for cpu in 0..cpu_count {
            if !excluded.contains(&cpu) {
                libc::CPU_SET(cpu, &mut set);
            }
        }
        if libc::sched_setaffinity(0, mem::size_of::<libc::cpu_set_t>(), &raw const set) != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    tracing::info!("Set thread affinity to all CPUs except {:?}", excluded);
    Ok(())
}

/// Allow the current thread to run on all CPUs except the excluded ones (no-op on non-Linux)
#[cfg(not(target_os = "linux"))]
pub fn allow_all_but_current_thread(excluded: &[usize]) -> io::Result<()> {
    // No-op on non-Linux
    tracing::debug!(
        "CPU affinity not supported on this platform (would exclude CPUs {:?})",
        excluded
    );
    Ok(())
}
