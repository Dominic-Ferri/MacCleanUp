use crate::scanner::Control;
use crate::scanner::{measure_all, Size};
use objc2_foundation::NSProcessInfo;
use std::ffi::CString;
use std::mem::{size_of, MaybeUninit};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Read-only storage figures that take longer than the 2-second metrics.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct StorageExtras {
    /// None when macOS does not let MacCleanUp read the Trash.
    pub trash: Option<Size>,
    /// Time Machine local snapshots on the startup disk. None when `tmutil`
    /// cannot list them.
    pub snapshots: Option<usize>,
}

pub fn storage_extras(home: &Path, control: &Control) -> StorageExtras {
    StorageExtras {
        trash: trash_size(&home.join(".Trash"), control),
        snapshots: local_snapshots(),
    }
}

fn trash_size(trash: &Path, control: &Control) -> Option<Size> {
    let children: Vec<PathBuf> = std::fs::read_dir(trash)
        .ok()?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<_, _>>()
        .ok()?;
    let sizes = measure_all(&children, control).sizes;
    Some(Size {
        bytes: sizes.iter().map(|size| size.bytes).sum(),
        complete: sizes.iter().all(|size| size.complete),
    })
}

/// Counts snapshots with `tmutil listlocalsnapshots /`, which needs no
/// administrator rights and changes nothing.
fn local_snapshots() -> Option<usize> {
    let output = std::process::Command::new("/usr/bin/tmutil")
        .args(["listlocalsnapshots", "/"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(count_snapshots(&String::from_utf8_lossy(&output.stdout)))
}

fn count_snapshots(listing: &str) -> usize {
    listing
        .lines()
        .filter(|line| line.trim_start().starts_with("com.apple.TimeMachine."))
        .count()
}

#[derive(Clone, Debug)]
pub struct SystemStatus {
    pub os_version: String,
    pub processor: String,
    pub logical_cpus: usize,
    pub memory: Option<MemoryStatus>,
    pub disk: Option<DiskStatus>,
    pub uptime_seconds: Option<u64>,
    pub load_average: Option<[f64; 3]>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MemoryStatus {
    pub total: u64,
    pub used: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiskStatus {
    pub total: u64,
    pub available: u64,
}

pub fn collect() -> SystemStatus {
    let version = NSProcessInfo::processInfo().operatingSystemVersion();
    SystemStatus {
        os_version: format!(
            "macOS {}.{}.{}",
            version.majorVersion, version.minorVersion, version.patchVersion
        ),
        processor: sysctl_string("machdep.cpu.brand_string")
            .or_else(|| sysctl_string("hw.model"))
            .unwrap_or_else(|| "Unknown processor".to_string()),
        logical_cpus: std::thread::available_parallelism()
            .map(|count| count.get())
            .unwrap_or(0),
        memory: memory_status(),
        disk: disk_status(),
        uptime_seconds: uptime(),
        load_average: load_average(),
    }
}

fn memory_status() -> Option<MemoryStatus> {
    let total = sysctl_value::<u64>("hw.memsize").ok()?;
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if page_size <= 0 {
        return None;
    }
    let mut stats = MaybeUninit::<libc::vm_statistics64>::zeroed();
    let mut count = libc::HOST_VM_INFO64_COUNT;
    #[allow(deprecated)]
    let result = unsafe {
        libc::host_statistics64(
            libc::mach_host_self(),
            libc::HOST_VM_INFO64,
            stats.as_mut_ptr().cast(),
            &mut count,
        )
    };
    if result != libc::KERN_SUCCESS {
        return None;
    }
    let stats = unsafe { stats.assume_init() };
    let used_pages = u64::from(stats.active_count)
        .saturating_add(u64::from(stats.wire_count))
        .saturating_add(u64::from(stats.compressor_page_count));
    Some(MemoryStatus {
        total,
        used: used_pages.saturating_mul(page_size as u64).min(total),
    })
}

fn disk_status() -> Option<DiskStatus> {
    let path = CString::new("/").unwrap();
    let mut stats = MaybeUninit::<libc::statfs>::zeroed();
    if unsafe { libc::statfs(path.as_ptr(), stats.as_mut_ptr()) } != 0 {
        return None;
    }
    let stats = unsafe { stats.assume_init() };
    let block_size = u64::from(stats.f_bsize);
    Some(DiskStatus {
        total: stats.f_blocks.saturating_mul(block_size),
        available: stats.f_bavail.saturating_mul(block_size),
    })
}

fn uptime() -> Option<u64> {
    let boot = sysctl_value::<libc::timeval>("kern.boottime").ok()?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs() as i64;
    Some(now.saturating_sub(boot.tv_sec).max(0) as u64)
}

fn load_average() -> Option<[f64; 3]> {
    let mut load = [0.0; 3];
    if unsafe { libc::getloadavg(load.as_mut_ptr(), 3) } == 3 {
        Some(load)
    } else {
        None
    }
}

fn sysctl_value<T: Copy>(name: &str) -> std::io::Result<T> {
    let name = CString::new(name).unwrap();
    let mut value = MaybeUninit::<T>::zeroed();
    let mut size = size_of::<T>();
    let result = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            value.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if result != 0 || size != size_of::<T>() {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { value.assume_init() })
}

fn sysctl_string(name: &str) -> Option<String> {
    let name = CString::new(name).ok()?;
    let mut size = 0;
    let result = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if result != 0 || size == 0 || size > 4096 {
        return None;
    }
    let mut buffer = vec![0_u8; size];
    if unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            buffer.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    } != 0
    {
        return None;
    }
    let end = buffer.iter().position(|byte| *byte == 0).unwrap_or(size);
    let value = String::from_utf8_lossy(&buffer[..end]).trim().to_string();
    (!value.is_empty()).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_status_has_real_metrics() {
        let status = collect();
        assert!(status.os_version.starts_with("macOS "));
        assert!(status.logical_cpus > 0);
        let memory = status.memory.expect("memory metrics");
        assert!(memory.total > 0);
        assert!(memory.used <= memory.total);
        let disk = status.disk.expect("disk metrics");
        assert!(disk.total > 0);
        assert!(disk.available <= disk.total);
        assert!(status.uptime_seconds.is_some());
        assert!(status.load_average.is_some());
    }

    #[test]
    fn snapshot_listing_counts_only_snapshot_lines() {
        let listing = "Snapshots for disk /:\ncom.apple.TimeMachine.2026-11-23-092637.local (dataless)\ncom.apple.TimeMachine.2026-11-23-102637.local\n";
        assert_eq!(count_snapshots(listing), 2);
        assert_eq!(count_snapshots("Snapshots for disk /:\n"), 0);
    }

    #[test]
    fn unreadable_or_missing_trash_is_unavailable_not_empty() {
        let missing = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/home/.Trash");
        assert_eq!(trash_size(&missing, &Control::default()), None);
        let folder = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/home/Library/Logs");
        let size = trash_size(&folder, &Control::default()).unwrap();
        assert!(size.complete && size.bytes > 0);
    }
}
