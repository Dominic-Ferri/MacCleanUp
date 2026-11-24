use crate::scanner::Control;
use crate::scanner::{measure_all, Size};
use objc2::runtime::AnyObject;
use objc2::{msg_send, ClassType};
use objc2_foundation::{
    NSArray, NSData, NSDictionary, NSNumber, NSProcessInfo, NSPropertyListMutabilityOptions,
    NSPropertyListSerialization, NSString, NSURLVolumeAvailableCapacityForImportantUsageKey,
    NSURLVolumeAvailableCapacityKey, NSURL,
};
use std::collections::HashMap;
use std::ffi::CString;
use std::mem::{size_of, MaybeUninit};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// CPU time each running process had used at one moment.
#[derive(Clone, Debug)]
pub struct CpuSample {
    pub at: Instant,
    /// Process ID to name and total CPU time, user plus system.
    pub processes: HashMap<i32, (String, Duration)>,
}

/// Reads the CPU time of every process with `ps -A -o pid=,time=,comm=`.
/// `/bin/ps` can read system processes that MacCleanUp itself cannot. It
/// only reads: nothing is signalled, suspended or quit. MacCleanUp itself
/// is left out. An empty sample means `ps` could not run.
pub fn sample_processes() -> CpuSample {
    let at = Instant::now();
    let output = std::process::Command::new("/bin/ps")
        .args(["-A", "-o", "pid=,time=,comm="])
        .output();
    let processes = match output {
        Ok(output) if output.status.success() => parse_ps(
            &String::from_utf8_lossy(&output.stdout),
            std::process::id() as i32,
        ),
        _ => HashMap::new(),
    };
    CpuSample { at, processes }
}

/// Parses `pid time command` lines. The command is a path that may contain
/// spaces; the name is its last component.
fn parse_ps(listing: &str, own: i32) -> HashMap<i32, (String, Duration)> {
    listing
        .lines()
        .filter_map(|line| {
            let mut fields = line.trim_start().splitn(2, char::is_whitespace);
            let pid: i32 = fields.next()?.parse().ok()?;
            let rest = fields.next()?.trim_start();
            let (time, command) = rest.split_once(char::is_whitespace)?;
            let command = command.trim();
            let name = command.rsplit('/').next().filter(|name| !name.is_empty())?;
            let time = parse_cpu_time(time)?;
            (pid != own).then(|| (pid, (name.to_string(), time)))
        })
        .collect()
}

/// `ps` prints CPU time as `[[dd-]hh:]mm:ss.cc`, where minutes may run past 59.
fn parse_cpu_time(text: &str) -> Option<Duration> {
    let (days, clock) = match text.split_once('-') {
        Some((days, clock)) => (days.parse::<u64>().ok()?, clock),
        None => (0, text),
    };
    let parts: Vec<&str> = clock.split(':').collect();
    let (hours, minutes, seconds) = match parts.as_slice() {
        [minutes, seconds] => (0, minutes.parse::<u64>().ok()?, *seconds),
        [hours, minutes, seconds] => (
            hours.parse::<u64>().ok()?,
            minutes.parse::<u64>().ok()?,
            *seconds,
        ),
        _ => return None,
    };
    let seconds: f64 = seconds
        .parse()
        .ok()
        .filter(|value: &f64| value.is_finite() && *value >= 0.0)?;
    let whole = ((days * 24 + hours) * 60 + minutes) * 60;
    Some(Duration::from_secs(whole) + Duration::from_secs_f64(seconds))
}

/// A process that kept the CPU busy for the whole window.
#[derive(Clone, Debug, PartialEq)]
pub struct HighCpu {
    pub pid: i32,
    pub name: String,
    /// Average use over the window, in percent of one core.
    pub percent: f64,
}

/// Processes whose average use between two samples is at least
/// `threshold` percent of one core, busiest first. A process that exited
/// or whose ID was reused for another name is left out.
pub fn sustained_high_cpu(first: &CpuSample, last: &CpuSample, threshold: f64) -> Vec<HighCpu> {
    let wall = last.at.saturating_duration_since(first.at).as_secs_f64();
    if wall <= 0.0 {
        return Vec::new();
    }
    let mut busy: Vec<HighCpu> = last
        .processes
        .iter()
        .filter_map(|(pid, (name, after))| {
            let (old_name, before) = first.processes.get(pid)?;
            if old_name != name {
                return None;
            }
            let percent = after.saturating_sub(*before).as_secs_f64() / wall * 100.0;
            (percent >= threshold).then(|| HighCpu {
                pid: *pid,
                name: name.clone(),
                percent,
            })
        })
        .collect();
    busy.sort_by(|a, b| b.percent.total_cmp(&a.percent));
    busy
}

/// Read-only storage figures that take longer than the 2-second metrics.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StorageExtras {
    /// None when macOS does not let MacCleanUp read the Trash.
    pub trash: Option<Size>,
    /// Names of the Time Machine local snapshots on the startup disk. None
    /// when `tmutil` cannot list them.
    pub snapshots: Option<Vec<String>>,
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

/// Lists snapshots with `tmutil listlocalsnapshots /`, which needs no
/// administrator rights and changes nothing.
fn local_snapshots() -> Option<Vec<String>> {
    let output = std::process::Command::new("/usr/bin/tmutil")
        .args(["listlocalsnapshots", "/"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(snapshot_names(&String::from_utf8_lossy(&output.stdout)))
}

fn snapshot_names(listing: &str) -> Vec<String> {
    listing
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("com.apple.TimeMachine."))
        .map(str::to_string)
        .collect()
}

/// How the startup disk's APFS container is used, by volume role.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct VolumeUsage {
    /// The System, Preboot, Recovery, Update and other macOS volumes.
    pub macos: u64,
    /// Each macOS volume's name and size, largest first.
    pub macos_volumes: Vec<(String, u64)>,
    /// The Data volume: apps, your files, and everything else you add.
    pub data: u64,
    /// The VM volume: swap files and the sleep image.
    pub vm: u64,
    /// Space macOS frees by itself when it needs room, such as Time Machine
    /// local snapshots. None when macOS does not report it.
    pub purgeable: Option<u64>,
}

/// Reads per-volume use with `diskutil apfs list -plist`, which needs no
/// administrator rights and changes nothing, and the purgeable estimate from
/// the Data volume's resource values.
pub fn volume_usage() -> Option<VolumeUsage> {
    let container = root_container()?;
    let output = std::process::Command::new("/usr/sbin/diskutil")
        .args(["apfs", "list", "-plist"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let mut usage = container_usage(&output.stdout, &container)?;
    usage.purgeable = purgeable_bytes();
    Some(usage)
}

/// The APFS container of the startup volume, such as `disk3` for
/// `/dev/disk3s1s1`.
fn root_container() -> Option<String> {
    let path = CString::new("/").ok()?;
    let mut stats = MaybeUninit::<libc::statfs>::zeroed();
    if unsafe { libc::statfs(path.as_ptr(), stats.as_mut_ptr()) } != 0 {
        return None;
    }
    let stats = unsafe { stats.assume_init() };
    let device = unsafe { std::ffi::CStr::from_ptr(stats.f_mntfromname.as_ptr()) };
    container_of(device.to_str().ok()?)
}

fn container_of(device: &str) -> Option<String> {
    let name = device.strip_prefix("/dev/")?;
    let digits = name
        .strip_prefix("disk")?
        .chars()
        .take_while(char::is_ascii_digit)
        .count();
    (digits > 0).then(|| name[..4 + digits].to_string())
}

/// Returns `object` as a `T` when the runtime says it is one. objc2 cannot
/// downcast to generic collection types, so the class is checked here.
fn instance_of<T: ClassType>(object: &AnyObject) -> Option<&T> {
    let is_kind: bool = unsafe { msg_send![object, isKindOfClass: T::class()] };
    // SAFETY: the runtime confirmed that the object is a T or a subclass.
    is_kind.then(|| unsafe { &*(object as *const AnyObject).cast::<T>() })
}

type PlistDictionary = NSDictionary<NSString, AnyObject>;
type PlistArray = NSArray<AnyObject>;

/// Sums `CapacityInUse` by volume role for one container in the XML plist
/// that `diskutil apfs list -plist` prints.
fn container_usage(plist: &[u8], container: &str) -> Option<VolumeUsage> {
    objc2::rc::autoreleasepool(|_| {
        let data = NSData::with_bytes(plist);
        let root = unsafe {
            NSPropertyListSerialization::propertyListWithData_options_format_error(
                &data,
                NSPropertyListMutabilityOptions::Immutable,
                std::ptr::null_mut(),
            )
        }
        .ok()?;
        let get = |dictionary: &PlistDictionary, key: &str| {
            dictionary.objectForKey(&NSString::from_str(key))
        };
        let containers = get(instance_of::<PlistDictionary>(&root)?, "Containers")?;
        let containers = instance_of::<PlistArray>(&containers)?;
        let mut usage = VolumeUsage::default();
        let mut found = false;
        for entry in containers.iter() {
            let Some(entry) = instance_of::<PlistDictionary>(&entry) else {
                continue;
            };
            let reference = get(entry, "ContainerReference");
            let is_root = reference
                .as_deref()
                .and_then(instance_of::<NSString>)
                .is_some_and(|reference| reference.to_string() == container);
            if !is_root {
                continue;
            }
            found = true;
            let volumes = get(entry, "Volumes")?;
            for volume in instance_of::<PlistArray>(&volumes)?.iter() {
                let Some(volume) = instance_of::<PlistDictionary>(&volume) else {
                    continue;
                };
                let used = get(volume, "CapacityInUse")
                    .as_deref()
                    .and_then(instance_of::<NSNumber>)
                    .map_or(0, |number| number.unsignedLongLongValue());
                let roles: Vec<String> = get(volume, "Roles")
                    .as_deref()
                    .and_then(instance_of::<PlistArray>)
                    .map(|roles| {
                        roles
                            .iter()
                            .filter_map(|role| {
                                instance_of::<NSString>(&role).map(|r| r.to_string())
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                if roles.iter().any(|role| role == "Data") {
                    usage.data = usage.data.saturating_add(used);
                } else if roles.iter().any(|role| role == "VM") {
                    usage.vm = usage.vm.saturating_add(used);
                } else {
                    usage.macos = usage.macos.saturating_add(used);
                    let name = get(volume, "Name")
                        .as_deref()
                        .and_then(instance_of::<NSString>)
                        .map_or_else(|| roles.join(", "), |name| name.to_string());
                    usage.macos_volumes.push((name, used));
                }
            }
        }
        usage
            .macos_volumes
            .sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        found.then_some(usage)
    })
}

/// "Available for important use" counts space macOS can purge; plain
/// "available" does not. The difference is the purgeable space.
fn purgeable_bytes() -> Option<u64> {
    objc2::rc::autoreleasepool(|_| {
        let url = NSURL::fileURLWithPath(&NSString::from_str("/System/Volumes/Data"));
        let (important, plain) = unsafe {
            (
                NSURLVolumeAvailableCapacityForImportantUsageKey,
                NSURLVolumeAvailableCapacityKey,
            )
        };
        let values = url
            .resourceValuesForKeys_error(&NSArray::from_slice(&[important, plain]))
            .ok()?;
        let number = |key: &NSString| {
            values
                .objectForKey(key)
                .as_deref()
                .and_then(instance_of::<NSNumber>)
                .map(|number| number.longLongValue())
        };
        Some(number(important)?.saturating_sub(number(plain)?).max(0) as u64)
    })
}

/// Sizes of the parts of the Data volume that a list does not already
/// cover. Folders macOS does not let MacCleanUp read are None, never zero.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Breakdown {
    /// Desktop, Documents, Downloads, Movies, Music, Public, and Pictures
    /// without the Photos library (reading it raises a privacy prompt).
    pub files: Size,
    /// Xcode and simulator data, Rust and npm toolchains, and Homebrew.
    pub developer: Size,
    /// The folders measured for `developer`, so the caches list inside them
    /// can be taken out and not counted twice.
    pub developer_roots: Vec<PathBuf>,
    pub backups: Option<Size>,
    pub mail: Option<Size>,
    pub trash: Option<Size>,
    /// Every folder or file measured, with its part, so the Storage view can
    /// show what each part holds. Backups list one folder per device.
    pub entries: Vec<StorageEntry>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoragePart {
    Files,
    Developer,
    Backups,
    Mail,
    Trash,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StorageEntry {
    pub part: StoragePart,
    pub path: PathBuf,
    pub size: Size,
}

/// The children of `folder`, or None when it cannot be read.
fn children(folder: &Path) -> Option<Vec<PathBuf>> {
    let listing = std::fs::read_dir(folder).ok()?;
    Some(listing.flatten().map(|entry| entry.path()).collect())
}

/// Measures the breakdown in one parallel walk.
pub fn storage_breakdown(home: &Path, control: &Control) -> Breakdown {
    use StoragePart as Part;
    let exists = |path: &PathBuf| std::fs::symlink_metadata(path).is_ok();
    let readable = |path: &PathBuf| std::fs::read_dir(path).is_ok();
    let mut parts: Vec<(Part, PathBuf)> = Vec::new();
    for name in [
        "Desktop",
        "Documents",
        "Downloads",
        "Movies",
        "Music",
        "Public",
    ] {
        parts.push((Part::Files, home.join(name)));
    }
    if let Ok(pictures) = std::fs::read_dir(home.join("Pictures")) {
        for entry in pictures.flatten() {
            let path = entry.path();
            let photos = path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("photoslibrary"));
            if !photos {
                parts.push((Part::Files, path));
            }
        }
    }
    let developer_roots: Vec<PathBuf> = [
        home.join("Library/Developer"),
        home.join(".cargo"),
        home.join(".rustup"),
        home.join(".npm"),
        PathBuf::from("/opt/homebrew"),
    ]
    .into_iter()
    .filter(exists)
    .collect();
    parts.extend(
        developer_roots
            .iter()
            .map(|root| (Part::Developer, root.clone())),
    );
    let backups = children(&home.join("Library/Application Support/MobileSync/Backup"));
    let has_backups = backups.is_some();
    parts.extend(
        backups
            .into_iter()
            .flatten()
            .map(|path| (Part::Backups, path)),
    );
    let mail: Vec<PathBuf> = [home.join("Library/Mail"), home.join("Library/Messages")]
        .into_iter()
        .filter(readable)
        .collect();
    let has_mail = !mail.is_empty();
    parts.extend(mail.into_iter().map(|path| (Part::Mail, path)));
    let trash = children(&home.join(".Trash"));
    let has_trash = trash.is_some();
    parts.extend(trash.into_iter().flatten().map(|path| (Part::Trash, path)));
    parts.retain(|(_, path)| exists(path));

    let paths: Vec<PathBuf> = parts.iter().map(|(_, path)| path.clone()).collect();
    let sizes = measure_all(&paths, control).sizes;
    let mut entries: Vec<StorageEntry> = parts
        .into_iter()
        .zip(sizes)
        .map(|((part, path), size)| StorageEntry { part, path, size })
        .collect();
    entries.sort_by(|a, b| {
        b.size
            .bytes
            .cmp(&a.size.bytes)
            .then_with(|| a.path.cmp(&b.path))
    });
    let total = |wanted: Part| {
        entries.iter().filter(|entry| entry.part == wanted).fold(
            Size {
                bytes: 0,
                complete: true,
            },
            |sum, entry| Size {
                bytes: sum.bytes.saturating_add(entry.size.bytes),
                complete: sum.complete && entry.size.complete,
            },
        )
    };
    Breakdown {
        files: total(Part::Files),
        developer: total(Part::Developer),
        developer_roots,
        backups: has_backups.then(|| total(Part::Backups)),
        mail: has_mail.then(|| total(Part::Mail)),
        trash: has_trash.then(|| total(Part::Trash)),
        entries,
    }
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
    pub swap: Option<SwapStatus>,
}

/// Swap files on the VM volume, from `vm.swapusage`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SwapStatus {
    pub total: u64,
    pub used: u64,
}

fn swap_status() -> Option<SwapStatus> {
    let usage = sysctl_value::<libc::xsw_usage>("vm.swapusage").ok()?;
    Some(SwapStatus {
        total: usage.xsu_total,
        used: usage.xsu_used,
    })
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
        swap: swap_status(),
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
        let swap = status.swap.expect("swap metrics");
        assert!(swap.used <= swap.total);
    }

    #[test]
    fn sustained_cpu_is_the_average_over_the_window() {
        let start = Instant::now();
        let sample = |offset: u64, processes: &[(i32, &str, u64)]| CpuSample {
            at: start + Duration::from_secs(offset),
            processes: processes
                .iter()
                .map(|(pid, name, ms)| (*pid, (name.to_string(), Duration::from_millis(*ms))))
                .collect(),
        };
        let first = sample(
            0,
            &[
                (10, "busy", 0),
                (11, "idle", 0),
                (12, "old", 0),
                (13, "two", 0),
            ],
        );
        // Over 30 s: busy used 45 s of CPU (150%), idle 3 s (10%), two 27 s
        // (90%); pid 12 now belongs to another program; pid 14 is new.
        let last = sample(
            30,
            &[
                (10, "busy", 45_000),
                (11, "idle", 3_000),
                (12, "reused", 60_000),
                (13, "two", 27_000),
                (14, "new", 90_000),
            ],
        );
        let busy = sustained_high_cpu(&first, &last, 80.0);
        let names: Vec<_> = busy.iter().map(|high| high.name.as_str()).collect();
        assert_eq!(names, ["busy", "two"]);
        assert!((busy[0].percent - 150.0).abs() < 0.01);
        assert!((busy[1].percent - 90.0).abs() < 0.01);
        assert!(sustained_high_cpu(&first, &first, 80.0).is_empty());
    }

    #[test]
    fn live_process_samples_include_system_processes_but_not_this_one() {
        let sample = sample_processes();
        assert!(sample.processes.len() > 5, "{}", sample.processes.len());
        assert!(!sample.processes.contains_key(&(std::process::id() as i32)));
        let (name, _) = sample.processes.get(&1).expect("pid 1 is root's launchd");
        assert_eq!(name, "launchd");
        let parent = std::os::unix::process::parent_id() as i32;
        assert!(sample.processes.contains_key(&parent));
    }

    #[test]
    fn ps_cpu_times_and_lines_parse() {
        let secs = |text| parse_cpu_time(text).map(|time| time.as_secs_f64());
        assert_eq!(secs("0:01.50"), Some(1.5));
        assert_eq!(secs("7:38.42"), Some(458.42));
        assert_eq!(secs("125:00.00"), Some(7_500.0));
        assert_eq!(secs("1:02:03.04"), Some(3_723.04));
        assert_eq!(secs("2-01:00:00.00"), Some(176_400.0));
        for bad in ["", "abc", "1:2:3:4", "x:01.00", "1-", "0:-1.00"] {
            assert_eq!(parse_cpu_time(bad), None, "{bad:?}");
        }
        let listing = "    1   7:38.42 /sbin/launchd\n  300   0:01.00 /Applications/Google Chrome.app/Contents/MacOS/Google Chrome Helper (Renderer)\n  42   0:00.10 /usr/bin/self\nnot a line\n";
        let parsed = parse_ps(listing, 42);
        assert_eq!(parsed.len(), 2, "own pid and the bad line are left out");
        assert_eq!(parsed[&1].0, "launchd");
        assert_eq!(parsed[&300].0, "Google Chrome Helper (Renderer)");
        assert_eq!(parsed[&300].1, Duration::from_secs(1));
    }

    #[test]
    fn container_name_comes_from_the_startup_device() {
        assert_eq!(container_of("/dev/disk3s1s1").as_deref(), Some("disk3"));
        assert_eq!(container_of("/dev/disk12s5").as_deref(), Some("disk12"));
        assert_eq!(container_of("map auto_home"), None);
        assert_eq!(container_of("/dev/diskX"), None);
    }

    #[test]
    fn volume_roles_are_summed_for_the_startup_container_only() {
        let plist = br#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict><key>Containers</key><array>
<dict><key>ContainerReference</key><string>disk1</string><key>Volumes</key><array>
<dict><key>Roles</key><array><string>Preboot</string></array><key>CapacityInUse</key><integer>999</integer></dict>
</array></dict>
<dict><key>ContainerReference</key><string>disk3</string><key>Volumes</key><array>
<dict><key>Roles</key><array><string>System</string></array><key>CapacityInUse</key><integer>100</integer></dict>
<dict><key>Roles</key><array><string>Preboot</string></array><key>CapacityInUse</key><integer>20</integer></dict>
<dict><key>Roles</key><array><string>Data</string></array><key>CapacityInUse</key><integer>5000</integer></dict>
<dict><key>Roles</key><array><string>VM</string></array><key>CapacityInUse</key><integer>300</integer></dict>
</array></dict>
</array></dict></plist>"#;
        let usage = container_usage(plist, "disk3").unwrap();
        assert_eq!((usage.macos, usage.data, usage.vm), (120, 5000, 300));
        // Unnamed volumes fall back to their role.
        assert_eq!(
            usage.macos_volumes,
            [("System".to_string(), 100), ("Preboot".to_string(), 20)]
        );
        assert_eq!(container_usage(plist, "disk9"), None);
        assert_eq!(container_usage(b"not a plist", "disk3"), None);
    }

    #[test]
    fn live_volume_usage_fits_inside_the_startup_disk() {
        let usage = volume_usage().expect("diskutil reports the startup container");
        let disk = collect().disk.unwrap();
        assert!(usage.macos > 0 && usage.data > 0);
        assert!(usage.macos + usage.data + usage.vm <= disk.total);
        assert!(usage
            .purgeable
            .is_some_and(|purgeable| purgeable < disk.total));
    }

    #[test]
    fn breakdown_measures_your_files_and_leaves_unreadable_parts_unavailable() {
        let home = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/home");
        let breakdown = storage_breakdown(&home, &Control::default());
        let du = |path: &str| crate::scanner::tests::du_bytes(&home.join(path));
        assert_eq!(
            breakdown.files.bytes,
            du("Desktop") + du("Documents") + du("Downloads")
        );
        assert!(breakdown.files.complete);
        assert!(breakdown.developer_roots.contains(&home.join(".npm")));
        assert!(breakdown.developer.bytes >= du(".npm"));
        let desktop = breakdown
            .entries
            .iter()
            .find(|entry| entry.path == home.join("Desktop"))
            .expect("one entry per measured folder");
        assert_eq!(desktop.part, StoragePart::Files);
        assert_eq!(desktop.size.bytes, du("Desktop"));
        assert!(breakdown
            .entries
            .windows(2)
            .all(|pair| pair[0].size.bytes >= pair[1].size.bytes));
        assert_eq!(breakdown.backups, None, "no backup folder in the fixture");
        assert_eq!(breakdown.mail, None);
        assert_eq!(breakdown.trash, None);
    }

    #[test]
    fn snapshot_listing_counts_only_snapshot_lines() {
        let listing = "Snapshots for disk /:\ncom.apple.TimeMachine.2026-11-23-092637.local (dataless)\ncom.apple.TimeMachine.2026-11-23-102637.local\n";
        assert_eq!(
            snapshot_names(listing),
            [
                "com.apple.TimeMachine.2026-11-23-092637.local (dataless)",
                "com.apple.TimeMachine.2026-11-23-102637.local"
            ]
        );
        assert!(snapshot_names("Snapshots for disk /:\n").is_empty());
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
