use crate::model::{AppItem, CleanCategory, CleanItem, DiskItem, ItemIdentity, RelatedPath};
use objc2::runtime::AnyObject;
use objc2_foundation::{ns_string, NSDictionary, NSString};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::{Duration, SystemTime};

#[derive(Clone, Debug)]
pub struct ScanResult<T> {
    pub items: Vec<T>,
    pub errors: Vec<String>,
}

impl<T> Default for ScanResult<T> {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            errors: Vec::new(),
        }
    }
}

/// Space an item takes on disk: allocated blocks, each hard-linked file
/// counted once. `complete` is false when part of it could not be read or
/// the walk was stopped.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Size {
    pub bytes: u64,
    pub complete: bool,
}

/// Folder sizes from earlier walks, keyed by path. Only folders with many
/// entries are kept, because small ones are quick to measure again.
pub type SizeCache = HashMap<PathBuf, Size>;

/// A disk map listing plus new cache entries; fails only when the folder
/// itself cannot be read.
pub type DiskScan = std::io::Result<(ScanResult<DiskItem>, Vec<(PathBuf, Size)>)>;

/// Folders with at least this many entries below them go into the cache.
const CACHE_MIN_ENTRIES: u64 = 500;

/// Large-file search defaults: at least 1 GB on disk, unchanged for 180 days.
pub const LARGE_FILE_BYTES: u64 = 1_000_000_000;
pub const LARGE_FILE_AGE: Duration = Duration::from_secs(180 * 86_400);

fn allocated(metadata: &fs::Metadata) -> u64 {
    // st_blocks counts 512-byte units on macOS, whatever the volume block size.
    metadata.blocks().saturating_mul(512)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Shared by a scan and the window: a stop flag the walk checks, and counts
/// the window turns into a progress bar.
#[derive(Debug, Default)]
pub struct Control {
    stopped: AtomicBool,
    checked: AtomicU64,
    done: AtomicUsize,
    total: AtomicUsize,
}

/// A snapshot of a scan's progress. `total` grows as each phase starts.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Progress {
    /// Top-level items fully measured or searched.
    pub done: usize,
    /// Top-level items started.
    pub total: usize,
    /// Files and folders read so far.
    pub checked: u64,
}

impl Progress {
    /// Share of the started items that are finished, when any have started.
    pub fn fraction(self) -> Option<f32> {
        (self.total > 0).then(|| (self.done as f32 / self.total as f32).min(1.0))
    }
}

impl Control {
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::Relaxed);
    }

    pub fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::Relaxed)
    }

    pub fn progress(&self) -> Progress {
        Progress {
            done: self.done.load(Ordering::Relaxed),
            total: self.total.load(Ordering::Relaxed),
            checked: self.checked.load(Ordering::Relaxed),
        }
    }

    fn started(&self, items: usize) {
        self.total.fetch_add(items, Ordering::Relaxed);
    }

    fn finished(&self, items: usize) {
        self.done.fetch_add(items, Ordering::Relaxed);
    }
}

// Parallel walk -------------------------------------------------------------

/// What the walker does with one directory entry.
struct Visit {
    bytes: u64,
    descend: bool,
    complete: bool,
}

/// One directory the walk read: its own entries' bytes, and after
/// `aggregate` the totals of everything below it.
struct Node {
    path: PathBuf,
    parent: Option<usize>,
    root: usize,
    bytes: u64,
    entries: u64,
    complete: bool,
}

struct Queue {
    jobs: Vec<usize>,
    nodes: Vec<Node>,
    active: usize,
    /// Folders still to read per root; a root is finished when it reaches 0.
    pending: Vec<usize>,
}

/// Reads every directory below `starts` on all cores. `visit` sees each entry
/// once, with `lstat` metadata, and decides whether to descend. Returns one
/// node per directory read, with subtree totals; the first `starts.len()`
/// nodes are the starts themselves. Each start that finishes counts as done
/// in `control`. A stopped `control` ends the walk and marks the unfinished
/// nodes incomplete.
fn walk(
    starts: Vec<(usize, PathBuf)>,
    control: &Control,
    visit: &(dyn Fn(usize, &Path, &fs::Metadata) -> Visit + Sync),
) -> Vec<Node> {
    let mut pending = vec![0; starts.iter().map(|(root, _)| root + 1).max().unwrap_or(0)];
    for (root, _) in &starts {
        pending[*root] += 1;
    }
    let nodes: Vec<Node> = starts
        .into_iter()
        .map(|(root, path)| Node {
            path,
            parent: None,
            root,
            bytes: 0,
            entries: 0,
            complete: true,
        })
        .collect();
    let queue = Mutex::new(Queue {
        jobs: (0..nodes.len()).rev().collect(),
        nodes,
        active: 0,
        pending,
    });
    let ready = Condvar::new();
    let threads = std::thread::available_parallelism()
        .map_or(4, |count| count.get())
        .min(16);
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| worker(&queue, &ready, control, visit));
        }
    });
    let mut nodes = queue
        .into_inner()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .nodes;
    if control.is_stopped() {
        for node in &mut nodes {
            node.complete = false;
        }
    }
    aggregate(&mut nodes);
    nodes
}

fn worker(
    queue: &Mutex<Queue>,
    ready: &Condvar,
    control: &Control,
    visit: &(dyn Fn(usize, &Path, &fs::Metadata) -> Visit + Sync),
) {
    loop {
        let (id, root, directory) = {
            let mut state = lock(queue);
            loop {
                if control.is_stopped() {
                    state.jobs.clear();
                }
                if let Some(id) = state.jobs.pop() {
                    state.active += 1;
                    let node = &state.nodes[id];
                    break (id, node.root, node.path.clone());
                }
                if state.active == 0 {
                    ready.notify_all();
                    return;
                }
                state = ready
                    .wait(state)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
            }
        };
        let mut bytes = 0_u64;
        let mut entries = 0_u64;
        let mut complete = true;
        let mut children = Vec::new();
        match fs::read_dir(&directory) {
            Ok(listing) => {
                for entry in listing {
                    if control.is_stopped() {
                        complete = false;
                        break;
                    }
                    // DirEntry::metadata does not follow symlinks.
                    let Ok((path, metadata)) =
                        entry.and_then(|entry| Ok((entry.path(), entry.metadata()?)))
                    else {
                        complete = false;
                        continue;
                    };
                    entries += 1;
                    let seen = visit(root, &path, &metadata);
                    bytes = bytes.saturating_add(seen.bytes);
                    complete &= seen.complete;
                    if seen.descend {
                        children.push(path);
                    }
                }
            }
            Err(_) => complete = false,
        }
        control.checked.fetch_add(entries, Ordering::Relaxed);
        let mut state = lock(queue);
        let node = &mut state.nodes[id];
        node.bytes = bytes;
        node.entries = entries;
        node.complete = complete;
        state.pending[root] = state.pending[root] + children.len() - 1;
        if state.pending[root] == 0 {
            control.finished(1);
        }
        for path in children {
            let child = state.nodes.len();
            state.nodes.push(Node {
                path,
                parent: Some(id),
                root,
                bytes: 0,
                entries: 0,
                complete: true,
            });
            state.jobs.push(child);
        }
        state.active -= 1;
        ready.notify_all();
    }
}

/// Children always come after their parent, so one reverse pass adds every
/// subtree into its parent.
fn aggregate(nodes: &mut [Node]) {
    for index in (0..nodes.len()).rev() {
        if let Some(parent) = nodes[index].parent {
            let (bytes, entries, complete) = {
                let node = &nodes[index];
                (node.bytes, node.entries, node.complete)
            };
            let parent = &mut nodes[parent];
            parent.bytes = parent.bytes.saturating_add(bytes);
            parent.entries += entries;
            parent.complete &= complete;
        }
    }
}

/// Measured sizes, in input order, plus the large folders found on the way
/// for the disk map cache. A stopped walk returns no cache entries.
pub struct Measured {
    pub sizes: Vec<Size>,
    pub folders: Vec<(PathBuf, Size)>,
}

/// Measures every path at once on all cores. Symlinks count as themselves,
/// other volumes are not entered, and a file with several hard links counts
/// once per measured path.
pub fn measure_all(paths: &[PathBuf], control: &Control) -> Measured {
    control.started(paths.len());
    let mut sizes = vec![Size::default(); paths.len()];
    let mut devices = vec![0_u64; paths.len()];
    let mut starts = Vec::new();
    for (index, path) in paths.iter().enumerate() {
        let Ok(metadata) = fs::symlink_metadata(path) else {
            control.finished(1);
            continue;
        };
        sizes[index] = Size {
            bytes: allocated(&metadata),
            complete: true,
        };
        if metadata.is_dir() {
            devices[index] = metadata.dev();
            starts.push((index, path.clone()));
        } else {
            control.finished(1);
        }
    }
    let links = Mutex::new(HashSet::new());
    let visit = |root: usize, _: &Path, metadata: &fs::Metadata| {
        let is_dir = metadata.is_dir();
        if !is_dir
            && metadata.nlink() > 1
            && !lock(&links).insert((root, metadata.dev(), metadata.ino()))
        {
            return Visit {
                bytes: 0,
                descend: false,
                complete: true,
            };
        }
        let same_volume = metadata.dev() == devices[root];
        Visit {
            bytes: allocated(metadata),
            descend: is_dir && same_volume,
            complete: !is_dir || same_volume,
        }
    };
    let nodes = walk(starts, control, &visit);
    let stopped = control.is_stopped();
    for node in nodes.iter().filter(|node| node.parent.is_none()) {
        let size = &mut sizes[node.root];
        size.bytes = size.bytes.saturating_add(node.bytes);
        size.complete = node.complete;
    }
    if stopped {
        for size in &mut sizes {
            size.complete = false;
        }
        return Measured {
            sizes,
            folders: Vec::new(),
        };
    }
    let folders = nodes
        .into_iter()
        .filter(|node| node.entries >= CACHE_MIN_ENTRIES)
        .map(|node| {
            let size = match node.parent {
                // A measured path's size also includes its own entry.
                None => sizes[node.root],
                Some(_) => Size {
                    bytes: node.bytes,
                    complete: node.complete,
                },
            };
            (node.path, size)
        })
        .collect();
    Measured { sizes, folders }
}

// Caches and logs -----------------------------------------------------------

fn cleanup_groups(home: &Path) -> Vec<(PathBuf, CleanCategory)> {
    let xcode = home.join("Library/Developer/Xcode");
    vec![
        (home.join("Library/Caches"), CleanCategory::Cache),
        (home.join("Library/Logs"), CleanCategory::Log),
        (home.join(".cache"), CleanCategory::Temporary),
        (xcode.join("DerivedData"), CleanCategory::Developer),
        (xcode.join("iOS DeviceSupport"), CleanCategory::Developer),
        (
            xcode.join("watchOS DeviceSupport"),
            CleanCategory::Developer,
        ),
        (xcode.join("tvOS DeviceSupport"), CleanCategory::Developer),
        (
            xcode.join("visionOS DeviceSupport"),
            CleanCategory::Developer,
        ),
        (
            home.join("Library/Developer/CoreSimulator/Caches"),
            CleanCategory::Developer,
        ),
        (home.join(".npm/_cacache"), CleanCategory::Developer),
    ]
}

pub fn cleanup_roots(home: &Path) -> Vec<PathBuf> {
    cleanup_groups(home)
        .into_iter()
        .map(|(root, _)| root)
        .collect()
}

pub fn scan_cleanup(home: &Path, control: &Control) -> ScanResult<CleanItem> {
    let mut result = ScanResult::default();
    let mut found = Vec::new();
    for (root, category) in cleanup_groups(home) {
        list_children(&root, &mut result.errors, |path| {
            found.push((path, category))
        });
    }
    let mut result = finish_items(found, result, control);
    sort_largest_first(&mut result.items);
    result
}

/// Calls `add` for each entry of `root`. A missing root is not an error.
pub(crate) fn list_children(root: &Path, errors: &mut Vec<String>, mut add: impl FnMut(PathBuf)) {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => {
            errors.push(format!("{}: {error}", root.display()));
            return;
        }
    };
    for entry in entries {
        match entry {
            Ok(entry) => add(entry.path()),
            Err(error) => errors.push(format!("{}: {error}", root.display())),
        }
    }
}

/// Measures the found paths together and turns them into unselected items.
pub(crate) fn finish_items(
    found: Vec<(PathBuf, CleanCategory)>,
    mut result: ScanResult<CleanItem>,
    control: &Control,
) -> ScanResult<CleanItem> {
    let paths: Vec<PathBuf> = found.iter().map(|(path, _)| path.clone()).collect();
    let sizes = measure_all(&paths, control).sizes;
    for ((path, category), size) in found.into_iter().zip(sizes) {
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) => {
                if error.kind() != std::io::ErrorKind::NotFound {
                    result.errors.push(format!("{}: {error}", path.display()));
                }
                continue;
            }
        };
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        result.items.push(CleanItem {
            id: path.to_string_lossy().into_owned(),
            category,
            name,
            identity: item_identity(&path),
            path,
            bytes: size.bytes,
            selected: false,
            complete: size.complete,
            modified: metadata.modified().ok(),
            note: None,
        });
    }
    result
}

pub(crate) fn sort_largest_first(items: &mut [CleanItem]) {
    items.sort_by(|a, b| {
        b.bytes
            .cmp(&a.bytes)
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.path.cmp(&b.path))
    });
}

// Installers ----------------------------------------------------------------

const INSTALLER_EXTENSIONS: [&str; 5] = ["dmg", "pkg", "mpkg", "iso", "xip"];
/// Installer packages that are often folders (bundle packages), not files.
const BUNDLE_INSTALLER_EXTENSIONS: [&str; 2] = ["pkg", "mpkg"];

/// User-owned folders where downloaded installers usually collect. Other apps'
/// sandbox containers are left out so scanning never raises a privacy prompt
/// for another app's data.
pub fn installer_roots(home: &Path) -> Vec<PathBuf> {
    vec![
        home.join("Downloads"),
        home.join("Desktop"),
        home.join("Documents"),
        home.join("Public"),
        home.join("Library/Downloads"),
        home.join("Library/Mobile Documents/com~apple~CloudDocs/Downloads"),
    ]
}

/// Finds installer images and packages directly inside each root or one
/// folder below it, without following symlinks or opening app bundles.
pub fn scan_installers(home: &Path, control: &Control) -> ScanResult<CleanItem> {
    let mut result = ScanResult::default();
    let mut found = Vec::new();
    for root in installer_roots(home) {
        collect_installers(&root, 2, &mut found, &mut result.errors);
    }
    let mut result = finish_items(found, result, control);
    sort_largest_first(&mut result.items);
    result
}

fn collect_installers(
    directory: &Path,
    depth: usize,
    found: &mut Vec<(PathBuf, CleanCategory)>,
    errors: &mut Vec<String>,
) {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => {
            errors.push(format!("{}: {error}", directory.display()));
            return;
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                errors.push(format!("{}: {error}", directory.display()));
                continue;
            }
        };
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if is_installer_kind(&path, file_type) {
            found.push((path, CleanCategory::Installer));
        } else if file_type.is_dir()
            && depth > 1
            && path.extension().is_none_or(|extension| extension != "app")
        {
            collect_installers(&path, depth - 1, found, errors);
        }
    }
}

fn has_extension(path: &Path, known: &[&str]) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| known.iter().any(|k| extension.eq_ignore_ascii_case(k)))
}

/// Installer files, or bundle-style `.pkg`/`.mpkg` folders.
fn is_installer_kind(path: &Path, file_type: fs::FileType) -> bool {
    (file_type.is_file() && has_extension(path, &INSTALLER_EXTENSIONS))
        || (file_type.is_dir() && has_extension(path, &BUNDLE_INSTALLER_EXTENSIONS))
}

pub fn is_safe_installer_path(path: &Path, home: &Path) -> bool {
    if !has_safe_path_text(path)
        || !path.is_absolute()
        || has_parent_component(path)
        || !fs::symlink_metadata(path)
            .is_ok_and(|metadata| is_installer_kind(path, metadata.file_type()))
    {
        return false;
    }
    let Some(parent) = path
        .parent()
        .and_then(|parent| fs::canonicalize(parent).ok())
    else {
        return false;
    };
    installer_roots(home).iter().any(|root| {
        fs::canonicalize(root).is_ok_and(|root| {
            parent
                .strip_prefix(&root)
                .is_ok_and(|rest| rest.components().count() <= 1)
        })
    })
}

// Disk map protection -------------------------------------------------------

/// The item's path relative to the home folder, lowercased, with its parent
/// resolved. None when it is outside the home folder or cannot be resolved.
/// macOS volumes are usually case-insensitive, so `~/library/keychains`
/// names the same folder as `~/Library/Keychains`.
fn home_relative_lowercase(path: &Path, home: &Path) -> Option<PathBuf> {
    let resolved = path
        .parent()
        .and_then(|parent| fs::canonicalize(parent).ok())
        .zip(path.file_name())
        .map(|(parent, name)| parent.join(name))?;
    let home = fs::canonicalize(home).ok()?;
    let (path, home) = (
        resolved.to_str()?.to_lowercase(),
        home.to_str()?.to_lowercase(),
    );
    Path::new(&path)
        .strip_prefix(&home)
        .ok()
        .map(Path::to_path_buf)
}

/// Folders the disk analyzer must never offer to move to Trash: the home
/// folder's standard top-level folders, credential stores, and anything
/// outside the home folder.
pub fn is_protected_disk_path(path: &Path, home: &Path) -> bool {
    const PROTECTED_HOME_CHILDREN: [&str; 16] = [
        "Applications",
        "Desktop",
        "Documents",
        "Downloads",
        "Library",
        "Movies",
        "Music",
        "Pictures",
        "Public",
        ".Trash",
        ".ssh",
        ".gnupg",
        ".config",
        ".vault",
        ".zshrc",
        ".zprofile",
    ];
    let Some(relative) = home_relative_lowercase(path, home) else {
        return true;
    };
    relative.as_os_str().is_empty()
        || (relative.components().count() == 1
            && PROTECTED_HOME_CHILDREN
                .iter()
                .any(|name| relative.as_os_str() == name.to_lowercase().as_str()))
        || [".ssh", ".gnupg", ".vault", "library/keychains"]
            .iter()
            .any(|tree| relative.starts_with(tree))
}

// Applications --------------------------------------------------------------

pub fn scan_apps(home: &Path, control: &Control) -> ScanResult<AppItem> {
    let mut result = ScanResult::default();
    let mut found = Vec::new();
    for root in [PathBuf::from("/Applications"), home.join("Applications")] {
        collect_apps(&root, 1, &mut found, &mut result.errors);
    }
    result.items = describe_apps(found, home, control);
    result.items.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.path.cmp(&b.path))
    });
    result
}

fn collect_apps(root: &Path, depth: usize, found: &mut Vec<PathBuf>, errors: &mut Vec<String>) {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => {
            errors.push(format!("{}: {error}", root.display()));
            return;
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                errors.push(format!("{}: {error}", root.display()));
                continue;
            }
        };
        let path = entry.path();
        if path.extension().is_some_and(|extension| extension == "app") {
            found.push(path);
        } else if depth > 0
            && entry.file_type().is_ok_and(|file_type| file_type.is_dir())
            && !entry.file_name().to_string_lossy().starts_with('.')
        {
            collect_apps(&path, depth - 1, found, errors);
        }
    }
}

/// Reads each app's metadata and measures the apps and their bundle-ID
/// caches and logs in one walk.
fn describe_apps(paths: Vec<PathBuf>, home: &Path, control: &Control) -> Vec<AppItem> {
    let mut apps: Vec<AppItem> = paths
        .into_iter()
        .map(|path| {
            let (bundle_id, version) = read_bundle_info(&path);
            let id = bundle_id.as_deref().filter(|id| is_safe_bundle_id(id));
            let found = |paths: Vec<PathBuf>, safe: &dyn Fn(&Path) -> bool| -> Vec<RelatedPath> {
                paths
                    .into_iter()
                    .filter(|path| safe(path))
                    .map(|path| RelatedPath {
                        identity: item_identity(&path),
                        path,
                        bytes: 0,
                    })
                    .collect()
            };
            let (related, data) = id.map_or_else(Default::default, |id| {
                (
                    found(exact_bundle_caches(home, id).to_vec(), &|path| {
                        is_safe_cleanup_path(path, home)
                    }),
                    found(exact_app_data(home, id).to_vec(), &|path| {
                        is_safe_app_data_path(path, home, id)
                    }),
                )
            });
            AppItem {
                name: path
                    .file_stem()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "Application".to_string()),
                bundle_id,
                version,
                identity: item_identity(&path),
                modified: fs::symlink_metadata(&path)
                    .and_then(|metadata| metadata.modified())
                    .ok(),
                path,
                related,
                data,
                ..AppItem::default()
            }
        })
        .collect();
    let mut paths = Vec::new();
    for app in &apps {
        paths.push(app.path.clone());
        paths.extend(
            app.related
                .iter()
                .chain(&app.data)
                .map(|related| related.path.clone()),
        );
    }
    let mut sizes = measure_all(&paths, control).sizes.into_iter();
    for app in &mut apps {
        let size = sizes.next().unwrap_or_default();
        app.bytes = size.bytes;
        app.complete = size.complete;
        for related in app.related.iter_mut().chain(&mut app.data) {
            related.bytes = sizes.next().unwrap_or_default().bytes;
        }
    }
    apps
}

/// Settings and app data named after the exact bundle ID. Containers and
/// Group Containers are left out: macOS asks for permission before another
/// app's container can be read or moved.
pub fn exact_app_data(home: &Path, bundle_id: &str) -> [PathBuf; 5] {
    let library = home.join("Library");
    [
        library.join("Application Support").join(bundle_id),
        library
            .join("Preferences")
            .join(format!("{bundle_id}.plist")),
        library
            .join("Saved Application State")
            .join(format!("{bundle_id}.savedState")),
        library.join("HTTPStorages").join(bundle_id),
        library.join("WebKit").join(bundle_id),
    ]
}

/// True only for an existing path that is exactly one of `exact_app_data`,
/// inside the real `~/Library` folder it names.
pub fn is_safe_app_data_path(path: &Path, home: &Path, bundle_id: &str) -> bool {
    if !is_safe_bundle_id(bundle_id)
        || !has_safe_path_text(path)
        || !exact_app_data(home, bundle_id)
            .iter()
            .any(|expected| expected == path)
        || fs::symlink_metadata(path).is_err()
    {
        return false;
    }
    let Some((parent, folder)) = path
        .parent()
        .and_then(|parent| Some((parent, parent.file_name()?)))
    else {
        return false;
    };
    fs::canonicalize(parent)
        .ok()
        .zip(fs::canonicalize(home).ok())
        .is_some_and(|(parent, home)| parent == home.join("Library").join(folder))
}

/// A bundle ID from an app's Info.plist is untrusted text. It must be a
/// single plain path component before it is joined to a Library folder.
pub fn is_safe_bundle_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 255
        && !id.starts_with('.')
        && !id.contains('/')
        && !id.chars().any(char::is_control)
}

pub fn exact_bundle_caches(home: &Path, bundle_id: &str) -> [PathBuf; 2] {
    [
        home.join("Library/Caches").join(bundle_id),
        home.join("Library/Logs").join(bundle_id),
    ]
}

/// Reads `CFBundleIdentifier` and `CFBundleShortVersionString` from the app's Info.plist.
fn read_bundle_info(app_path: &Path) -> (Option<String>, Option<String>) {
    with_plist(&app_path.join("Contents/Info.plist"), |dictionary| {
        (
            plist_text(dictionary, ns_string!("CFBundleIdentifier")),
            plist_text(dictionary, ns_string!("CFBundleShortVersionString")),
        )
    })
    .unwrap_or((None, None))
}

/// Runs `read` on a property list file (XML or binary) whose top level is a
/// dictionary. None when the file is missing, unreadable, or not a dictionary.
pub(crate) fn with_plist<R>(
    file: &Path,
    read: impl FnOnce(&NSDictionary<NSString, AnyObject>) -> R,
) -> Option<R> {
    #[allow(deprecated)]
    objc2::rc::autoreleasepool(|_| {
        let path = NSString::from_str(&file.to_string_lossy());
        let dictionary =
            unsafe { NSDictionary::<NSString, AnyObject>::dictionaryWithContentsOfFile(&path) }?;
        Some(read(&dictionary))
    })
}

pub(crate) fn plist_text(
    dictionary: &NSDictionary<NSString, AnyObject>,
    key: &NSString,
) -> Option<String> {
    dictionary
        .objectForKey(key)
        .and_then(|value| value.downcast::<NSString>().ok())
        .map(|value| value.to_string())
}

// Disk map ------------------------------------------------------------------

/// Lists `root` with each item's size. Folders already in `cache` are not
/// walked again. Unreadable entries are skipped and reported in `errors`;
/// only an unreadable `root` fails. Returns new cache entries, including
/// `root` itself when the walk finished.
pub fn analyze_directory(root: &Path, cache: &SizeCache, control: &Control) -> DiskScan {
    let mut result = ScanResult::default();
    let mut found = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                result.errors.push(format!("{}: {error}", root.display()));
                continue;
            }
        };
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(error) => {
                result
                    .errors
                    .push(format!("{}: {error}", entry.path().display()));
                continue;
            }
        };
        found.push((entry, metadata));
    }
    let uncached: Vec<PathBuf> = found
        .iter()
        .map(|(entry, _)| entry.path())
        .filter(|path| !cache.contains_key(path))
        .collect();
    let measured = measure_all(&uncached, control);
    let mut sizes: HashMap<&PathBuf, Size> = uncached.iter().zip(measured.sizes).collect();
    for (entry, metadata) in found {
        let path = entry.path();
        let size = cache
            .get(&path)
            .copied()
            .or_else(|| sizes.remove(&path))
            .unwrap_or_default();
        result.items.push(DiskItem {
            name: entry.file_name().to_string_lossy().into_owned(),
            identity: item_identity(&path),
            path,
            bytes: size.bytes,
            is_directory: metadata.is_dir(),
            complete: size.complete,
            modified: metadata.modified().ok(),
        });
    }
    result
        .items
        .sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.name.cmp(&b.name)));
    let mut folders = measured.folders;
    if !control.is_stopped() {
        let own = fs::symlink_metadata(root).map_or(0, |metadata| allocated(&metadata));
        folders.push((
            root.to_path_buf(),
            Size {
                bytes: result
                    .items
                    .iter()
                    .fold(own, |total, item| total.saturating_add(item.bytes)),
                complete: result.errors.is_empty() && result.items.iter().all(|item| item.complete),
            },
        ));
    }
    Ok((result, folders))
}

// Large files and build folders ---------------------------------------------

/// Home-folder children the searches never enter: Library holds other apps'
/// data (reading it raises privacy prompts), and Trash is already deleted.
const SEARCH_SKIPPED_HOME_CHILDREN: [&str; 2] = ["library", ".trash"];

/// Bundles the searches never open: app bundles are listed under
/// Applications, and reading a Photos library raises a privacy prompt.
const SEARCH_SKIPPED_EXTENSIONS: [&str; 2] = ["app", "photoslibrary"];

fn search_enters(path: &Path, home: &Path, metadata: &fs::Metadata, device: u64) -> bool {
    if !metadata.is_dir()
        || metadata.dev() != device
        || has_extension(path, &SEARCH_SKIPPED_EXTENSIONS)
    {
        return false;
    }
    !(path.parent() == Some(home)
        && path.file_name().is_some_and(|name| {
            SEARCH_SKIPPED_HOME_CHILDREN
                .iter()
                .any(|skipped| name.to_string_lossy().eq_ignore_ascii_case(skipped))
        }))
}

/// Walks the home folder, outside Library and Trash, and calls `found` for
/// entries it wants. `found` returns true to stop descending there. Each
/// home-folder child is its own root, so progress counts them as they finish.
fn search_home(
    home: &Path,
    control: &Control,
    found: &(dyn Fn(&Path, &fs::Metadata) -> bool + Sync),
) -> Option<()> {
    let device = fs::symlink_metadata(home).ok()?.dev();
    let mut starts = Vec::new();
    for entry in fs::read_dir(home).ok()?.flatten() {
        let path = entry.path();
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if !found(&path, &metadata) && search_enters(&path, home, &metadata, device) {
            starts.push((starts.len(), path));
        }
    }
    control.started(starts.len());
    let visit = |_: usize, path: &Path, metadata: &fs::Metadata| {
        let claimed = found(path, metadata);
        Visit {
            bytes: 0,
            descend: !claimed && search_enters(path, home, metadata, device),
            complete: true,
        }
    };
    walk(starts, control, &visit);
    Some(())
}

/// Files of at least `min_bytes` on disk that have not changed for `min_age`.
pub fn scan_large_files(
    home: &Path,
    min_bytes: u64,
    min_age: Duration,
    control: &Control,
) -> ScanResult<CleanItem> {
    let now = SystemTime::now();
    let matches = Mutex::new(Vec::new());
    let found = |path: &Path, metadata: &fs::Metadata| {
        let old = metadata
            .modified()
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age >= min_age);
        if metadata.is_file() && allocated(metadata) >= min_bytes && old {
            lock(&matches).push((path.to_path_buf(), CleanCategory::LargeFile));
        }
        false
    };
    let mut result = ScanResult::default();
    if search_home(home, control, &found).is_none() {
        result
            .errors
            .push(format!("{}: cannot be read", home.display()));
    }
    let found = matches
        .into_inner()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut result = finish_items(found, result, control);
    sort_largest_first(&mut result.items);
    result
}

/// Cargo `target` folders (marked with `CACHEDIR.TAG`) and npm
/// `node_modules` folders next to a `package.json`.
pub fn scan_build_folders(home: &Path, control: &Control) -> ScanResult<CleanItem> {
    let matches = Mutex::new(Vec::new());
    let found = |path: &Path, metadata: &fs::Metadata| {
        let hit = metadata.is_dir() && is_build_folder(path);
        if hit {
            lock(&matches).push((path.to_path_buf(), CleanCategory::BuildFolder));
        }
        hit
    };
    let mut result = ScanResult::default();
    if search_home(home, control, &found).is_none() {
        result
            .errors
            .push(format!("{}: cannot be read", home.display()));
    }
    let found = matches
        .into_inner()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut result = finish_items(found, result, control);
    sort_largest_first(&mut result.items);
    result
}

fn is_build_folder(path: &Path) -> bool {
    let is_file = |path: PathBuf| fs::symlink_metadata(path).is_ok_and(|m| m.is_file());
    match path.file_name().and_then(|name| name.to_str()) {
        Some("target") => is_file(path.join("CACHEDIR.TAG")),
        Some("node_modules") => path
            .parent()
            .is_some_and(|parent| is_file(parent.join("package.json"))),
        _ => false,
    }
}

/// True when `path` is inside the home folder, outside Library and Trash,
/// and not a protected disk-map path.
fn is_in_searched_home(path: &Path, home: &Path) -> bool {
    if !has_safe_path_text(path) || !path.is_absolute() || has_parent_component(path) {
        return false;
    }
    let Some(relative) = home_relative_lowercase(path, home) else {
        return false;
    };
    let first = relative.components().next();
    first.is_some_and(|first| {
        !SEARCH_SKIPPED_HOME_CHILDREN
            .iter()
            .any(|skipped| first.as_os_str() == *skipped)
    }) && !is_protected_disk_path(path, home)
}

pub fn is_safe_large_file_path(path: &Path, home: &Path) -> bool {
    is_in_searched_home(path, home)
        && fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_file())
}

pub fn is_safe_build_folder_path(path: &Path, home: &Path) -> bool {
    is_in_searched_home(path, home)
        && fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_dir())
        && is_build_folder(path)
}

// Identity and path guards --------------------------------------------------

pub fn item_identity(path: &Path) -> Option<ItemIdentity> {
    let metadata = fs::symlink_metadata(path).ok()?;
    let parent = fs::canonicalize(path.parent()?).ok()?;
    let parent_metadata = fs::metadata(parent).ok()?;
    Some(ItemIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
        parent_device: parent_metadata.dev(),
        parent_inode: parent_metadata.ino(),
    })
}

pub fn identity_matches(path: &Path, expected: Option<ItemIdentity>) -> bool {
    expected.is_some_and(|expected| item_identity(path) == Some(expected))
}

fn has_safe_path_text(path: &Path) -> bool {
    path.to_str()
        .is_some_and(|text| !text.chars().any(|character| character.is_control()))
}

fn has_parent_component(path: &Path) -> bool {
    path.components()
        .any(|component| matches!(component, Component::ParentDir))
}

pub fn is_safe_cleanup_path(path: &Path, home: &Path) -> bool {
    if !has_safe_path_text(path)
        || !path.is_absolute()
        || has_parent_component(path)
        || path.file_name().is_none()
        || fs::symlink_metadata(path).is_err()
    {
        return false;
    }

    cleanup_roots(home).iter().any(|root| {
        if path == root || !root.exists() {
            return false;
        }
        let Ok(root) = fs::canonicalize(root) else {
            return false;
        };
        let Some(parent) = path.parent() else {
            return false;
        };
        fs::canonicalize(parent).is_ok_and(|parent| parent.starts_with(root))
    })
}

pub fn is_safe_app_path(path: &Path, home: &Path) -> bool {
    if !has_safe_path_text(path)
        || !path.is_absolute()
        || path.extension().is_none_or(|extension| extension != "app")
        || has_parent_component(path)
        || fs::symlink_metadata(path).is_err()
    {
        return false;
    }
    [PathBuf::from("/Applications"), home.join("Applications")]
        .iter()
        .any(|root| {
            if path == root || !root.exists() {
                return false;
            }
            let Ok(root) = fs::canonicalize(root) else {
                return false;
            };
            path.parent().is_some_and(|parent| {
                fs::canonicalize(parent).is_ok_and(|parent| parent.starts_with(root))
            })
        })
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub fn fixture_home() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/home")
    }

    pub fn go() -> Control {
        Control::default()
    }

    /// Independent size oracle: `du -sk` reports allocated KiB and counts
    /// each hard-linked file once.
    pub fn du_bytes(path: &Path) -> u64 {
        let output = std::process::Command::new("/usr/bin/du")
            .arg("-sk")
            .arg(path)
            .output()
            .expect("run du");
        let text = String::from_utf8(output.stdout).unwrap();
        text.split_whitespace()
            .next()
            .unwrap()
            .parse::<u64>()
            .unwrap()
            * 1024
    }

    /// A scratch folder under target/ with fixed names, so reruns reuse it.
    fn scratch(name: &str) -> PathBuf {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/test-scratch")
            .join(name);
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn cleanup_scan_finds_fixture_caches_without_selecting_them() {
        let result = scan_cleanup(&fixture_home(), &go());
        assert_eq!(result.items.len(), 5, "errors: {:?}", result.errors);
        assert!(result
            .items
            .iter()
            .all(|item| !item.selected && item.complete));
        assert!(result.items.iter().all(|item| item.identity.is_some()));
        for item in &result.items {
            assert_eq!(item.bytes, du_bytes(&item.path), "{}", item.path.display());
        }
        assert!(result.errors.is_empty());
    }

    #[test]
    fn cleanup_guard_accepts_children_and_rejects_roots_and_escape() {
        let home = fixture_home();
        let child = home.join("Library/Caches/com.example.alpha");
        assert!(is_safe_cleanup_path(&child, &home));
        assert!(is_safe_cleanup_path(
            &home.join(".npm/_cacache/content-v2"),
            &home
        ));
        assert!(!is_safe_cleanup_path(&home.join(".npm/_cacache"), &home));
        assert!(!is_safe_cleanup_path(&home.join("Library/Caches"), &home));
        assert!(!is_safe_cleanup_path(Path::new("/etc/hosts"), &home));
        assert!(!is_safe_cleanup_path(
            &home.join("Library/Caches/../../Documents"),
            &home
        ));
        assert!(!is_safe_cleanup_path(
            &home.join("Library/Caches/missing/child"),
            &home
        ));
    }

    #[test]
    fn measure_all_matches_du_counts_hard_links_once_and_sparse_files_as_allocated() {
        let folder = scratch("links");
        let original = folder.join("original.bin");
        fs::write(&original, vec![7_u8; 200_000]).unwrap();
        let link = folder.join("link.bin");
        if fs::symlink_metadata(&link).is_err() {
            fs::hard_link(&original, &link).unwrap();
        }
        assert!(fs::metadata(&original).unwrap().nlink() >= 2);
        let sparse = folder.join("sparse.bin");
        let file = fs::File::create(&sparse).unwrap();
        file.set_len(64 * 1024 * 1024).unwrap();

        let size = measure_all(std::slice::from_ref(&folder), &go()).sizes[0];
        assert!(size.complete);
        assert_eq!(size.bytes, du_bytes(&folder));
        let one_copy = fs::metadata(&original).unwrap().blocks() * 512;
        assert!(size.bytes < 2 * one_copy, "hard link counted twice");
        assert!(
            size.bytes < 64 * 1024 * 1024,
            "sparse file counted at full length"
        );
    }

    #[test]
    fn measure_all_keeps_input_order_and_reports_missing_paths() {
        let home = fixture_home();
        let paths = [
            home.join("Library/Caches/com.example.beta"),
            home.join("missing"),
            home.join("Library/Logs/example.log"),
        ];
        let sizes = measure_all(&paths, &go()).sizes;
        assert_eq!(sizes[0].bytes, du_bytes(&paths[0]));
        assert_eq!(sizes[1], Size::default());
        assert_eq!(sizes[2].bytes, du_bytes(&paths[2]));
        assert!(sizes[0].complete && sizes[2].complete);
    }

    #[test]
    fn progress_counts_every_measured_item_and_entry() {
        let home = fixture_home();
        let paths = [
            home.join("Library"),
            home.join("missing"),
            home.join("Downloads/Setup.dmg"),
        ];
        let control = go();
        assert_eq!(control.progress().fraction(), None, "nothing started yet");
        measure_all(&paths, &control);
        let progress = control.progress();
        assert_eq!((progress.done, progress.total), (3, 3));
        assert_eq!(progress.fraction(), Some(1.0));
        // Library holds 5 children and 6 entries below them, so at least 11.
        assert!(progress.checked >= 11, "{progress:?}");

        let search = go();
        scan_build_folders(&home, &search);
        let progress = search.progress();
        assert_eq!(progress.done, progress.total, "{progress:?}");
        assert!(progress.total >= 2 && progress.checked > 0);
    }

    #[test]
    fn stopped_walk_marks_sizes_incomplete_and_caches_nothing() {
        let stop = Control::default();
        stop.stop();
        let measured = measure_all(&[fixture_home()], &stop);
        assert!(!measured.sizes[0].complete);
        assert!(measured.folders.is_empty());
        let (_, folders) = analyze_directory(&fixture_home(), &SizeCache::new(), &stop).unwrap();
        assert!(folders.is_empty());
    }

    #[test]
    fn analyzer_reports_sizes_and_reuses_cached_folders() {
        let caches = fixture_home().join("Library/Caches");
        let (result, folders) = analyze_directory(&caches, &SizeCache::new(), &go()).unwrap();
        assert_eq!(result.items.len(), 2);
        assert!(result
            .items
            .iter()
            .all(|item| item.is_directory && item.complete));
        for item in &result.items {
            assert_eq!(item.bytes, du_bytes(&item.path));
            assert!(item.identity.is_some());
        }
        assert_eq!(folders.last().unwrap().0, caches);

        let mut cache = SizeCache::new();
        let alpha = caches.join("com.example.alpha");
        cache.insert(
            alpha.clone(),
            Size {
                bytes: 123_456,
                complete: true,
            },
        );
        let (result, _) = analyze_directory(&caches, &cache, &go()).unwrap();
        let item = result.items.iter().find(|item| item.path == alpha).unwrap();
        assert_eq!(item.bytes, 123_456);
        assert!(
            item.identity.is_some(),
            "identity is read fresh, not cached"
        );
    }

    #[test]
    fn app_inventory_reads_metadata_and_measures_exact_caches() {
        let home = fixture_home();
        let mut found = Vec::new();
        let mut errors = Vec::new();
        collect_apps(&home.join("Applications"), 0, &mut found, &mut errors);
        let apps = describe_apps(found, &home, &go());
        assert_eq!(apps.len(), 1, "errors: {errors:?}");
        assert_eq!(apps[0].name, "Example");
        assert_eq!(apps[0].bundle_id.as_deref(), Some("com.example.alpha"));
        assert!(apps[0].identity.is_some() && apps[0].complete);
        assert_eq!(apps[0].bytes, du_bytes(&apps[0].path));
        assert!(is_safe_app_path(&apps[0].path, &home));
        let cache = home.join("Library/Caches/com.example.alpha");
        assert_eq!(apps[0].related.len(), 1);
        assert_eq!(apps[0].related[0].path, cache);
        assert_eq!(apps[0].related[0].bytes, du_bytes(&cache));
        assert!(apps[0].related[0].identity.is_some());
        let data: Vec<_> = apps[0].data.iter().map(|data| data.path.clone()).collect();
        assert_eq!(
            data,
            [
                home.join("Library/Application Support/com.example.alpha"),
                home.join("Library/Preferences/com.example.alpha.plist"),
            ]
        );
        assert!(apps[0]
            .data
            .iter()
            .all(|data| data.bytes == du_bytes(&data.path)));
        assert!(!apps[0].include_data, "app data starts unticked");
    }

    #[test]
    fn app_data_guard_accepts_only_the_exact_names() {
        let home = fixture_home();
        let id = "com.example.alpha";
        assert!(is_safe_app_data_path(
            &home.join("Library/Preferences/com.example.alpha.plist"),
            &home,
            id
        ));
        for rejected in [
            "Library/Preferences/com.example.beta.plist",
            "Library/Preferences",
            "Library/Application Support/com.example.alpha/data.bin",
            "Library/Saved Application State/com.example.alpha.savedState",
            "Library/Caches/com.example.alpha",
        ] {
            assert!(
                !is_safe_app_data_path(&home.join(rejected), &home, id),
                "{rejected}"
            );
        }
        assert!(!is_safe_app_data_path(
            &home.join("Library/Application Support/../Preferences"),
            &home,
            "../Preferences"
        ));
    }

    #[test]
    fn bundle_ids_must_be_one_plain_component() {
        assert!(is_safe_bundle_id("com.example.app"));
        for bad in ["", ".", "..", "../x", "a/b", "/abs", ".hidden", "a\nb"] {
            assert!(!is_safe_bundle_id(bad), "{bad:?}");
        }
    }

    #[test]
    fn installer_scan_finds_images_and_bundle_packages_two_levels_deep_only() {
        let home = fixture_home();
        let result = scan_installers(&home, &go());
        let mut names: Vec<_> = result.items.iter().map(|item| item.name.as_str()).collect();
        names.sort_unstable();
        assert_eq!(
            names,
            ["Bundle.mpkg", "Setup.dmg", "Tool.PKG", "Xcode.xip"],
            "{result:?}"
        );
        for item in &result.items {
            assert_eq!(item.bytes, du_bytes(&item.path));
        }
        assert!(result
            .items
            .iter()
            .all(|item| !item.selected && item.identity.is_some()));
        assert!(result.errors.is_empty());
    }

    #[test]
    fn installer_guard_accepts_found_files_and_rejects_everything_else() {
        let home = fixture_home();
        for accepted in [
            "Downloads/Setup.dmg",
            "Downloads/sub/Tool.PKG",
            "Downloads/Bundle.mpkg",
        ] {
            assert!(
                is_safe_installer_path(&home.join(accepted), &home),
                "{accepted}"
            );
        }
        for rejected in [
            "Downloads/sub/deeper/TooDeep.iso",
            "Downloads/Link.dmg",
            "Downloads/readme.txt",
            "Downloads/missing.dmg",
            "Downloads/../Downloads/Setup.dmg",
            "Downloads/Folder.dmg",
            "Library/Caches/com.example.alpha",
        ] {
            assert!(
                !is_safe_installer_path(&home.join(rejected), &home),
                "{rejected}"
            );
        }
    }

    #[test]
    fn analyzer_protects_home_structure_and_credentials() {
        let home = fixture_home();
        for protected in ["", "Library", "Downloads", "Desktop", ".ssh/id_ed25519"] {
            assert!(
                is_protected_disk_path(&home.join(protected), &home),
                "{protected}"
            );
        }
        assert!(is_protected_disk_path(
            &home.join("Library/Keychains/placeholder"),
            &home
        ));
        // Case-insensitive volumes resolve differently cased names to the same folder.
        assert!(is_protected_disk_path(
            &home.join("library/keychains/placeholder"),
            &home
        ));
        assert!(is_protected_disk_path(&home.join("LIBRARY"), &home));
        assert!(is_protected_disk_path(Path::new("/etc/hosts"), &home));
        assert!(is_protected_disk_path(Path::new("/"), &home));
        assert!(!is_protected_disk_path(
            &home.join("Downloads/Setup.dmg"),
            &home
        ));
        assert!(!is_protected_disk_path(
            &home.join("Library/Caches/com.example.alpha"),
            &home
        ));
    }

    #[test]
    fn app_guard_rejects_non_apps_and_paths_outside_application_roots() {
        let home = fixture_home();
        assert!(!is_safe_app_path(Path::new("/etc/hosts"), &home));
        assert!(!is_safe_app_path(&home.join("Library/Fake.app"), &home));
        assert!(!is_safe_app_path(
            Path::new("/Applications/Missing.app"),
            &home
        ));
    }

    #[test]
    fn large_file_search_skips_library_apps_and_respects_size_and_age() {
        let home = fixture_home();
        let result = scan_large_files(&home, 1, Duration::ZERO, &go());
        let paths: Vec<_> = result.items.iter().map(|item| item.path.clone()).collect();
        assert!(paths.contains(&home.join("Downloads/Setup.dmg")));
        assert!(paths.contains(&home.join("Documents/proj/target/debug/out.bin")));
        for path in &paths {
            let relative = path.strip_prefix(&home).unwrap();
            assert!(!relative.starts_with("Library"), "{}", path.display());
            assert!(
                !path
                    .components()
                    .any(|c| c.as_os_str().to_string_lossy().ends_with(".app")),
                "{}",
                path.display()
            );
            assert!(is_safe_large_file_path(path, &home), "{}", path.display());
        }
        assert!(result
            .items
            .iter()
            .all(|item| item.category == CleanCategory::LargeFile));
        let none = scan_large_files(&home, LARGE_FILE_BYTES, Duration::ZERO, &go());
        assert!(none.items.is_empty());
        let too_new = scan_large_files(&home, 1, Duration::from_secs(100 * 365 * 86_400), &go());
        assert!(too_new.items.is_empty());
    }

    #[test]
    fn large_file_guard_rejects_library_trash_protected_and_folders() {
        let home = fixture_home();
        for rejected in [
            "Library/Caches/com.example.alpha/cache.bin",
            "library/logs/example.log",
            ".ssh/id_ed25519",
            "Downloads",
            "Downloads/sub",
            "Downloads/missing.bin",
        ] {
            assert!(
                !is_safe_large_file_path(&home.join(rejected), &home),
                "{rejected}"
            );
        }
        assert!(!is_safe_large_file_path(Path::new("/etc/hosts"), &home));
        assert!(!is_safe_large_file_path(
            &home.join("Downloads/../Downloads/Setup.dmg"),
            &home
        ));
    }

    #[test]
    fn build_folder_search_needs_the_tool_markers() {
        let home = fixture_home();
        let result = scan_build_folders(&home, &go());
        let mut paths: Vec<_> = result
            .items
            .iter()
            .map(|item| item.path.strip_prefix(&home).unwrap().to_path_buf())
            .collect();
        paths.sort();
        assert_eq!(
            paths,
            [
                PathBuf::from("Documents/proj/target"),
                PathBuf::from("Documents/web/node_modules")
            ]
        );
        for item in &result.items {
            assert_eq!(item.bytes, du_bytes(&item.path));
            assert!(is_safe_build_folder_path(&item.path, &home));
        }
        for rejected in [
            "Documents/fake/target",
            "Documents/loose/node_modules",
            "Documents/proj",
            "Library/Caches/com.example.alpha",
        ] {
            assert!(
                !is_safe_build_folder_path(&home.join(rejected), &home),
                "{rejected}"
            );
        }
    }
}
