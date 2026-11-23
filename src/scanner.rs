use crate::model::{AppItem, CleanCategory, CleanItem, DiskItem, ItemIdentity};
use objc2::runtime::AnyObject;
use objc2_foundation::{ns_string, NSDictionary, NSString};
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Size {
    pub bytes: u64,
    pub complete: bool,
}

pub fn scan_cleanup(home: &Path) -> ScanResult<CleanItem> {
    let mut result = ScanResult::default();
    let groups = [
        (home.join("Library/Caches"), CleanCategory::Cache),
        (home.join("Library/Logs"), CleanCategory::Log),
        (home.join(".cache"), CleanCategory::Temporary),
        (
            home.join("Library/Developer/Xcode/DerivedData"),
            CleanCategory::Developer,
        ),
        (
            home.join("Library/Developer/CoreSimulator/Caches"),
            CleanCategory::Developer,
        ),
        (home.join(".npm/_cacache"), CleanCategory::Developer),
    ];

    for (root, category) in groups {
        add_children(&root, category, &mut result);
    }

    result.items.sort_by(|a, b| {
        b.bytes
            .cmp(&a.bytes)
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.path.cmp(&b.path))
    });
    result
}

fn add_children(root: &Path, category: CleanCategory, result: &mut ScanResult<CleanItem>) {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => {
            result.errors.push(format!("{}: {error}", root.display()));
            return;
        }
    };

    for entry in entries {
        match entry {
            Ok(entry) => add_single(&entry.path(), category, result),
            Err(error) => result.errors.push(format!("{}: {error}", root.display())),
        }
    }
}

fn add_single(path: &Path, category: CleanCategory, result: &mut ScanResult<CleanItem>) {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) => {
            if error.kind() != std::io::ErrorKind::NotFound {
                result.errors.push(format!("{}: {error}", path.display()));
            }
            return;
        }
    };
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let size = measure(path);
    result.items.push(CleanItem {
        id: path.to_string_lossy().into_owned(),
        category,
        name,
        path: path.to_path_buf(),
        bytes: size.bytes,
        selected: false,
        complete: size.complete,
        identity: item_identity(path),
        modified: metadata.modified().ok(),
    });
}

const INSTALLER_EXTENSIONS: [&str; 5] = ["dmg", "pkg", "mpkg", "iso", "xip"];

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
pub fn scan_installers(home: &Path) -> ScanResult<CleanItem> {
    let mut result = ScanResult::default();
    for root in installer_roots(home) {
        collect_installers(&root, 2, &mut result);
    }
    result.items.sort_by(|a, b| {
        b.bytes
            .cmp(&a.bytes)
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.path.cmp(&b.path))
    });
    result
}

fn collect_installers(directory: &Path, depth: usize, result: &mut ScanResult<CleanItem>) {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => {
            result
                .errors
                .push(format!("{}: {error}", directory.display()));
            return;
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                result
                    .errors
                    .push(format!("{}: {error}", directory.display()));
                continue;
            }
        };
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_file() && has_installer_extension(&path) {
            add_single(&path, CleanCategory::Installer, result);
        } else if file_type.is_dir()
            && depth > 1
            && path.extension().is_none_or(|extension| extension != "app")
        {
            collect_installers(&path, depth - 1, result);
        }
    }
}

fn has_installer_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            INSTALLER_EXTENSIONS
                .iter()
                .any(|known| extension.eq_ignore_ascii_case(known))
        })
}

pub fn is_safe_installer_path(path: &Path, home: &Path) -> bool {
    if !has_safe_path_text(path)
        || !path.is_absolute()
        || !has_installer_extension(path)
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
        || !fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_file())
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
    let resolved = path
        .parent()
        .and_then(|parent| fs::canonicalize(parent).ok())
        .zip(path.file_name())
        .map(|(parent, name)| parent.join(name));
    let (Some(path), Ok(home)) = (resolved, fs::canonicalize(home)) else {
        return true;
    };
    // macOS volumes are usually case-insensitive, so `~/library/keychains`
    // names the same folder as `~/Library/Keychains`. Compare lowercased text.
    let (Some(path), Some(home)) = (path.to_str(), home.to_str()) else {
        return true;
    };
    let (path, home) = (path.to_lowercase(), home.to_lowercase());
    let Ok(relative) = Path::new(&path).strip_prefix(&home) else {
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

pub fn scan_apps(home: &Path) -> ScanResult<AppItem> {
    let mut result = ScanResult::default();
    for root in [PathBuf::from("/Applications"), home.join("Applications")] {
        collect_apps(&root, 1, &mut result);
    }
    result.items.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.path.cmp(&b.path))
    });
    result
}

fn collect_apps(root: &Path, depth: usize, result: &mut ScanResult<AppItem>) {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => {
            result.errors.push(format!("{}: {error}", root.display()));
            return;
        }
    };

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                result.errors.push(format!("{}: {error}", root.display()));
                continue;
            }
        };
        let path = entry.path();
        if path.extension().is_some_and(|extension| extension == "app") {
            let size = measure(&path);
            let identity = item_identity(&path);
            let name = path
                .file_stem()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Application".to_string());
            let (bundle_id, version) = read_bundle_info(&path);
            let modified = fs::symlink_metadata(&path)
                .and_then(|metadata| metadata.modified())
                .ok();
            result.items.push(AppItem {
                bundle_id,
                version,
                name,
                path,
                bytes: size.bytes,
                selected: false,
                identity,
                modified,
            });
        } else if depth > 0
            && entry.file_type().is_ok_and(|file_type| file_type.is_dir())
            && !entry.file_name().to_string_lossy().starts_with('.')
        {
            collect_apps(&path, depth - 1, result);
        }
    }
}

/// Reads `CFBundleIdentifier` and `CFBundleShortVersionString` from the app's Info.plist.
fn read_bundle_info(app_path: &Path) -> (Option<String>, Option<String>) {
    let plist = app_path.join("Contents/Info.plist");
    #[allow(deprecated)]
    objc2::rc::autoreleasepool(|_| {
        let path = NSString::from_str(&plist.to_string_lossy());
        let Some(dictionary) =
            (unsafe { NSDictionary::<NSString, AnyObject>::dictionaryWithContentsOfFile(&path) })
        else {
            return (None, None);
        };
        let text = |key: &NSString| {
            dictionary
                .objectForKey(key)
                .and_then(|value| value.downcast::<NSString>().ok())
                .map(|value| value.to_string())
        };
        (
            text(ns_string!("CFBundleIdentifier")),
            text(ns_string!("CFBundleShortVersionString")),
        )
    })
}

pub fn analyze_directory(root: &Path) -> std::io::Result<Vec<DiskItem>> {
    let mut items = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        let size = measure(&path);
        let identity = item_identity(&path);
        let is_directory = entry
            .file_type()
            .map(|file_type| file_type.is_dir())
            .unwrap_or(false);
        let modified = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .ok();
        items.push(DiskItem {
            name: entry.file_name().to_string_lossy().into_owned(),
            path,
            bytes: size.bytes,
            is_directory,
            complete: size.complete,
            identity,
            modified,
        });
    }
    items.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.name.cmp(&b.name)));
    Ok(items)
}

pub fn measure(path: &Path) -> Size {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(_) => return Size::default(),
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Size {
            bytes: metadata.len(),
            complete: true,
        };
    }

    let device = metadata.dev();
    let mut bytes: u64 = 0;
    let mut complete = true;
    let mut stack = vec![path.to_path_buf()];
    while let Some(directory) = stack.pop() {
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(_) => {
                complete = false;
                continue;
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => {
                    complete = false;
                    continue;
                }
            };
            let metadata = match fs::symlink_metadata(entry.path()) {
                Ok(metadata) => metadata,
                Err(_) => {
                    complete = false;
                    continue;
                }
            };
            bytes = bytes.saturating_add(metadata.len());
            if metadata.is_dir() && !metadata.file_type().is_symlink() && metadata.dev() == device {
                stack.push(entry.path());
            } else if metadata.is_dir() && metadata.dev() != device {
                complete = false;
            }
        }
    }
    Size { bytes, complete }
}

pub fn cleanup_roots(home: &Path) -> Vec<PathBuf> {
    vec![
        home.join("Library/Caches"),
        home.join("Library/Logs"),
        home.join(".cache"),
        home.join("Library/Developer/Xcode/DerivedData"),
        home.join("Library/Developer/CoreSimulator/Caches"),
        home.join(".npm/_cacache"),
    ]
}

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

pub fn is_safe_cleanup_path(path: &Path, home: &Path) -> bool {
    if !has_safe_path_text(path)
        || !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
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
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
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
mod tests {
    use super::*;

    fn fixture_home() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/home")
    }

    #[test]
    fn cleanup_scan_finds_fixture_caches_without_selecting_them() {
        let result = scan_cleanup(&fixture_home());
        assert_eq!(result.items.len(), 5, "errors: {:?}", result.errors);
        assert!(result.items.iter().all(|item| !item.selected));
        assert!(result.items.iter().all(|item| item.identity.is_some()));
        assert_eq!(result.items.iter().map(|item| item.bytes).sum::<u64>(), 52);
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
    fn analyzer_reports_file_and_directory_sizes() {
        let items = analyze_directory(&fixture_home().join("Library/Caches")).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items.iter().map(|item| item.bytes).sum::<u64>(), 30);
        assert!(items.iter().all(|item| item.is_directory && item.complete));
    }

    #[test]
    fn app_inventory_reads_fixture_bundle_metadata() {
        let home = fixture_home();
        let mut result = ScanResult::default();
        collect_apps(&home.join("Applications"), 0, &mut result);
        assert_eq!(result.items.len(), 1, "errors: {:?}", result.errors);
        assert_eq!(result.items[0].name, "Example");
        assert_eq!(
            result.items[0].bundle_id.as_deref(),
            Some("com.example.fixture")
        );
        assert!(result.items[0].identity.is_some());
        assert!(is_safe_app_path(&result.items[0].path, &home));
    }

    #[test]
    fn installer_scan_finds_images_two_levels_deep_only() {
        let home = fixture_home();
        let result = scan_installers(&home);
        let mut names: Vec<_> = result.items.iter().map(|item| item.name.as_str()).collect();
        names.sort_unstable();
        assert_eq!(names, ["Setup.dmg", "Tool.PKG", "Xcode.xip"], "{result:?}");
        assert_eq!(result.items.iter().map(|item| item.bytes).sum::<u64>(), 13);
        assert!(result
            .items
            .iter()
            .all(|item| !item.selected && item.identity.is_some()));
        assert!(result.errors.is_empty());
    }

    #[test]
    fn installer_guard_accepts_found_files_and_rejects_everything_else() {
        let home = fixture_home();
        assert!(is_safe_installer_path(
            &home.join("Downloads/Setup.dmg"),
            &home
        ));
        assert!(is_safe_installer_path(
            &home.join("Downloads/sub/Tool.PKG"),
            &home
        ));
        for rejected in [
            "Downloads/sub/deeper/TooDeep.iso",
            "Downloads/Link.dmg",
            "Downloads/readme.txt",
            "Downloads/missing.dmg",
            "Downloads/../Downloads/Setup.dmg",
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
}
