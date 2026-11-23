//! Application state and background jobs, independent of drawing.

use maccleanup::model::{format_bytes, AppItem, CleanItem, DiskItem, OperationResult};
use maccleanup::scanner::Control;
#[cfg(test)]
use maccleanup::scanner::Size;
use maccleanup::scanner::{
    analyze_directory, is_protected_disk_path, scan_apps, scan_build_folders, scan_cleanup,
    scan_installers, scan_large_files, DiskScan, ScanResult, SizeCache, LARGE_FILE_AGE,
    LARGE_FILE_BYTES,
};
use maccleanup::system::{self, Breakdown, StorageExtras, SystemStatus, VolumeUsage};
use maccleanup::trash::{related_app_data, trash_apps, trash_disk_item, trash_items};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

pub const STATUS_REFRESH: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Section {
    Overview,
    Storage,
    Clean,
    Applications,
    Installers,
    LargeFiles,
    BuildFolders,
    Disk,
    Status,
    Activity,
}

impl Section {
    pub const COUNT: usize = 10;
    pub const ALL: [Self; Self::COUNT] = [
        Self::Overview,
        Self::Storage,
        Self::Clean,
        Self::Applications,
        Self::Installers,
        Self::LargeFiles,
        Self::BuildFolders,
        Self::Disk,
        Self::Status,
        Self::Activity,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Storage => "Storage",
            Self::Clean => "Caches and logs",
            Self::Applications => "Applications",
            Self::Installers => "Installer files",
            Self::LargeFiles => "Large old files",
            Self::BuildFolders => "Build folders",
            Self::Disk => "Disk map",
            Self::Status => "System",
            Self::Activity => "Activity",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Overview => "Where your storage goes, and what you can safely clear.",
            Self::Storage => "Everything on your startup disk, part by part. Click a part to see what it holds.",
            Self::Clean => "Tick the items you want to clear, then move them to Trash.",
            Self::Applications => "Uninstall moves the app and its exact caches and logs. Settings and app data move only if you tick them.",
            Self::Installers => "Disk images and packages left in Downloads, Desktop, Documents, Public, and iCloud Downloads.",
            Self::LargeFiles => "Files of 1 GB or more, unchanged for 6 months, anywhere in your home folder outside Library.",
            Self::BuildFolders => "Cargo target and npm node_modules folders in your projects. Your build tools make them again.",
            Self::Disk => "The largest items in a folder, drawn to scale. Double-click a folder to open it.",
            Self::Status => "Memory, storage, and load. Updates every 2 seconds.",
            Self::Activity => "Everything MacCleanUp moved to Trash since it opened.",
        }
    }

    pub fn index(self) -> usize {
        Self::ALL
            .iter()
            .position(|section| *section == self)
            .unwrap_or(0)
    }

    /// Sections whose scans start without a click. The searches read
    /// Desktop, Documents, and Downloads, and the disk map sizes whole
    /// folders; those can raise macOS privacy prompts, so they wait for an
    /// explicit scan.
    fn loads_automatically(self) -> bool {
        matches!(
            self,
            Self::Storage | Self::Clean | Self::Applications | Self::Status
        )
    }

    /// Sections whose rows are `CleanItem`s with a selection.
    pub fn has_clean_items(self) -> bool {
        matches!(
            self,
            Self::Clean | Self::Installers | Self::LargeFiles | Self::BuildFolders
        )
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SortKey {
    #[default]
    Size,
    Name,
    Modified,
}

#[derive(Clone, Debug)]
pub struct Busy {
    pub label: String,
    pub mutating: bool,
    /// Sections this scan loads; a stopped scan leaves them unloaded.
    pub sections: Vec<Section>,
}

#[derive(Clone, Debug)]
pub struct Notice {
    pub title: String,
    pub lines: Vec<String>,
}

#[derive(Clone, Debug)]
pub enum PendingAction {
    Items(Section),
    Applications,
    Disk(DiskItem, PathBuf),
}

#[derive(Clone, Debug)]
pub struct Confirmation {
    pub title: String,
    pub button: String,
    pub notes: Vec<String>,
    /// Every path that will move, with its size, so the dialog shows the
    /// exact list the operation receives.
    pub moves: Vec<(String, u64)>,
    pub action: PendingAction,
}

#[derive(Clone, Debug)]
pub struct ActivityEntry {
    pub when: SystemTime,
    pub summary: String,
    pub details: Vec<String>,
    pub failed: bool,
}

type ItemScan = fn(&Path, &Control) -> ScanResult<CleanItem>;

#[derive(Debug)]
enum WorkerMessage {
    Items(u64, Section, ScanResult<CleanItem>),
    Applications(u64, ScanResult<AppItem>),
    Overview(
        u64,
        ScanResult<CleanItem>,
        ScanResult<AppItem>,
        Option<VolumeUsage>,
    ),
    Breakdown(u64, Option<VolumeUsage>, Breakdown),
    Disk(u64, PathBuf, DiskScan),
    Storage(u64, StorageExtras, Option<VolumeUsage>),
    Operation(u64, Section, Vec<(String, u64)>, OperationResult),
}

pub type Waker = Arc<dyn Fn() + Send + Sync>;

pub struct State {
    pub home: PathBuf,
    pub section: Section,
    pub clean_items: Vec<CleanItem>,
    pub app_items: Vec<AppItem>,
    pub installer_items: Vec<CleanItem>,
    pub large_items: Vec<CleanItem>,
    pub build_items: Vec<CleanItem>,
    /// Installed app names by lowercased bundle ID, for cache folder names.
    pub app_names: HashMap<String, String>,
    pub disk_items: Vec<DiskItem>,
    pub disk_root: PathBuf,
    /// Index of the item shown in the inspector, per section.
    pub focus: [Option<usize>; Section::COUNT],
    pub sort: [SortKey; Section::COUNT],
    pub filter: [String; Section::COUNT],
    pub status: Option<SystemStatus>,
    pub extras: Option<StorageExtras>,
    /// Per-volume use of the startup disk, read with the Overview scan.
    pub volumes: Option<VolumeUsage>,
    /// The Data volume split into your files, developer tools, backups and
    /// more, after the person asks for it.
    pub breakdown: Option<Breakdown>,
    pub message: String,
    pub busy: Option<Busy>,
    pub confirmation: Option<Confirmation>,
    pub notice: Option<Notice>,
    pub scan_errors: [Vec<String>; Section::COUNT],
    pub activity: Vec<ActivityEntry>,
    pub path_draft: String,
    /// Folder sizes the disk map already measured, shared with workers.
    pub size_cache: Arc<SizeCache>,
    loaded: [bool; Section::COUNT],
    /// Set when the person stops a scan, so it does not restart by itself.
    stopped: [bool; Section::COUNT],
    status_read_at: Option<Instant>,
    operation_summary: Option<String>,
    sender: Sender<WorkerMessage>,
    receiver: Receiver<WorkerMessage>,
    generation: u64,
    /// The running job's stop flag and progress counts.
    control: Arc<Control>,
    /// The running scan's progress, refreshed every frame. It never moves
    /// backward, even when a later phase adds items to measure.
    pub progress: ScanProgress,
    waker: Option<Waker>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ScanProgress {
    /// 0.0 to 1.0, or None before the scan knows how much it has to do.
    pub fraction: Option<f32>,
    /// Files and folders read so far.
    pub checked: u64,
}

impl State {
    pub fn new(home: PathBuf) -> Self {
        let (sender, receiver) = mpsc::channel();
        Self {
            disk_root: home.clone(),
            path_draft: tilde_path(&home, &home),
            home,
            section: Section::Overview,
            clean_items: Vec::new(),
            app_items: Vec::new(),
            installer_items: Vec::new(),
            large_items: Vec::new(),
            build_items: Vec::new(),
            app_names: HashMap::new(),
            disk_items: Vec::new(),
            focus: [None; Section::COUNT],
            sort: [SortKey::Size; Section::COUNT],
            filter: Default::default(),
            status: None,
            extras: None,
            volumes: None,
            breakdown: None,
            message: "Nothing moves until you review it.".to_string(),
            busy: None,
            confirmation: None,
            notice: None,
            scan_errors: Default::default(),
            activity: Vec::new(),
            size_cache: Arc::default(),
            loaded: [false; Section::COUNT],
            stopped: [false; Section::COUNT],
            status_read_at: None,
            operation_summary: None,
            sender,
            receiver,
            generation: 0,
            control: Arc::default(),
            progress: ScanProgress::default(),
            waker: None,
        }
    }

    /// Lets worker threads wake the UI when they finish.
    pub fn set_waker(&mut self, waker: Waker) {
        self.waker = Some(waker);
    }

    pub fn is_loaded(&self, section: Section) -> bool {
        self.loaded[section.index()]
    }

    pub fn is_stopped(&self, section: Section) -> bool {
        self.stopped[section.index()]
    }

    pub fn select_section(&mut self, section: Section) {
        if section != self.section && !self.message.starts_with("Moved") {
            // Scan counts belong to the view that produced them; Trash
            // results stay visible until the next one.
            self.message.clear();
        }
        self.section = section;
    }

    /// The `CleanItem` list behind a section, if it has one.
    pub fn items(&self, section: Section) -> Option<&Vec<CleanItem>> {
        match section {
            Section::Clean => Some(&self.clean_items),
            Section::Installers => Some(&self.installer_items),
            Section::LargeFiles => Some(&self.large_items),
            Section::BuildFolders => Some(&self.build_items),
            _ => None,
        }
    }

    fn items_mut(&mut self, section: Section) -> Option<&mut Vec<CleanItem>> {
        match section {
            Section::Clean => Some(&mut self.clean_items),
            Section::Installers => Some(&mut self.installer_items),
            Section::LargeFiles => Some(&mut self.large_items),
            Section::BuildFolders => Some(&mut self.build_items),
            _ => None,
        }
    }

    fn wants_auto_load(&self, section: Section) -> bool {
        !self.is_loaded(section) && !self.is_stopped(section)
    }

    /// Runs every frame: loads automatic sections once no job is running and
    /// keeps system metrics fresh. Returns how soon it wants another frame.
    pub fn update(&mut self) -> Option<Duration> {
        self.poll_worker();
        let now = self.control.progress();
        // Until the job reports a count, keep what is shown.
        if self.busy.as_ref().is_some_and(|busy| !busy.mutating) && now != Default::default() {
            self.progress = ScanProgress {
                fraction: now
                    .fraction()
                    .map(|fraction| fraction.max(self.progress.fraction.unwrap_or(0.0))),
                checked: now.checked,
            };
        }
        let wants_status = matches!(
            self.section,
            Section::Status | Section::Overview | Section::Storage
        );
        if wants_status
            && self
                .status_read_at
                .is_none_or(|read_at| read_at.elapsed() >= STATUS_REFRESH)
        {
            self.read_status();
        }
        if self.busy.is_none() {
            if self.section == Section::Overview {
                let clean = self.wants_auto_load(Section::Clean);
                let apps = self.wants_auto_load(Section::Applications);
                match (clean, apps) {
                    (true, true) => self.scan_overview(),
                    (true, false) => self.start_scan(Section::Clean),
                    (false, true) => self.start_scan(Section::Applications),
                    (false, false) => {}
                }
            } else if self.section.loads_automatically() && self.wants_auto_load(self.section) {
                self.start_scan(self.section);
            }
        }
        if self.busy.is_some() {
            Some(Duration::from_millis(120))
        } else if wants_status {
            Some(STATUS_REFRESH)
        } else {
            None
        }
    }

    pub fn read_status(&mut self) {
        self.status = Some(system::collect());
        self.status_read_at = Some(Instant::now());
    }

    pub fn can_start(&self) -> bool {
        self.busy.is_none()
    }

    /// True while a scan that loads `section` runs.
    pub fn is_scanning(&self, section: Section) -> bool {
        self.busy
            .as_ref()
            .is_some_and(|busy| !busy.mutating && busy.sections.contains(&section))
    }

    /// Starts the scan for `section` at the person's request. Ignored while
    /// another job runs.
    pub fn scan(&mut self, section: Section) {
        if !self.can_start() {
            return;
        }
        self.stopped[section.index()] = false;
        if section == Section::Disk {
            // "Measure again" means fresh numbers, not remembered ones.
            self.size_cache = Arc::default();
        }
        self.start_scan(section);
    }

    fn start_scan(&mut self, section: Section) {
        if !self.can_start() {
            return;
        }
        self.loaded[section.index()] = true;
        let home = self.home.clone();
        let list: Option<(&str, ItemScan)> = match section {
            Section::Clean => Some(("Scanning caches and logs", scan_cleanup)),
            Section::Installers => Some(("Looking for installer files", scan_installers)),
            Section::LargeFiles => Some(("Looking for large old files", |home, cancel| {
                scan_large_files(home, LARGE_FILE_BYTES, LARGE_FILE_AGE, cancel)
            })),
            Section::BuildFolders => Some(("Looking for build folders", scan_build_folders)),
            _ => None,
        };
        if let Some((label, scan)) = list {
            self.spawn(label, vec![section], move |generation, cancel| {
                WorkerMessage::Items(generation, section, scan(&home, cancel))
            });
            return;
        }
        match section {
            Section::Applications => self.spawn(
                "Measuring installed apps",
                vec![section],
                move |generation, cancel| {
                    WorkerMessage::Applications(generation, scan_apps(&home, cancel))
                },
            ),
            Section::Disk => self.open_folder(self.disk_root.clone()),
            Section::Status | Section::Storage => {
                self.read_status();
                self.spawn(
                    "Reading Trash and snapshots",
                    vec![section],
                    move |generation, cancel| {
                        WorkerMessage::Storage(
                            generation,
                            system::storage_extras(&home, cancel),
                            system::volume_usage(),
                        )
                    },
                );
            }
            _ => {}
        }
    }

    /// Scans caches and apps at the same time, and reads how the startup
    /// disk's volumes are used.
    fn scan_overview(&mut self) {
        let home = self.home.clone();
        self.loaded[Section::Clean.index()] = true;
        self.loaded[Section::Applications.index()] = true;
        self.spawn(
            "Scanning caches and apps",
            vec![Section::Clean, Section::Applications],
            move |generation, cancel| {
                let (clean, apps, volumes) = std::thread::scope(|scope| {
                    let apps = scope.spawn(|| scan_apps(&home, cancel));
                    let volumes = scope.spawn(system::volume_usage);
                    let clean = scan_cleanup(&home, cancel);
                    (
                        clean,
                        apps.join().unwrap_or_default(),
                        volumes.join().ok().flatten(),
                    )
                });
                WorkerMessage::Overview(generation, clean, apps, volumes)
            },
        );
    }

    /// Measures your files, developer tools, backups, Mail and Messages,
    /// and the Trash, so the storage bar can name more of the used space.
    /// Reads Desktop, Documents and Downloads, so it waits for a click.
    pub fn measure_storage(&mut self) {
        if !self.can_start() {
            return;
        }
        let home = self.home.clone();
        self.spawn(
            "Measuring storage",
            vec![Section::Overview],
            move |generation, control| {
                let volumes = system::volume_usage();
                WorkerMessage::Breakdown(
                    generation,
                    volumes,
                    system::storage_breakdown(&home, control),
                )
            },
        );
    }

    pub fn open_folder(&mut self, root: PathBuf) {
        if !self.can_start() {
            return;
        }
        // Clear the old listing so a cancelled scan never shows one folder's
        // entries under another folder's path.
        self.loaded[Section::Disk.index()] = true;
        self.stopped[Section::Disk.index()] = false;
        self.disk_items.clear();
        self.focus[Section::Disk.index()] = None;
        self.path_draft = tilde_path(&root, &self.home);
        self.disk_root = root.clone();
        let label = format!("Measuring {}", tilde_path(&root, &self.home));
        let cache = Arc::clone(&self.size_cache);
        self.spawn(label, vec![Section::Disk], move |generation, cancel| {
            let result = analyze_directory(&root, &cache, cancel);
            WorkerMessage::Disk(generation, root, result)
        });
    }

    /// Opens the folder typed in the path field. Accepts `~` for home.
    pub fn open_typed_folder(&mut self) {
        let entered = self.path_draft.trim();
        let path = match entered.strip_prefix('~') {
            Some(rest) if rest.is_empty() || rest.starts_with('/') => {
                self.home.join(rest.trim_start_matches('/'))
            }
            _ => PathBuf::from(entered),
        };
        if path.is_absolute() && path.is_dir() {
            self.open_folder(path);
        } else {
            self.message = format!("{entered} is not a folder. Enter a full path or start with ~.");
        }
    }

    pub fn open_parent(&mut self) {
        if let Some(parent) = self.disk_root.parent() {
            self.open_folder(parent.to_path_buf());
        }
    }

    pub fn cancel_scan(&mut self) {
        let Some(busy) = self.busy.as_ref().filter(|busy| !busy.mutating) else {
            return;
        };
        // The walk stops at its next folder; its result carries the old
        // generation and is discarded.
        self.control.stop();
        for section in busy.sections.clone() {
            self.loaded[section.index()] = false;
            self.stopped[section.index()] = true;
        }
        self.generation += 1;
        self.busy = None;
        self.message = match self.operation_summary.take() {
            Some(summary) => format!("{summary} Rescan stopped; scan again to refresh the list."),
            None => "Scan stopped. Scan again when you are ready.".to_string(),
        };
    }

    /// False while a Trash operation runs; the window must stay open.
    pub fn can_quit(&self) -> bool {
        !self.busy.as_ref().is_some_and(|busy| busy.mutating)
    }

    fn spawn<F>(&mut self, label: impl Into<String>, sections: Vec<Section>, work: F)
    where
        F: FnOnce(u64, &Control) -> WorkerMessage + Send + 'static,
    {
        self.start_job(label.into(), false, sections, work);
    }

    fn start_job<F>(&mut self, label: String, mutating: bool, sections: Vec<Section>, work: F)
    where
        F: FnOnce(u64, &Control) -> WorkerMessage + Send + 'static,
    {
        self.generation += 1;
        let generation = self.generation;
        let sender = self.sender.clone();
        let waker = self.waker.clone();
        let control = Arc::new(Control::default());
        self.control = Arc::clone(&control);
        self.progress = ScanProgress::default();
        self.busy = Some(Busy {
            label,
            mutating,
            sections,
        });
        std::thread::spawn(move || {
            let _ = sender.send(work(generation, &control));
            if let Some(waker) = waker {
                waker();
            }
        });
    }

    pub fn poll_worker(&mut self) {
        while let Ok(message) = self.receiver.try_recv() {
            let generation = match &message {
                WorkerMessage::Items(generation, ..)
                | WorkerMessage::Applications(generation, _)
                | WorkerMessage::Overview(generation, ..)
                | WorkerMessage::Breakdown(generation, ..)
                | WorkerMessage::Disk(generation, ..)
                | WorkerMessage::Storage(generation, ..)
                | WorkerMessage::Operation(generation, ..) => *generation,
            };
            if generation != self.generation {
                continue;
            }
            self.busy = None;
            // The finished job's counts must not show under the next one.
            self.control = Arc::default();
            let message = match message {
                WorkerMessage::Items(_, section, result) => {
                    let count = result.items.len();
                    if let Some(items) = self.items_mut(section) {
                        *items = result.items;
                    }
                    self.finish_scan(section, count, result.errors)
                }
                WorkerMessage::Applications(_, result) => self.finish_apps(result),
                WorkerMessage::Overview(_, clean, apps, volumes) => {
                    let count = clean.items.len();
                    self.clean_items = clean.items;
                    let clean = self.finish_scan(Section::Clean, count, clean.errors);
                    let apps = self.finish_apps(apps);
                    self.keep_volumes(volumes);
                    format!("Caches: {clean} Apps: {apps}")
                }
                WorkerMessage::Breakdown(_, volumes, breakdown) => {
                    self.keep_volumes(volumes);
                    self.breakdown = Some(breakdown);
                    "Storage measured.".to_string()
                }
                WorkerMessage::Disk(_, root, result) => self.finish_disk(&root, result),
                WorkerMessage::Storage(_, extras, volumes) => {
                    self.extras = Some(extras);
                    self.keep_volumes(volumes);
                    "Storage and system metrics refreshed.".to_string()
                }
                WorkerMessage::Operation(_, section, moves, result) => {
                    self.finish_operation(section, moves, result);
                    continue;
                }
            };
            // A rescan after a Trash operation keeps the operation's outcome
            // visible instead of replacing it with the scan count.
            self.message = match self.operation_summary.take() {
                Some(summary) => format!("{summary} List refreshed."),
                None => message,
            };
        }
    }

    /// A failed read keeps the last good volume figures.
    fn keep_volumes(&mut self, volumes: Option<VolumeUsage>) {
        if volumes.is_some() {
            self.volumes = volumes;
        }
    }

    fn finish_apps(&mut self, result: ScanResult<AppItem>) -> String {
        self.app_items = result.items;
        self.app_names = app_names(&self.app_items);
        self.finish_scan(Section::Applications, self.app_items.len(), result.errors)
    }

    fn finish_disk(&mut self, root: &Path, result: DiskScan) -> String {
        let index = Section::Disk.index();
        match result {
            Ok((scan, folders)) => {
                self.disk_items = scan.items;
                Arc::make_mut(&mut self.size_cache).extend(folders);
                self.focus[index] = (!self.disk_items.is_empty()).then_some(0);
                let mut text = format!(
                    "{} in {}",
                    plural(self.disk_items.len(), "item", "items"),
                    tilde_path(root, &self.home)
                );
                if !scan.errors.is_empty() {
                    text.push_str(&format!(
                        "; {} could not be read",
                        plural(scan.errors.len(), "item", "items")
                    ));
                }
                self.scan_errors[index] = scan.errors;
                text + "."
            }
            Err(error) => {
                self.disk_items.clear();
                self.focus[index] = None;
                self.scan_errors[index].clear();
                format!("Cannot open {}: {error}", tilde_path(root, &self.home))
            }
        }
    }

    fn finish_scan(&mut self, section: Section, count: usize, errors: Vec<String>) -> String {
        let index = section.index();
        let len = self.len(section);
        self.focus[index] = (len > 0).then(|| self.focus[index].unwrap_or(0).min(len - 1));
        let text = if errors.is_empty() {
            format!("Found {}.", plural(count, "item", "items"))
        } else {
            format!(
                "Found {}; {} could not be read.",
                plural(count, "item", "items"),
                plural(errors.len(), "location", "locations")
            )
        };
        self.scan_errors[index] = errors;
        text
    }

    /// Forgets remembered sizes of the moved paths, what was inside them,
    /// and every folder that contained them.
    fn invalidate(&mut self, moved: &[PathBuf]) {
        if moved.is_empty() {
            return;
        }
        Arc::make_mut(&mut self.size_cache).retain(|cached, _| {
            !moved
                .iter()
                .any(|path| path.starts_with(cached) || cached.starts_with(path))
        });
        for app in &mut self.app_items {
            app.related.retain(|related| !moved.contains(&related.path));
            app.data.retain(|data| !moved.contains(&data.path));
        }
    }

    fn finish_operation(
        &mut self,
        section: Section,
        moves: Vec<(String, u64)>,
        result: OperationResult,
    ) {
        let moved = plural(result.moved, "item", "items");
        let summary = if !result.failures.is_empty() {
            format!("Moved {moved} to Trash; {} failed.", result.failures.len())
        } else {
            format!("Moved {moved} ({}) to Trash.", format_bytes(result.bytes))
        };
        let mut details: Vec<String> = moves.into_iter().map(|(path, _)| path).collect();
        details.extend(result.notes.iter().cloned());
        details.extend(
            result
                .failures
                .iter()
                .map(|failure| format!("Failed: {failure}")),
        );
        if !result.failures.is_empty() || !result.notes.is_empty() {
            let mut lines = result.failures.clone();
            lines.extend(result.notes.clone());
            self.notice = Some(Notice {
                title: "Some items need your attention".to_string(),
                lines,
            });
        }
        self.activity.insert(
            0,
            ActivityEntry {
                when: SystemTime::now(),
                summary: summary.clone(),
                details,
                failed: !result.failures.is_empty(),
            },
        );
        self.invalidate(&result.moved_paths);
        if result.moved > 0 {
            // Moved items changed the totals, so measure again before showing them.
            self.breakdown = None;
        }
        self.message = summary.clone();
        if section == Section::Disk {
            // Only the moved item changed, so drop it instead of measuring
            // the whole folder again.
            self.disk_items
                .retain(|item| !result.moved_paths.contains(&item.path));
            let index = Section::Disk.index();
            self.focus[index] = self.focus[index].filter(|focus| *focus < self.disk_items.len());
            return;
        }
        self.operation_summary = Some(summary);
        // Rescan the list the operation changed, even if the person switched
        // sections while it ran.
        self.scan(section);
    }

    pub fn len(&self, section: Section) -> usize {
        match section {
            Section::Applications => self.app_items.len(),
            Section::Disk => self.disk_items.len(),
            Section::Activity => self.activity.len(),
            section => self.items(section).map_or(0, Vec::len),
        }
    }

    pub fn focus_index(&self) -> Option<usize> {
        self.focus[self.section.index()].filter(|index| *index < self.len(self.section))
    }

    pub fn set_focus(&mut self, index: usize) {
        self.focus[self.section.index()] = Some(index);
    }

    pub fn is_selected(&self, index: usize) -> bool {
        match self.section {
            Section::Applications => self.app_items.get(index).is_some_and(|app| app.selected),
            section => self
                .items(section)
                .and_then(|items| items.get(index))
                .is_some_and(|item| item.selected),
        }
    }

    /// Selectable lists for the current section. Disk items are reviewed one
    /// at a time, so they have no selection.
    pub fn toggle(&mut self, index: usize) {
        match self.section {
            Section::Applications => {
                if let Some(app) = self.app_items.get_mut(index) {
                    app.selected = !app.selected;
                }
            }
            section => {
                if let Some(item) = self
                    .items_mut(section)
                    .and_then(|items| items.get_mut(index))
                {
                    item.selected = !item.selected;
                }
            }
        }
    }

    /// Ticks or clears "also move settings and app data" for one app.
    pub fn toggle_app_data(&mut self, index: usize) {
        if let Some(app) = self.app_items.get_mut(index) {
            app.include_data = !app.include_data;
        }
    }

    /// Selects every item in `indices` unless all are already selected, in
    /// which case it clears them.
    pub fn toggle_all(&mut self, indices: &[usize]) {
        let select = indices.iter().any(|&index| !self.is_selected(index));
        for &index in indices {
            if self.is_selected(index) != select {
                self.toggle(index);
            }
        }
    }

    pub fn selected_total(&self, section: Section) -> (usize, u64) {
        let sum = |items: &mut dyn Iterator<Item = u64>| {
            items.fold((0, 0), |(count, bytes), item| (count + 1, bytes + item))
        };
        match section {
            Section::Applications => sum(&mut self
                .app_items
                .iter()
                .filter(|item| item.selected)
                .map(|item| item.bytes)),
            section => self.items(section).map_or((0, 0), |items| {
                sum(&mut items
                    .iter()
                    .filter(|item| item.selected)
                    .map(|item| item.bytes))
            }),
        }
    }

    /// Opens the review dialog for the current section's selection.
    pub fn review(&mut self) {
        if !self.can_start() {
            return;
        }
        let home = self.home.clone();
        let tilde = |path: &Path| tilde_path(path, &home);
        let confirmation = match self.section {
            Section::Applications => {
                let mut moves = Vec::new();
                let mut notes = Vec::new();
                let mut with_data = false;
                for app in self.app_items.iter().filter(|app| app.selected) {
                    moves.push((tilde(&app.path), app.bytes));
                    let related = related_app_data(&self.app_items, app);
                    moves.extend(
                        related
                            .paths
                            .iter()
                            .map(|related| (tilde(&related.path), related.bytes)),
                    );
                    with_data |= app.include_data && !related.kept_shared;
                    if related.kept_shared {
                        notes.push(format!(
                            "{} shares caches and app data with another installed copy, so they stay.",
                            app.name
                        ));
                    }
                }
                if moves.is_empty() {
                    self.message = "Select at least one app first.".to_string();
                    return;
                }
                notes.push(if with_data {
                    "Settings and app data move too for the apps where you ticked them. Documents stay.".to_string()
                } else {
                    "Settings, documents, and other personal data stay.".to_string()
                });
                notes.push("Quit an app before you uninstall it.".to_string());
                let apps = self.app_items.iter().filter(|app| app.selected).count();
                Confirmation {
                    title: format!("Uninstall {}?", plural(apps, "app", "apps")),
                    button: "Move to Trash".to_string(),
                    notes,
                    moves,
                    action: PendingAction::Applications,
                }
            }
            Section::Disk => {
                let Some(item) = self
                    .focus_index()
                    .and_then(|index| self.disk_items.get(index))
                else {
                    self.message = "Select an item on the map first.".to_string();
                    return;
                };
                if is_protected_disk_path(&item.path, &home) {
                    self.message = format!(
                        "{} is protected. MacCleanUp does not move it to Trash.",
                        item.name
                    );
                    return;
                }
                Confirmation {
                    title: format!("Move {} to Trash?", item.name),
                    button: "Move to Trash".to_string(),
                    notes: vec![if item.is_directory {
                        "The whole folder and everything in it moves.".to_string()
                    } else {
                        "Only this file moves.".to_string()
                    }],
                    moves: vec![(tilde(&item.path), item.bytes)],
                    action: PendingAction::Disk(item.clone(), self.disk_root.clone()),
                }
            }
            section => {
                let Some(items) = self.items(section) else {
                    return;
                };
                let moves: Vec<_> = items
                    .iter()
                    .filter(|item| item.selected)
                    .map(|item| (tilde(&item.path), item.bytes))
                    .collect();
                if moves.is_empty() {
                    self.message = "Select at least one item first.".to_string();
                    return;
                }
                let note = match section {
                    Section::Installers => "Installed apps keep working without their installers.",
                    Section::LargeFiles => "Check each file before you move it. MacCleanUp cannot tell whether you still need it.",
                    Section::BuildFolders => "The projects' source code stays. Build again to make these folders again.",
                    _ => "Apps rebuild caches when they need them. Close an app first if you are clearing its cache.",
                };
                Confirmation {
                    title: format!("Move {} to Trash?", plural(moves.len(), "item", "items")),
                    button: "Move to Trash".to_string(),
                    notes: vec![note.to_string()],
                    moves,
                    action: PendingAction::Items(section),
                }
            }
        };
        self.confirmation = Some(confirmation);
    }

    pub fn cancel_confirmation(&mut self) {
        self.confirmation = None;
    }

    /// Runs the reviewed operation. Only the dialog's Trash button calls this.
    pub fn accept_confirmation(&mut self) {
        if !self.can_start() {
            return;
        }
        let Some(confirmation) = self.confirmation.take() else {
            return;
        };
        let home = self.home.clone();
        let moves = confirmation.moves;
        let (label, section): (&str, Section) = match &confirmation.action {
            PendingAction::Items(Section::Clean) => {
                ("Moving caches and logs to Trash", Section::Clean)
            }
            PendingAction::Items(section) => ("Moving items to Trash", *section),
            PendingAction::Applications => ("Moving apps to Trash", Section::Applications),
            PendingAction::Disk(..) => ("Moving item to Trash", Section::Disk),
        };
        let items = self.items(section).cloned().unwrap_or_default();
        let apps = self.app_items.clone();
        let action = confirmation.action;
        self.start_job(label.to_string(), true, Vec::new(), move |generation, _| {
            let result = match action {
                PendingAction::Items(_) => trash_items(&items, &home),
                PendingAction::Applications => trash_apps(&apps, &home),
                PendingAction::Disk(item, root) => trash_disk_item(&item, &root, &home),
            };
            WorkerMessage::Operation(generation, section, moves, result)
        });
    }

    #[cfg(test)]
    pub fn inject_operation(&mut self, section: Section, result: OperationResult) {
        self.finish_operation(section, Vec::new(), result);
    }
}

/// Installed app names by lowercased bundle ID.
pub fn app_names(apps: &[AppItem]) -> HashMap<String, String> {
    apps.iter()
        .filter_map(|app| Some((app.bundle_id.as_ref()?.to_lowercase(), app.name.clone())))
        .collect()
}

pub fn plural(count: usize, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

/// Shows paths inside the home folder as `~/…`.
pub fn tilde_path(path: &Path, home: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

/// "Today", "Yesterday", "3 days ago", "5 weeks ago", "2 years ago".
pub fn age(modified: Option<SystemTime>) -> String {
    let Some(modified) = modified else {
        return "Unknown".to_string();
    };
    let Ok(elapsed) = SystemTime::now().duration_since(modified) else {
        return "Today".to_string();
    };
    let days = elapsed.as_secs() / 86_400;
    match days {
        0 => "Today".to_string(),
        1 => "Yesterday".to_string(),
        2..=13 => format!("{days} days ago"),
        14..=59 => format!("{} weeks ago", days / 7),
        60..=729 => format!("{} months ago", days / 30),
        _ => format!("{} years ago", days / 365),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use maccleanup::model::RelatedPath;

    fn fixture_home() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/home")
    }

    pub fn wait_idle(state: &mut State) {
        for _ in 0..12_000 {
            state.poll_worker();
            if state.busy.is_none() {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("worker did not finish");
    }

    #[test]
    fn overview_scans_caches_and_apps_together_but_not_private_folders() {
        let mut state = State::new(fixture_home());
        state.update();
        assert_eq!(
            state.busy.as_ref().unwrap().label,
            "Scanning caches and apps"
        );
        wait_idle(&mut state);
        assert_eq!(state.clean_items.len(), 5);
        assert!(state.is_loaded(Section::Clean) && state.is_loaded(Section::Applications));
        state.update();
        assert!(state.busy.is_none(), "nothing else starts on its own");
        assert!(!state.is_loaded(Section::Installers));
        assert!(!state.is_loaded(Section::LargeFiles));
        assert!(!state.is_loaded(Section::BuildFolders));
        assert!(!state.is_loaded(Section::Disk));
        assert!(state.status.is_some());
        for section in [
            Section::Installers,
            Section::LargeFiles,
            Section::BuildFolders,
        ] {
            state.select_section(section);
            state.update();
            assert!(
                state.busy.is_none(),
                "{section:?} waits for an explicit scan"
            );
        }
        state.scan(Section::Installers);
        wait_idle(&mut state);
        assert_eq!(state.installer_items.len(), 4);
        state.scan(Section::BuildFolders);
        wait_idle(&mut state);
        assert_eq!(state.build_items.len(), 2);
    }

    #[test]
    fn a_stopped_scan_stays_stopped_until_the_person_scans_again() {
        let mut state = State::new(fixture_home());
        state.update();
        let control = Arc::clone(&state.control);
        state.cancel_scan();
        assert!(control.is_stopped(), "the walk is told to stop");
        assert!(!state.is_loaded(Section::Clean));
        assert!(!state.is_loaded(Section::Applications));
        assert!(state.is_stopped(Section::Clean));
        state.update();
        assert!(state.busy.is_none(), "the overview does not restart it");
        state.scan(Section::Clean);
        assert!(!state.is_stopped(Section::Clean));
        wait_idle(&mut state);
        assert!(state.is_loaded(Section::Clean));
    }

    #[test]
    fn progress_resets_per_scan_and_never_moves_backward() {
        let mut state = State::new(fixture_home());
        state.progress.fraction = Some(0.9);
        state.scan(Section::Installers);
        assert_eq!(
            state.progress,
            ScanProgress::default(),
            "a new scan starts at zero"
        );
        assert!(state.is_scanning(Section::Installers));
        assert!(!state.is_scanning(Section::Clean));
        let mut seen = Vec::new();
        while state.busy.is_some() {
            state.update();
            seen.extend(state.progress.fraction);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(seen.windows(2).all(|pair| pair[0] <= pair[1]), "{seen:?}");
        assert!(seen.iter().all(|fraction| (0.0..=1.0).contains(fraction)));
        assert!(!state.is_scanning(Section::Installers));
    }

    #[test]
    fn operation_keeps_summary_rescans_origin_and_logs_activity() {
        let mut state = State::new(fixture_home());
        state.section = Section::Activity;
        state.inject_operation(
            Section::Installers,
            OperationResult {
                moved: 2,
                bytes: 2_000,
                ..OperationResult::default()
            },
        );
        assert!(state.busy.is_some(), "installer rescan started");
        wait_idle(&mut state);
        assert_eq!(state.installer_items.len(), 4);
        assert_eq!(
            state.message,
            "Moved 2 items (2.00 KB) to Trash. List refreshed."
        );
        assert_eq!(state.activity.len(), 1);
        assert!(state.notice.is_none());
    }

    #[test]
    fn failures_open_a_notice_and_mark_the_activity_entry() {
        let mut state = State::new(fixture_home());
        state.inject_operation(
            Section::Clean,
            OperationResult {
                failures: vec!["/x: denied".to_string()],
                ..OperationResult::default()
            },
        );
        assert!(state.notice.is_some());
        assert!(state.activity[0].failed);
        assert!(state.message.contains("1 failed"));
    }

    #[test]
    fn no_second_job_starts_while_one_runs_and_trash_cannot_be_cancelled() {
        let mut state = State::new(fixture_home());
        state.busy = Some(Busy {
            label: "Moving".to_string(),
            mutating: true,
            sections: Vec::new(),
        });
        let generation = state.generation;
        state.scan(Section::Clean);
        state.open_folder(fixture_home());
        state.cancel_scan();
        assert_eq!(state.generation, generation);
        assert!(!state.can_quit());

        state.busy = Some(Busy {
            label: "Scanning".to_string(),
            mutating: false,
            sections: vec![Section::Clean],
        });
        state.cancel_scan();
        assert!(state.busy.is_none());
        assert!(state.generation > generation);
        assert!(state.can_quit());
    }

    #[test]
    fn cancelled_folder_scan_clears_the_previous_listing() {
        let home = fixture_home();
        let mut state = State::new(home.clone());
        state.open_folder(home.clone());
        wait_idle(&mut state);
        assert!(!state.disk_items.is_empty());
        state.open_folder(home.join("Library"));
        state.cancel_scan();
        assert!(state.disk_items.is_empty());
        assert!(!state.is_loaded(Section::Disk));
        assert_eq!(state.disk_root, home.join("Library"));
    }

    #[test]
    fn disk_map_reuses_measured_folders_until_something_moves() {
        let home = fixture_home();
        let library = home.join("Library");
        let mut state = State::new(home.clone());
        Arc::make_mut(&mut state.size_cache).insert(
            library.clone(),
            Size {
                bytes: 987_654,
                complete: true,
            },
        );
        state.open_folder(home.clone());
        wait_idle(&mut state);
        let size = |state: &State| {
            state
                .disk_items
                .iter()
                .find(|item| item.path == library)
                .unwrap()
                .bytes
        };
        assert_eq!(size(&state), 987_654, "remembered size used");
        assert!(
            state.size_cache.contains_key(&home),
            "the opened folder is remembered"
        );

        state.inject_operation(
            Section::Installers,
            OperationResult {
                moved: 1,
                moved_paths: vec![library.join("Caches/com.example.gone")],
                ..OperationResult::default()
            },
        );
        assert!(!state.size_cache.contains_key(&library));
        assert!(!state.size_cache.contains_key(&home));
        wait_idle(&mut state);
        state.open_folder(home.clone());
        wait_idle(&mut state);
        assert_ne!(size(&state), 987_654);

        Arc::make_mut(&mut state.size_cache).insert(library.clone(), Size::default());
        state.scan(Section::Disk);
        assert!(state.size_cache.is_empty(), "Measure again forgets sizes");
        wait_idle(&mut state);
    }

    #[test]
    fn a_disk_map_move_drops_the_item_without_measuring_again() {
        let home = fixture_home();
        let mut state = State::new(home.clone());
        state.open_folder(home.join("Library"));
        wait_idle(&mut state);
        let moved = home.join("Library/Logs");
        assert!(state.disk_items.iter().any(|item| item.path == moved));
        let count = state.disk_items.len();
        state.inject_operation(
            Section::Disk,
            OperationResult {
                moved: 1,
                moved_paths: vec![moved.clone()],
                ..OperationResult::default()
            },
        );
        assert!(state.busy.is_none(), "no rescan");
        assert_eq!(state.disk_items.len(), count - 1);
        assert!(!state.disk_items.iter().any(|item| item.path == moved));
    }

    #[test]
    fn typed_folder_accepts_tilde_and_rejects_missing_paths() {
        let home = fixture_home();
        let mut state = State::new(home.clone());
        state.path_draft = "~/Library".to_string();
        state.open_typed_folder();
        assert_eq!(state.disk_root, home.join("Library"));
        wait_idle(&mut state);
        assert_eq!(state.disk_items.len(), 5);
        state.path_draft = "relative/path".to_string();
        state.open_typed_folder();
        assert!(state.busy.is_none());
        assert!(state.message.contains("is not a folder"));
    }

    #[test]
    fn protected_disk_items_are_refused_before_the_dialog() {
        let home = fixture_home();
        let mut state = State::new(home.clone());
        state.section = Section::Disk;
        state.disk_items = vec![DiskItem {
            name: "Library".to_string(),
            path: home.join("Library"),
            bytes: 1,
            is_directory: true,
            complete: true,
            identity: None,
            modified: None,
        }];
        state.set_focus(0);
        state.review();
        assert!(state.confirmation.is_none());
        assert!(state.message.contains("protected"));
    }

    #[test]
    fn uninstall_review_lists_the_same_paths_the_operation_moves() {
        let home = fixture_home();
        let mut state = State::new(home.clone());
        state.section = Section::Applications;
        state.app_items = vec![AppItem {
            name: "Example".to_string(),
            bundle_id: Some("com.example.alpha".to_string()),
            version: Some("1.0".to_string()),
            path: home.join("Applications/Example.app"),
            bytes: 100,
            selected: true,
            related: vec![RelatedPath {
                path: home.join("Library/Caches/com.example.alpha"),
                bytes: 10,
                identity: None,
            }],
            data: vec![RelatedPath {
                path: home.join("Library/Preferences/com.example.alpha.plist"),
                bytes: 4,
                identity: None,
            }],
            ..AppItem::default()
        }];
        state.review();
        let paths = |state: &State| -> Vec<String> {
            state
                .confirmation
                .as_ref()
                .unwrap()
                .moves
                .iter()
                .map(|(path, _)| path.clone())
                .collect()
        };
        assert_eq!(paths(&state).len(), 2);
        assert!(paths(&state)[1].ends_with("Library/Caches/com.example.alpha"));
        assert_eq!(
            state.confirmation.as_ref().unwrap().title,
            "Uninstall 1 app?"
        );
        state.cancel_confirmation();

        state.toggle_app_data(0);
        state.review();
        assert_eq!(paths(&state).len(), 3);
        assert!(paths(&state)[2].ends_with("Preferences/com.example.alpha.plist"));
        state.cancel_confirmation();
        assert!(state.confirmation.is_none());
        assert!(state.busy.is_none());
    }

    #[test]
    fn toggle_all_respects_the_visible_subset() {
        let mut state = State::new(fixture_home());
        state.section = Section::Clean;
        state.scan(Section::Clean);
        wait_idle(&mut state);
        state.toggle_all(&[0, 1]);
        assert_eq!(state.selected_total(Section::Clean).0, 2);
        state.toggle_all(&[0, 1]);
        assert_eq!(state.selected_total(Section::Clean).0, 0);
        state.toggle(3);
        assert_eq!(state.selected_total(Section::Clean).0, 1);
    }

    #[test]
    fn system_view_reads_trash_and_snapshots_in_the_background() {
        let mut state = State::new(fixture_home());
        state.select_section(Section::Status);
        state.update();
        assert_eq!(
            state.busy.as_ref().unwrap().label,
            "Reading Trash and snapshots"
        );
        wait_idle(&mut state);
        let extras = state.extras.expect("extras");
        assert_eq!(extras.trash, None, "the fixture has no readable Trash");
    }

    #[test]
    fn helpers_format_counts_paths_and_ages() {
        assert_eq!(plural(1, "item", "items"), "1 item");
        assert_eq!(plural(3, "item", "items"), "3 items");
        let home = Path::new("/Users/test");
        assert_eq!(tilde_path(home, home), "~");
        assert_eq!(tilde_path(Path::new("/System"), home), "/System");
        let now = SystemTime::now();
        assert_eq!(age(Some(now)), "Today");
        assert_eq!(
            age(Some(now - Duration::from_secs(3 * 86_400))),
            "3 days ago"
        );
        assert_eq!(
            age(Some(now - Duration::from_secs(400 * 86_400))),
            "13 months ago"
        );
        assert_eq!(age(None), "Unknown");
    }
}
