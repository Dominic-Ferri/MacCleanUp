//! Application state and background jobs, independent of drawing.

use maccleanup::model::{format_bytes, AppItem, CleanItem, DiskItem, OperationResult};
use maccleanup::scanner::{
    analyze_directory, is_protected_disk_path, scan_apps, scan_cleanup, scan_installers, ScanResult,
};
use maccleanup::system::{self, SystemStatus};
use maccleanup::trash::{
    related_app_data, trash_apps, trash_clean_items, trash_disk_item, trash_installers,
};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

pub const STATUS_REFRESH: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Section {
    Overview,
    Clean,
    Applications,
    Installers,
    Disk,
    Status,
    Activity,
}

impl Section {
    pub const ALL: [Self; 7] = [
        Self::Overview,
        Self::Clean,
        Self::Applications,
        Self::Installers,
        Self::Disk,
        Self::Status,
        Self::Activity,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Clean => "Caches and logs",
            Self::Applications => "Applications",
            Self::Installers => "Installer files",
            Self::Disk => "Disk map",
            Self::Status => "System",
            Self::Activity => "Activity",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Overview => "Where your storage goes, and what you can safely clear.",
            Self::Clean => "Tick the items you want to clear, then move them to Trash.",
            Self::Applications => "Uninstall moves the app and its exact caches and logs. Settings and documents stay.",
            Self::Installers => "Disk images and packages left in Downloads, Desktop, Documents, Public, and iCloud Downloads.",
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

    /// Sections whose scans start without a click. Installers read Desktop,
    /// Documents, and Downloads, and the disk map sizes whole folders; both
    /// can raise macOS privacy prompts, so they wait for an explicit scan.
    fn loads_automatically(self) -> bool {
        matches!(self, Self::Clean | Self::Applications)
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
}

#[derive(Clone, Debug)]
pub struct Notice {
    pub title: String,
    pub lines: Vec<String>,
}

#[derive(Clone, Debug)]
pub enum PendingAction {
    Clean,
    Applications,
    Installers,
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

#[derive(Debug)]
enum WorkerMessage {
    Clean(u64, ScanResult<CleanItem>),
    Applications(u64, ScanResult<AppItem>),
    Installers(u64, ScanResult<CleanItem>),
    Disk(u64, PathBuf, std::io::Result<Vec<DiskItem>>),
    Operation(u64, Section, Vec<(String, u64)>, OperationResult),
}

pub type Waker = Arc<dyn Fn() + Send + Sync>;

pub struct State {
    pub home: PathBuf,
    pub section: Section,
    pub clean_items: Vec<CleanItem>,
    pub app_items: Vec<AppItem>,
    pub installer_items: Vec<CleanItem>,
    pub disk_items: Vec<DiskItem>,
    pub disk_root: PathBuf,
    /// Index of the item shown in the inspector, per section.
    pub focus: [Option<usize>; 7],
    pub sort: [SortKey; 7],
    pub filter: [String; 7],
    pub status: Option<SystemStatus>,
    pub message: String,
    pub busy: Option<Busy>,
    pub confirmation: Option<Confirmation>,
    pub notice: Option<Notice>,
    pub scan_errors: [Vec<String>; 7],
    pub activity: Vec<ActivityEntry>,
    pub path_draft: String,
    loaded: [bool; 7],
    status_read_at: Option<Instant>,
    operation_summary: Option<String>,
    sender: Sender<WorkerMessage>,
    receiver: Receiver<WorkerMessage>,
    generation: u64,
    waker: Option<Waker>,
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
            disk_items: Vec::new(),
            focus: [None; 7],
            sort: [SortKey::Size; 7],
            filter: Default::default(),
            status: None,
            message: "Nothing moves until you review it.".to_string(),
            busy: None,
            confirmation: None,
            notice: None,
            scan_errors: Default::default(),
            activity: Vec::new(),
            loaded: [false; 7],
            status_read_at: None,
            operation_summary: None,
            sender,
            receiver,
            generation: 0,
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

    pub fn select_section(&mut self, section: Section) {
        if section != self.section && !self.message.starts_with("Moved") {
            // Scan counts belong to the view that produced them; Trash
            // results stay visible until the next one.
            self.message.clear();
        }
        self.section = section;
    }

    /// Runs every frame: loads automatic sections once no job is running and
    /// keeps system metrics fresh. Returns how soon it wants another frame.
    pub fn update(&mut self) -> Option<Duration> {
        self.poll_worker();
        let wants_status = matches!(self.section, Section::Status | Section::Overview);
        if wants_status
            && self
                .status_read_at
                .is_none_or(|read_at| read_at.elapsed() >= STATUS_REFRESH)
        {
            self.read_status();
        }
        if self.busy.is_none() {
            let pending = match self.section {
                Section::Overview => [Section::Clean, Section::Applications]
                    .into_iter()
                    .find(|section| !self.is_loaded(*section)),
                section if section.loads_automatically() && !self.is_loaded(section) => {
                    Some(section)
                }
                _ => None,
            };
            if let Some(section) = pending {
                self.scan(section);
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

    /// Starts the scan for `section`. Ignored while another job runs.
    pub fn scan(&mut self, section: Section) {
        if !self.can_start() {
            return;
        }
        self.loaded[section.index()] = true;
        let home = self.home.clone();
        match section {
            Section::Clean => self.spawn("Scanning caches and logs", false, move |generation| {
                WorkerMessage::Clean(generation, scan_cleanup(&home))
            }),
            Section::Applications => {
                self.spawn("Measuring installed apps", false, move |generation| {
                    WorkerMessage::Applications(generation, scan_apps(&home))
                })
            }
            Section::Installers => {
                self.spawn("Looking for installer files", false, move |generation| {
                    WorkerMessage::Installers(generation, scan_installers(&home))
                })
            }
            Section::Disk => self.open_folder(self.disk_root.clone()),
            Section::Status => {
                self.read_status();
                self.message = "System metrics refreshed.".to_string();
            }
            Section::Overview | Section::Activity => {}
        }
    }

    pub fn open_folder(&mut self, root: PathBuf) {
        if !self.can_start() {
            return;
        }
        // Clear the old listing so a cancelled scan never shows one folder's
        // entries under another folder's path.
        self.loaded[Section::Disk.index()] = true;
        self.disk_items.clear();
        self.focus[Section::Disk.index()] = None;
        self.path_draft = tilde_path(&root, &self.home);
        self.disk_root = root.clone();
        let label = format!("Measuring {}", tilde_path(&root, &self.home));
        self.spawn(label, false, move |generation| {
            let result = analyze_directory(&root);
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
        if !self.busy.as_ref().is_some_and(|busy| !busy.mutating) {
            return;
        }
        // The worker finishes in the background; its result carries the old
        // generation and is discarded.
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

    fn spawn<F>(&mut self, label: impl Into<String>, mutating: bool, work: F)
    where
        F: FnOnce(u64) -> WorkerMessage + Send + 'static,
    {
        self.generation += 1;
        let generation = self.generation;
        let sender = self.sender.clone();
        let waker = self.waker.clone();
        self.busy = Some(Busy {
            label: label.into(),
            mutating,
        });
        std::thread::spawn(move || {
            let _ = sender.send(work(generation));
            if let Some(waker) = waker {
                waker();
            }
        });
    }

    pub fn poll_worker(&mut self) {
        while let Ok(message) = self.receiver.try_recv() {
            let generation = match &message {
                WorkerMessage::Clean(generation, _)
                | WorkerMessage::Applications(generation, _)
                | WorkerMessage::Installers(generation, _)
                | WorkerMessage::Disk(generation, _, _)
                | WorkerMessage::Operation(generation, _, _, _) => *generation,
            };
            if generation != self.generation {
                continue;
            }
            self.busy = None;
            let message = match message {
                WorkerMessage::Clean(_, result) => {
                    self.clean_items = result.items;
                    self.finish_scan(Section::Clean, self.clean_items.len(), result.errors)
                }
                WorkerMessage::Applications(_, result) => {
                    self.app_items = result.items;
                    self.finish_scan(Section::Applications, self.app_items.len(), result.errors)
                }
                WorkerMessage::Installers(_, result) => {
                    self.installer_items = result.items;
                    self.finish_scan(
                        Section::Installers,
                        self.installer_items.len(),
                        result.errors,
                    )
                }
                WorkerMessage::Disk(_, root, result) => match result {
                    Ok(items) => {
                        self.disk_items = items;
                        self.focus[Section::Disk.index()] =
                            (!self.disk_items.is_empty()).then_some(0);
                        self.scan_errors[Section::Disk.index()].clear();
                        format!(
                            "{} in {}.",
                            plural(self.disk_items.len(), "item", "items"),
                            tilde_path(&root, &self.home)
                        )
                    }
                    Err(error) => {
                        self.disk_items.clear();
                        self.focus[Section::Disk.index()] = None;
                        format!("Cannot open {}: {error}", tilde_path(&root, &self.home))
                    }
                },
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
        self.message = summary.clone();
        self.operation_summary = Some(summary);
        // Rescan the list the operation changed, even if the user switched
        // sections while it ran.
        self.scan(section);
    }

    pub fn len(&self, section: Section) -> usize {
        match section {
            Section::Clean => self.clean_items.len(),
            Section::Applications => self.app_items.len(),
            Section::Installers => self.installer_items.len(),
            Section::Disk => self.disk_items.len(),
            Section::Activity => self.activity.len(),
            Section::Overview | Section::Status => 0,
        }
    }

    pub fn focus_index(&self) -> Option<usize> {
        self.focus[self.section.index()].filter(|index| *index < self.len(self.section))
    }

    pub fn set_focus(&mut self, index: usize) {
        self.focus[self.section.index()] = Some(index);
    }

    /// Selectable lists for the current section. Disk items are reviewed one
    /// at a time, so they have no selection.
    pub fn toggle(&mut self, index: usize) {
        match self.section {
            Section::Clean => toggle_one(&mut self.clean_items, index, |item| &mut item.selected),
            Section::Applications => {
                toggle_one(&mut self.app_items, index, |item| &mut item.selected)
            }
            Section::Installers => {
                toggle_one(&mut self.installer_items, index, |item| &mut item.selected)
            }
            _ => {}
        }
    }

    /// Selects every item in `indices` unless all are already selected, in
    /// which case it clears them.
    pub fn toggle_all(&mut self, indices: &[usize]) {
        fn apply<T>(items: &mut [T], indices: &[usize], flag: impl Fn(&mut T) -> &mut bool) {
            let mut select = false;
            for &index in indices {
                if let Some(item) = items.get_mut(index) {
                    select |= !*flag(item);
                }
            }
            for &index in indices {
                if let Some(item) = items.get_mut(index) {
                    *flag(item) = select;
                }
            }
        }
        match self.section {
            Section::Clean => apply(&mut self.clean_items, indices, |item| &mut item.selected),
            Section::Applications => apply(&mut self.app_items, indices, |item| &mut item.selected),
            Section::Installers => apply(&mut self.installer_items, indices, |item| {
                &mut item.selected
            }),
            _ => {}
        }
    }

    pub fn selected_total(&self, section: Section) -> (usize, u64) {
        let sum = |items: &mut dyn Iterator<Item = u64>| {
            items.fold((0, 0), |(count, bytes), item| (count + 1, bytes + item))
        };
        match section {
            Section::Clean => sum(&mut self
                .clean_items
                .iter()
                .filter(|item| item.selected)
                .map(|item| item.bytes)),
            Section::Applications => sum(&mut self
                .app_items
                .iter()
                .filter(|item| item.selected)
                .map(|item| item.bytes)),
            Section::Installers => sum(&mut self
                .installer_items
                .iter()
                .filter(|item| item.selected)
                .map(|item| item.bytes)),
            _ => (0, 0),
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
            Section::Clean | Section::Installers => {
                let installers = self.section == Section::Installers;
                let items = if installers {
                    &self.installer_items
                } else {
                    &self.clean_items
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
                Confirmation {
                    title: format!("Move {} to Trash?", plural(moves.len(), "item", "items")),
                    button: "Move to Trash".to_string(),
                    notes: vec![if installers {
                        "Installed apps keep working without their installers.".to_string()
                    } else {
                        "Apps rebuild caches when they need them. Close an app first if you are clearing its cache.".to_string()
                    }],
                    moves,
                    action: if installers {
                        PendingAction::Installers
                    } else {
                        PendingAction::Clean
                    },
                }
            }
            Section::Applications => {
                let mut moves = Vec::new();
                let mut notes = Vec::new();
                for app in self.app_items.iter().filter(|app| app.selected) {
                    moves.push((tilde(&app.path), app.bytes));
                    let related = related_app_data(&self.app_items, app, &home);
                    moves.extend(
                        related
                            .paths
                            .iter()
                            .map(|(path, bytes)| (tilde(path), *bytes)),
                    );
                    if related.kept_shared {
                        notes.push(format!(
                            "{} shares caches with another installed copy, so they stay.",
                            app.name
                        ));
                    }
                }
                if moves.is_empty() {
                    self.message = "Select at least one app first.".to_string();
                    return;
                }
                notes.push("Settings, documents, and other personal data stay.".to_string());
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
            _ => return,
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
            PendingAction::Clean => ("Moving caches and logs to Trash", Section::Clean),
            PendingAction::Applications => ("Moving apps to Trash", Section::Applications),
            PendingAction::Installers => ("Moving installers to Trash", Section::Installers),
            PendingAction::Disk(..) => ("Moving item to Trash", Section::Disk),
        };
        let clean = self.clean_items.clone();
        let apps = self.app_items.clone();
        let installers = self.installer_items.clone();
        let action = confirmation.action;
        self.spawn(label, true, move |generation| {
            let result = match action {
                PendingAction::Clean => trash_clean_items(&clean, &home),
                PendingAction::Applications => trash_apps(&apps, &home),
                PendingAction::Installers => trash_installers(&installers, &home),
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

fn toggle_one<T>(items: &mut [T], index: usize, flag: impl Fn(&mut T) -> &mut bool) {
    if let Some(item) = items.get_mut(index) {
        let value = flag(item);
        *value = !*value;
    }
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
    fn overview_loads_caches_then_apps_but_not_private_folders() {
        let mut state = State::new(fixture_home());
        state.update();
        assert_eq!(
            state.busy.as_ref().unwrap().label,
            "Scanning caches and logs"
        );
        wait_idle(&mut state);
        assert_eq!(state.clean_items.len(), 5);
        state.update();
        assert!(state.busy.is_some(), "apps scan follows");
        wait_idle(&mut state);
        state.update();
        assert!(state.busy.is_none());
        assert!(!state.is_loaded(Section::Installers));
        assert!(!state.is_loaded(Section::Disk));
        assert!(state.status.is_some());
        state.select_section(Section::Installers);
        state.update();
        assert!(state.busy.is_none(), "installers wait for an explicit scan");
        state.scan(Section::Installers);
        wait_idle(&mut state);
        assert_eq!(state.installer_items.len(), 3);
    }

    #[test]
    fn operation_keeps_summary_rescans_origin_and_logs_activity() {
        let mut state = State::new(fixture_home());
        state.section = Section::Status;
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
        assert_eq!(state.installer_items.len(), 3);
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
        assert_eq!(state.disk_root, home.join("Library"));
    }

    #[test]
    fn typed_folder_accepts_tilde_and_rejects_missing_paths() {
        let home = fixture_home();
        let mut state = State::new(home.clone());
        state.path_draft = "~/Library".to_string();
        state.open_typed_folder();
        assert_eq!(state.disk_root, home.join("Library"));
        wait_idle(&mut state);
        assert_eq!(state.disk_items.len(), 3);
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
    fn uninstall_review_lists_the_same_caches_the_operation_moves() {
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
            identity: None,
            modified: None,
        }];
        state.review();
        let confirmation = state.confirmation.as_ref().unwrap();
        let paths: Vec<_> = confirmation
            .moves
            .iter()
            .map(|(path, _)| path.as_str())
            .collect();
        assert_eq!(paths.len(), 2);
        assert!(paths[1].ends_with("Library/Caches/com.example.alpha"));
        assert_eq!(confirmation.title, "Uninstall 1 app?");
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
