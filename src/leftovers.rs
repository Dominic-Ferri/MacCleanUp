//! Leftovers: startup items whose program is gone, and settings, data, or
//! privileged helpers named after apps that are not installed.

use crate::model::{CleanCategory, CleanItem};
use crate::scanner::{
    finish_items, list_children, plist_text, sort_largest_first, with_plist, Control, ScanResult,
};
use objc2::runtime::AnyObject;
use objc2_foundation::{ns_string, NSArray, NSDictionary, NSString};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};

/// Where the scan looks. Production values come from `for_home`; tests pass
/// fixture folders.
#[derive(Clone, Debug)]
pub struct LeftoverRoots {
    pub user_library: PathBuf,
    pub system_library: PathBuf,
    /// Folders searched for installed apps and the helpers inside them.
    pub app_roots: Vec<PathBuf>,
    /// Bundles that count as installed wherever they are, such as the
    /// running copy of MacCleanUp.
    pub extra_bundles: Vec<PathBuf>,
}

impl LeftoverRoots {
    pub fn for_home(home: &Path) -> Self {
        Self {
            user_library: home.join("Library"),
            system_library: PathBuf::from("/Library"),
            app_roots: vec![
                PathBuf::from("/Applications"),
                home.join("Applications"),
                PathBuf::from("/Library/Application Support"),
            ],
            extra_bundles: running_bundle().into_iter().collect(),
        }
    }

    fn launch_folders(&self) -> [PathBuf; 3] {
        [
            self.user_library.join("LaunchAgents"),
            self.system_library.join("LaunchAgents"),
            self.system_library.join("LaunchDaemons"),
        ]
    }

    fn helper_folder(&self) -> PathBuf {
        self.system_library.join("PrivilegedHelperTools")
    }

    /// Folders whose entries are named after bundle IDs. Containers and
    /// Group Containers are left out: macOS asks for permission before
    /// another app's container can be read.
    fn data_folders(&self) -> [PathBuf; 6] {
        let user = &self.user_library;
        [
            user.join("Application Support"),
            user.join("Preferences"),
            user.join("HTTPStorages"),
            user.join("WebKit"),
            user.join("Saved Application State"),
            self.system_library.join("Application Support"),
        ]
    }
}

fn running_bundle() -> Option<PathBuf> {
    let executable = std::env::current_exe().ok()?;
    executable
        .ancestors()
        .find(|path| path.extension().is_some_and(|extension| extension == "app"))
        .map(Path::to_path_buf)
}

// Who owns a name --------------------------------------------------------------

/// Bundle IDs of installed apps and the helpers inside them, lowercased.
#[derive(Debug, Default)]
struct Owners {
    ids: HashSet<String>,
    /// The first two parts of each ID, such as `com.adobe`. A name with an
    /// installed vendor counts as owned: vendors share data between apps.
    vendors: HashSet<String>,
    /// Names of bundles whose Info.plist could not be read, such as an app
    /// with its permissions removed. Any name with this part counts as owned.
    tokens: HashSet<String>,
}

impl Owners {
    fn add_id(&mut self, id: &str) {
        let id = id.to_lowercase();
        self.vendors.insert(vendor(&id));
        self.ids.insert(id);
    }

    fn owns(&self, name: &str) -> bool {
        let name = name.to_lowercase();
        self.ids.contains(&name)
            || self.vendors.contains(&vendor(&name))
            || name.split('.').any(|part| self.tokens.contains(part))
    }
}

fn vendor(id: &str) -> String {
    id.split('.').take(2).collect::<Vec<_>>().join(".")
}

const BUNDLE_EXTENSIONS: [&str; 5] = ["app", "appex", "xpc", "bundle", "systemextension"];
/// Where apps keep the login items, helpers, and extensions they register.
const EMBEDDED_FOLDERS: [&str; 6] = [
    "Contents/Library/LoginItems",
    "Contents/Library/LaunchServices",
    "Contents/Library/SystemExtensions",
    "Contents/XPCServices",
    "Contents/PlugIns",
    "Contents/Helpers",
];
/// Deep enough for vendor folders such as
/// `/Applications/Utilities/Adobe Creative Cloud/ACC/Creative Cloud.app`.
const FOLDER_DEPTH: usize = 4;
/// Bundles inside bundles inside an app.
const EMBED_DEPTH: usize = 2;

fn installed_owners(roots: &LeftoverRoots, control: &Control) -> Owners {
    let mut owners = Owners::default();
    for root in &roots.app_roots {
        find_bundles(root, FOLDER_DEPTH, EMBED_DEPTH, &mut owners, control);
    }
    for bundle in &roots.extra_bundles {
        add_bundle(bundle, EMBED_DEPTH, &mut owners, control);
    }
    owners
}

fn is_bundle(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| BUNDLE_EXTENSIONS.contains(&extension))
}

/// Reads bundles in `folder` and up to `depth` folder levels below it,
/// without following symlinks.
fn find_bundles(folder: &Path, depth: usize, embed: usize, owners: &mut Owners, control: &Control) {
    if control.is_stopped() {
        return;
    }
    let Ok(entries) = fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|file_type| file_type.is_dir()) {
            continue;
        }
        let path = entry.path();
        if is_bundle(&path) {
            add_bundle(&path, embed, owners, control);
        } else if depth > 1 && !entry.file_name().to_string_lossy().starts_with('.') {
            find_bundles(&path, depth - 1, embed, owners, control);
        }
    }
}

fn add_bundle(bundle: &Path, embed: usize, owners: &mut Owners, control: &Control) {
    let info = bundle.join("Contents/Info.plist");
    let read = with_plist(&info, |dictionary| {
        (
            plist_text(dictionary, ns_string!("CFBundleIdentifier")),
            privileged_helpers(dictionary),
        )
    });
    match read {
        Some((id, helpers)) => {
            for id in id.iter().chain(&helpers) {
                owners.add_id(id);
            }
        }
        // A missing Info.plist means this is not a real bundle. Any other
        // failure means the bundle exists but cannot be checked.
        None if !matches!(fs::metadata(&info), Err(error) if error.kind() == ErrorKind::NotFound) =>
        {
            let token: String = bundle
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .chars()
                .filter(char::is_ascii_alphanumeric)
                .collect::<String>()
                .to_lowercase();
            if token.len() >= 3 {
                owners.tokens.insert(token);
            }
        }
        None => {}
    }
    if embed > 0 {
        for folder in EMBEDDED_FOLDERS {
            find_bundles(&bundle.join(folder), 1, embed - 1, owners, control);
        }
    }
}

/// The helper labels an app declares in `SMPrivilegedExecutables`.
fn privileged_helpers(dictionary: &NSDictionary<NSString, AnyObject>) -> Vec<String> {
    dictionary
        .objectForKey(ns_string!("SMPrivilegedExecutables"))
        .and_then(|value| value.downcast::<NSDictionary>().ok())
        .map(|helpers| {
            helpers
                .allKeys()
                .iter()
                .filter_map(|key| key.downcast::<NSString>().ok().map(|key| key.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

/// The bundle ID a Library entry is named after: at least three
/// dot-separated parts of letters, digits, `-` and `_`, and not Apple's.
/// An app group's settings (`group.com.vendor.app`) count as the app's.
fn bundle_name(file_name: &str) -> Option<&str> {
    let name = file_name
        .strip_suffix(".plist")
        .or_else(|| file_name.strip_suffix(".savedState"))
        .unwrap_or(file_name);
    let name = name.strip_prefix("group.").unwrap_or(name);
    let plain = name.split('.').count() >= 3
        && name.split('.').all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || "-_".contains(character))
        });
    // `regid.1986-12.com.adobe` style names are software ID tags that
    // licensing tools read; they name a vendor, not an app.
    let not_an_app = NOT_APP_VENDORS.contains(&vendor(&name.to_ascii_lowercase()).as_str())
        || name.starts_with("regid.");
    (plain && !is_apple(name) && !not_an_app).then_some(name)
}

/// Also catches prefixed forms such as `systemgroup.com.apple.icloud`.
fn is_apple(id: &str) -> bool {
    let id = id.to_ascii_lowercase();
    id.starts_with("com.apple.") || id.contains(".com.apple.")
}

/// Names that belong to macOS (CUPS printing) or to code libraries that many
/// apps include and that write settings under the library's own name, so no
/// one app can be said to own them.
const NOT_APP_VENDORS: [&str; 8] = [
    "org.cups",
    "org.sparkle-project",
    "com.firebase",
    "com.amplitude",
    "com.segment",
    "com.launchdarkly",
    "com.breakpad",
    "com.install4j",
];

// Startup items ----------------------------------------------------------------

struct LaunchJob {
    label: String,
    /// `Program`, or else the first of `ProgramArguments`.
    target: Option<PathBuf>,
}

fn read_launch_job(plist: &Path) -> Option<LaunchJob> {
    let (label, program) = with_plist(plist, |dictionary| {
        let program = plist_text(dictionary, ns_string!("Program")).or_else(|| {
            dictionary
                .objectForKey(ns_string!("ProgramArguments"))
                .and_then(|value| value.downcast::<NSArray>().ok())
                .and_then(|arguments| arguments.firstObject())
                .and_then(|first| first.downcast::<NSString>().ok())
                .map(|first| first.to_string())
        });
        (plist_text(dictionary, ns_string!("Label")), program)
    })?;
    Some(LaunchJob {
        label: label.unwrap_or_else(|| {
            plist
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        }),
        target: program.map(PathBuf::from),
    })
}

fn is_plist_file(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension == "plist")
        && fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_file())
}

/// The program a startup item runs, when it is certainly gone: an absolute
/// path that macOS reports as not found, on a disk that is connected. A
/// path that cannot be checked, for example inside a folder this user
/// cannot open, is not reported.
fn missing_target(job: &LaunchJob) -> Option<&Path> {
    let target = job.target.as_deref()?;
    if !target.is_absolute()
        || target
            .components()
            .any(|component| component == Component::ParentDir)
        || is_apple(&job.label)
        || on_disconnected_volume(target)
    {
        return None;
    }
    matches!(fs::symlink_metadata(target), Err(error) if error.kind() == ErrorKind::NotFound)
        .then_some(target)
}

fn on_disconnected_volume(target: &Path) -> bool {
    let mut parts = target.components();
    match (parts.next(), parts.next(), parts.next()) {
        (
            Some(Component::RootDir),
            Some(Component::Normal(volumes)),
            Some(Component::Normal(disk)),
        ) if volumes == "Volumes" => {
            fs::symlink_metadata(Path::new("/Volumes").join(disk)).is_err()
        }
        _ => false,
    }
}

// Scan -------------------------------------------------------------------------

pub fn scan_leftovers(home: &Path, control: &Control) -> ScanResult<CleanItem> {
    scan_leftovers_in(&LeftoverRoots::for_home(home), control)
}

/// Lists startup items whose program is gone first, then possible
/// leftovers, each group largest first. Nothing is selected.
pub fn scan_leftovers_in(roots: &LeftoverRoots, control: &Control) -> ScanResult<CleanItem> {
    let mut result = ScanResult::default();
    let owners = installed_owners(roots, control);
    let helpers = roots.helper_folder();
    let mut found = Vec::new();
    let mut notes = HashMap::new();
    let mut add = |path: PathBuf, category: CleanCategory, note: String| {
        notes.insert(path.clone(), note);
        found.push((path, category));
    };
    let unowned = |path: &Path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .and_then(bundle_name)
            .filter(|name| !owners.owns(name))
            .map(str::to_string)
    };

    for folder in roots.launch_folders() {
        list_children(&folder, &mut result.errors, |plist| {
            if !is_plist_file(&plist) {
                return;
            }
            let Some(job) = read_launch_job(&plist) else {
                return;
            };
            if let Some(target) = missing_target(&job) {
                let note = format!(
                    "{} starts {}, which is no longer on this Mac.",
                    job.label,
                    target.display()
                );
                add(plist, CleanCategory::LaunchItem, note);
            } else if let Some(target) = job
                .target
                .as_deref()
                .filter(|target| target.starts_with(&helpers) && *target != helpers)
            {
                if let Some(name) = unowned(&plist) {
                    let note = format!(
                        "Starts the helper {} with administrator rights. No installed app declares {name}.",
                        target.display()
                    );
                    add(plist, CleanCategory::Leftover, note);
                }
            }
        });
    }
    list_children(&helpers, &mut result.errors, |helper| {
        if let Some(name) = unowned(&helper) {
            let note = format!(
                "A background helper that runs with administrator rights. No installed app declares {name}."
            );
            add(helper, CleanCategory::Leftover, note);
        }
    });
    for folder in roots.data_folders() {
        list_children(&folder, &mut result.errors, |entry| {
            if let Some(name) = unowned(&entry) {
                let note = format!(
                    "No installed app uses {name}. A command-line tool may still use it, so check before you move it."
                );
                add(entry, CleanCategory::Leftover, note);
            }
        });
    }

    let mut result = finish_items(found, result, control);
    for item in &mut result.items {
        item.note = notes.remove(&item.path);
    }
    sort_largest_first(&mut result.items);
    result
        .items
        .sort_by_key(|item| item.category != CleanCategory::LaunchItem);
    result
}

// Guard ------------------------------------------------------------------------

pub fn is_safe_leftover_path(path: &Path, category: CleanCategory, home: &Path) -> bool {
    is_safe_leftover_path_in(path, category, &LeftoverRoots::for_home(home))
}

/// True only for an entry directly inside one of the scanned folders. A
/// startup item must still name a program that is gone; a possible leftover
/// must still be named like a bundle ID that is not Apple's.
pub fn is_safe_leftover_path_in(
    path: &Path,
    category: CleanCategory,
    roots: &LeftoverRoots,
) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    if !path.is_absolute()
        || path
            .components()
            .any(|component| component == Component::ParentDir)
        || path
            .to_str()
            .is_none_or(|text| text.chars().any(char::is_control))
        || fs::symlink_metadata(path).is_err()
    {
        return false;
    }
    let in_folder = |folders: &[PathBuf]| {
        path.parent()
            .and_then(|parent| fs::canonicalize(parent).ok())
            .is_some_and(|parent| {
                folders
                    .iter()
                    .any(|folder| fs::canonicalize(folder).is_ok_and(|folder| folder == parent))
            })
    };
    let launch = roots.launch_folders();
    match category {
        CleanCategory::LaunchItem => {
            is_plist_file(path)
                && in_folder(&launch)
                && read_launch_job(path).is_some_and(|job| missing_target(&job).is_some())
        }
        CleanCategory::Leftover => {
            let mut folders = launch.to_vec();
            folders.push(roots.helper_folder());
            folders.extend(roots.data_folders());
            bundle_name(name).is_some() && in_folder(&folders)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::OperationResult;
    use crate::scanner::item_identity;
    use crate::trash::trash_items;
    use std::os::unix::fs::PermissionsExt;

    fn scratch(name: &str) -> PathBuf {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/test-scratch")
            .join(name);
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn roots(base: &Path) -> LeftoverRoots {
        LeftoverRoots {
            user_library: base.join("user"),
            system_library: base.join("system"),
            app_roots: vec![base.join("apps")],
            extra_bundles: Vec::new(),
        }
    }

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn plist(entries: &str) -> String {
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict>{entries}</dict></plist>\n"
        )
    }

    fn job(label: &str, program: &Path) -> String {
        plist(&format!(
            "<key>Label</key><string>{label}</string><key>Program</key><string>{}</string>",
            program.display()
        ))
    }

    fn info(id: &str, extra: &str) -> String {
        plist(&format!(
            "<key>CFBundleIdentifier</key><string>{id}</string>{extra}"
        ))
    }

    fn set_mode(path: &Path, mode: u32) {
        if path.exists() {
            fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
        }
    }

    /// Builds the fixture tree under `base`. Reruns reuse it: locked folders
    /// are opened first, and a program a test made reappear is moved aside.
    fn build(base: &Path) {
        let (user, system, apps) = (base.join("user"), base.join("system"), base.join("apps"));
        set_mode(&base.join("locked"), 0o755);
        set_mode(&apps.join("Locked.app/Contents"), 0o755);
        let gone = base.join("missing/bin/agent");
        if gone.exists() {
            fs::rename(&gone, base.join("missing/bin/agent.aside")).unwrap();
        }

        let agents = user.join("LaunchAgents");
        write(
            &agents.join("com.gone.agent.plist"),
            &job("com.gone.agent", &gone),
        );
        write(&base.join("bin/present"), "");
        write(
            &agents.join("com.present.agent.plist"),
            &job("com.present.agent", &base.join("bin/present")),
        );
        write(&base.join("locked/app/bin"), "");
        write(
            &agents.join("com.locked.agent.plist"),
            &plist(&format!(
                "<key>Label</key><string>x.locked</string><key>ProgramArguments</key><array><string>{}</string></array>",
                base.join("locked/app/bin").display()
            )),
        );
        write(
            &agents.join("com.shell.agent.plist"),
            &plist("<key>ProgramArguments</key><array><string>/bin/sh</string><string>-c</string><string>true</string></array>"),
        );
        write(
            &agents.join("com.bundleprog.agent.plist"),
            &plist("<key>Label</key><string>x.b</string><key>BundleProgram</key><string>Contents/MacOS/gone</string>"),
        );
        write(
            &agents.join("com.corrupt.agent.plist"),
            "not a property list",
        );
        write(
            &agents.join("com.volume.agent.plist"),
            &job(
                "com.volume.agent",
                Path::new("/Volumes/NoSuchDisk-MacCleanUpTest/bin/agent"),
            ),
        );
        write(
            &agents.join("com.apple.fake.plist"),
            &job("com.apple.fake", &base.join("missing/bin/apple")),
        );

        let helpers = system.join("PrivilegedHelperTools");
        write(&helpers.join("com.gonevendor.helper"), "");
        write(&helpers.join("com.declared.helper"), "");
        write(&helpers.join("com.gonedir.tool/bin/tool"), "");
        write(
            &system.join("LaunchDaemons/com.gonedir.tool.plist"),
            &job(
                "com.gonedir.tool",
                &helpers.join("com.gonedir.tool/bin/tool"),
            ),
        );
        write(
            &system.join("LaunchDaemons/com.gonevendor.helper.plist"),
            &job(
                "com.gonevendor.helper",
                &helpers.join("com.gonevendor.helper"),
            ),
        );
        write(
            &system.join("LaunchDaemons/com.declared.helper.plist"),
            &job("com.declared.helper", &helpers.join("com.declared.helper")),
        );

        write(
            &apps.join("Owner.app/Contents/Info.plist"),
            &info(
                "com.owner.App",
                "<key>SMPrivilegedExecutables</key><dict><key>com.declared.helper</key><string>anchor apple</string></dict>",
            ),
        );
        write(
            &apps.join("Owner.app/Contents/Library/LoginItems/Login.app/Contents/Info.plist"),
            &info("net.loginonly.helper", ""),
        );
        write(
            &apps.join("Vendor/Nested/Deep.app/Contents/Info.plist"),
            &info("org.deep.app", ""),
        );
        fs::create_dir_all(apps.join("Locked.app/Contents")).unwrap();

        let support = user.join("Application Support");
        for name in [
            "com.gone.data",
            "com.owner.app",
            "com.owner.otherthing",
            "org.deep.app.cache",
            "net.loginonly.helper",
            "com.locked.data",
            "com.apple.something",
            "regid.1986-12.com.gonetags",
            "Some Folder",
        ] {
            fs::create_dir_all(support.join(name)).unwrap();
        }
        write(&user.join("Preferences/com.gone.prefs.plist"), "");
        for name in [
            "com.owner.app.plist",
            "group.com.owner.app.plist",
            "group.com.apple.mail.plist",
            "systemgroup.com.apple.icloud.shared.plist",
            "org.cups.PrintingPrefs.plist",
            "org.sparkle-project.Sparkle.Autoupdate.plist",
            "group.com.goneshared.app.plist",
        ] {
            write(&user.join("Preferences").join(name), "");
        }
        fs::create_dir_all(user.join("Saved Application State/com.gone.state.savedState")).unwrap();

        set_mode(&base.join("locked"), 0o000);
        set_mode(&apps.join("Locked.app/Contents"), 0o000);
    }

    fn unlock(base: &Path) {
        set_mode(&base.join("locked"), 0o755);
        set_mode(&base.join("apps/Locked.app/Contents"), 0o755);
    }

    fn relative(items: &[CleanItem], base: &Path, category: CleanCategory) -> Vec<String> {
        let mut paths: Vec<String> = items
            .iter()
            .filter(|item| item.category == category)
            .map(|item| item.path.strip_prefix(base).unwrap().display().to_string())
            .collect();
        paths.sort();
        paths
    }

    #[test]
    fn scan_lists_gone_programs_and_unowned_names_only() {
        let base = scratch("leftovers-scan");
        build(&base);
        let result = scan_leftovers_in(&roots(&base), &Control::default());
        unlock(&base);

        assert_eq!(
            relative(&result.items, &base, CleanCategory::LaunchItem),
            ["user/LaunchAgents/com.gone.agent.plist"],
            "only a program that is not found counts as gone: present, unreadable, shell, BundleProgram, corrupt, disconnected-disk and Apple items stay out"
        );
        assert_eq!(
            relative(&result.items, &base, CleanCategory::Leftover),
            [
                "system/LaunchDaemons/com.gonedir.tool.plist",
                "system/LaunchDaemons/com.gonevendor.helper.plist",
                "system/PrivilegedHelperTools/com.gonedir.tool",
                "system/PrivilegedHelperTools/com.gonevendor.helper",
                "user/Application Support/com.gone.data",
                "user/Preferences/com.gone.prefs.plist",
                "user/Preferences/group.com.goneshared.app.plist",
                "user/Saved Application State/com.gone.state.savedState",
            ],
            "exact IDs, app groups, vendor siblings, nested and embedded bundles, declared helpers and unreadable bundles count as owners; Apple, CUPS and library names are never listed"
        );
        assert_eq!(result.items[0].category, CleanCategory::LaunchItem);
        assert!(result.items.iter().all(|item| !item.selected));
        assert!(result.items.iter().all(|item| item.note.is_some()));
        assert!(result.items[0]
            .note
            .as_deref()
            .unwrap()
            .contains("missing/bin/agent"));
    }

    #[test]
    fn guard_refuses_anything_the_scan_would_not_list() {
        let base = scratch("leftovers-guard");
        build(&base);
        unlock(&base);
        let roots = roots(&base);
        let support = base.join("user/Application Support");
        let agent = base.join("user/LaunchAgents/com.gone.agent.plist");

        assert!(is_safe_leftover_path_in(
            &agent,
            CleanCategory::LaunchItem,
            &roots
        ));
        assert!(is_safe_leftover_path_in(
            &support.join("com.gone.data"),
            CleanCategory::Leftover,
            &roots
        ));
        assert!(is_safe_leftover_path_in(
            &base.join("system/PrivilegedHelperTools/com.gonevendor.helper"),
            CleanCategory::Leftover,
            &roots
        ));

        fs::create_dir_all(base.join("elsewhere/com.gone.data")).unwrap();
        let link = support.join("link");
        if fs::symlink_metadata(&link).is_err() {
            std::os::unix::fs::symlink(base.join("elsewhere"), &link).unwrap();
        }
        for (path, category) in [
            (support.join("com.apple.something"), CleanCategory::Leftover),
            (support.join("Some Folder"), CleanCategory::Leftover),
            (
                base.join("elsewhere/com.gone.data"),
                CleanCategory::Leftover,
            ),
            (link.join("com.gone.data"), CleanCategory::Leftover),
            (
                support.join("../Application Support/com.gone.data"),
                CleanCategory::Leftover,
            ),
            (support.join("com.gone.data"), CleanCategory::LaunchItem),
            (support.join("com.gone.data"), CleanCategory::Cache),
            (
                base.join("user/LaunchAgents/com.present.agent.plist"),
                CleanCategory::LaunchItem,
            ),
            (
                base.join("user/LaunchAgents/com.apple.fake.plist"),
                CleanCategory::LaunchItem,
            ),
        ] {
            assert!(
                !is_safe_leftover_path_in(&path, category, &roots),
                "{} as {category:?}",
                path.display()
            );
        }

        // A program that comes back between the scan and the move keeps its
        // startup item.
        let program = base.join("missing/bin/agent");
        write(&program, "");
        assert!(!is_safe_leftover_path_in(
            &agent,
            CleanCategory::LaunchItem,
            &roots
        ));
        fs::rename(&program, base.join("missing/bin/agent.aside")).unwrap();
        assert!(is_safe_leftover_path_in(
            &agent,
            CleanCategory::LaunchItem,
            &roots
        ));
    }

    #[test]
    fn fixture_leftovers_outside_the_real_library_never_move() {
        let base = scratch("leftovers-guard-move");
        build(&base);
        unlock(&base);
        let path = base.join("user/Application Support/com.gone.data");
        let items = vec![CleanItem {
            id: path.to_string_lossy().into_owned(),
            category: CleanCategory::Leftover,
            name: "com.gone.data".to_string(),
            identity: item_identity(&path),
            path: path.clone(),
            bytes: 0,
            selected: true,
            complete: true,
            modified: None,
            note: None,
        }];
        let home = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/home");
        let result = trash_items(&items, &home);
        assert_eq!(result.moved, 0);
        assert_eq!(result.failures.len(), 1);
        assert_ne!(result, OperationResult::default());
        assert!(path.exists());
    }

    /// Reads this Mac's real folders and prints what the scan lists. Moves
    /// nothing. Run with `cargo test live_leftovers -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn live_leftovers_scan_prints_and_moves_nothing() {
        let home = PathBuf::from(std::env::var("HOME").unwrap());
        let started = std::time::Instant::now();
        let result = scan_leftovers(&home, &Control::default());
        for item in &result.items {
            println!(
                "{:?}\t{}\t{}\t{}",
                item.category,
                item.bytes,
                item.path.display(),
                item.note.as_deref().unwrap_or_default()
            );
        }
        println!(
            "{} items, {} errors, {:?}",
            result.items.len(),
            result.errors.len(),
            started.elapsed()
        );
        for error in &result.errors {
            println!("error\t{error}");
        }
    }
}
