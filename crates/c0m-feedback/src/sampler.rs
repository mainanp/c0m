//! The sensor: samples where a running program spends its cycles.
//!
//! Uses the Linux `perf_event_open` syscall in SAMPLING mode: every
//! `period` cycles the kernel stores the instruction pointer (IP) in a
//! ring buffer. After the program exits we read all the IPs.
//!
//! The target is started through a tiny `sh` wrapper that waits for a
//! newline and then `exec`s the program. That gives us its pid before it
//! runs; the event is created disabled with `enable_on_exec`, so only the
//! real program (not the shell) is sampled.

use std::io::{self, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{fence, Ordering};

const PERF_TYPE_HARDWARE: u32 = 0;
const PERF_TYPE_SOFTWARE: u32 = 1;
const PERF_COUNT_HW_CPU_CYCLES: u64 = 0;
const PERF_COUNT_SW_CPU_CLOCK: u64 = 0;
const PERF_SAMPLE_IP: u64 = 1;
const PERF_RECORD_SAMPLE: u32 = 9;
const PERF_FLAG_FD_CLOEXEC: libc::c_ulong = 1 << 3;

// bits of perf_event_attr's flag word
const FLAG_DISABLED: u64 = 1 << 0;
const FLAG_EXCLUDE_KERNEL: u64 = 1 << 5;
const FLAG_EXCLUDE_HV: u64 = 1 << 6;
const FLAG_ENABLE_ON_EXEC: u64 = 1 << 12;

/// Ring buffer data pages (power of two). 64 pages = 256 KiB, about 16k
/// samples per run, and within the default per-user mlock allowance.
const DATA_PAGES: usize = 64;
/// Offset of `data_head` inside the first (metadata) page.
const DATA_HEAD_OFFSET: usize = 1024;

/// If hardware cycle counters are unavailable (VMs, WSL) we fall back to
/// the software cpu-clock event, whose period is in nanoseconds.
pub const FALLBACK_CLOCK_PERIOD_NS: u64 = 100_000;

/// The first 64 bytes of the kernel's perf_event_attr (ABI "VER0").
#[repr(C)]
struct PerfEventAttr {
    type_: u32,
    size: u32,
    config: u64,
    sample_period: u64,
    sample_type: u64,
    read_format: u64,
    flags: u64,
    wakeup_events: u32,
    bp_type: u32,
    config1: u64,
}

fn open_event(pid: i32, type_: u32, config: u64, period: u64) -> io::Result<i32> {
    let attr = PerfEventAttr {
        type_,
        size: std::mem::size_of::<PerfEventAttr>() as u32,
        config,
        sample_period: period,
        sample_type: PERF_SAMPLE_IP,
        read_format: 0,
        flags: FLAG_DISABLED | FLAG_EXCLUDE_KERNEL | FLAG_EXCLUDE_HV | FLAG_ENABLE_ON_EXEC,
        wakeup_events: 0,
        bp_type: 0,
        config1: 0,
    };
    let fd = unsafe {
        libc::syscall(
            libc::SYS_perf_event_open,
            &attr as *const PerfEventAttr,
            pid as libc::c_long,
            -1 as libc::c_long, // any cpu
            -1 as libc::c_long, // no group
            PERF_FLAG_FD_CLOEXEC,
        )
    };
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(fd as i32)
    }
}

/// An open event plus its mapped ring buffer. Cleans up on drop.
struct Ring {
    fd: i32,
    base: *mut libc::c_void,
    len: usize,
    page: usize,
}

impl Drop for Ring {
    fn drop(&mut self) {
        unsafe {
            libc::munmap(self.base, self.len);
            libc::close(self.fd);
        }
    }
}

fn explain(e: io::Error) -> io::Error {
    io::Error::new(
        e.kind(),
        format!(
            "perf_event_open failed: {e}. Check /proc/sys/kernel/perf_event_paranoid \
             (it must be <= 2 to sample your own processes; see the README)."
        ),
    )
}

fn open_ring(pid: i32, period: u64) -> io::Result<Ring> {
    let fd = match open_event(pid, PERF_TYPE_HARDWARE, PERF_COUNT_HW_CPU_CYCLES, period) {
        Ok(fd) => fd,
        Err(first) => {
            match open_event(pid, PERF_TYPE_SOFTWARE, PERF_COUNT_SW_CPU_CLOCK, FALLBACK_CLOCK_PERIOD_NS) {
                Ok(fd) => fd,
                Err(_) => return Err(explain(first)),
            }
        }
    };
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
    let len = (1 + DATA_PAGES) * page;
    let base = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            len,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            fd,
            0,
        )
    };
    if base == libc::MAP_FAILED {
        let e = io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(explain(e));
    }
    Ok(Ring { fd, base, len, page })
}

impl Ring {
    /// Reads every sampled IP. The buffer is not overwritten when full, so
    /// a nearly full buffer means samples were probably lost: refuse it.
    fn read_ips(&self) -> io::Result<Vec<u64>> {
        let head = unsafe {
            std::ptr::read_volatile((self.base as *const u8).add(DATA_HEAD_OFFSET) as *const u64)
        };
        fence(Ordering::Acquire);
        let data_size = (DATA_PAGES * self.page) as u64;
        if head + 32 > data_size {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "sample buffer filled up (samples lost): raise --period or lower the workload",
            ));
        }
        let data = unsafe {
            std::slice::from_raw_parts((self.base as *const u8).add(self.page), head as usize)
        };
        let mut ips = Vec::new();
        let mut pos = 0usize;
        while pos + 8 <= data.len() {
            // perf_event_header: type u32 | misc u16 | size u16
            let typ = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap());
            let size = u16::from_le_bytes(data[pos + 6..pos + 8].try_into().unwrap()) as usize;
            if size < 8 || pos + size > data.len() {
                break;
            }
            if typ == PERF_RECORD_SAMPLE && size >= 16 {
                ips.push(u64::from_le_bytes(data[pos + 8..pos + 16].try_into().unwrap()));
            }
            pos += size;
        }
        Ok(ips)
    }
}

fn profile_once(exe: &Path, args: &[String], period: u64) -> io::Result<Vec<u64>> {
    let mut child = Command::new("sh")
        .arg("-c")
        .arg("read dummy; exec \"$0\" \"$@\"")
        .arg(exe)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()?;

    let ring = match open_ring(child.id() as i32, period) {
        Ok(r) => r,
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(e);
        }
    };

    // release the shell: it execs the target, which switches the event on
    child.stdin.take().expect("stdin is piped").write_all(b"\n")?;
    let status = child.wait()?;
    if !status.success() {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            format!("profiled program exited with {status}"),
        ));
    }
    ring.read_ips()
}

/// Runs `exe` `runs` times under the sampler and returns every sampled IP.
/// `period` = cycles between samples. Several runs are pooled because
/// short programs yield too few samples from a single run.
pub fn profile(exe: &Path, args: &[String], runs: u32, period: u64) -> io::Result<Vec<u64>> {
    let exe = exe.canonicalize()?;
    let mut all = Vec::new();
    for _ in 0..runs {
        all.extend(profile_once(&exe, args, period)?);
    }
    Ok(all)
}
