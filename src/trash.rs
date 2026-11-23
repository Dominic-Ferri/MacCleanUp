use crate::model::{AppItem, CleanCategory, CleanItem, DiskItem, OperationResult, RelatedPath};
use crate::scanner::{
    identity_matches, is_protected_disk_path, is_safe_app_data_path, is_safe_app_path,
    is_safe_build_folder_path, is_safe_cleanup_path, is_safe_installer_path,
    is_safe_large_file_path,
};
use objc2_foundation::{NSFileManager, NSString, NSURL};
use std::fs;
use std::path::Path;

/// The fixed-root guard each kind of list item must pass before it moves.
fn passes_guard(item: &CleanItem, home: &Path) -> bool {
    match item.category {
        CleanCategory::Cache
        | CleanCategory::Log
        | CleanCategory::Temporary
        | CleanCategory::Developer => is_safe_cleanup_path(&item.path, home),
        CleanCategory::Installer => is_safe_installer_path(&item.path, home),
        CleanCategory::LargeFile => is_safe_large_file_path(&item.path, home),
        CleanCategory::BuildFolder => is_safe_build_folder_path(&item.path, home),
    }
}

/// Moves the selected list items to Trash. Each item must still pass its
/// category's guard and match the identity recorded by the scan.
pub fn trash_items(items: &[CleanItem], home: &Path) -> OperationResult {
    let mut result = OperationResult::default();
    for item in items.iter().filter(|item| item.selected) {
        if !passes_guard(item, home) || !identity_matches(&item.path, item.identity) {
            result.failures.push(format!(
                "{}: path changed or failed protection",
                item.path.display()
            ));
            continue;
        }
        move_one(&item.path, item.bytes, &mut result);
    }
    result
}

pub fn trash_apps(apps: &[AppItem], home: &Path) -> OperationResult {
    let mut result = OperationResult::default();
    let current_executable = std::env::current_exe().ok();
    for app in apps.iter().filter(|app| app.selected) {
        if !is_safe_app_path(&app.path, home) || !identity_matches(&app.path, app.identity) {
            result.failures.push(format!(
                "{}: path changed or failed protection",
                app.path.display()
            ));
            continue;
        }
        if current_executable
            .as_ref()
            .is_some_and(|executable| executable.starts_with(&app.path))
        {
            result
                .failures
                .push(format!("{}: MacCleanUp cannot uninstall itself", app.name));
            continue;
        }

        if !move_one(&app.path, app.bytes, &mut result) {
            continue;
        }
        let related = related_app_data(apps, app);
        if related.kept_shared {
            if let Some(bundle_id) = &app.bundle_id {
                result.notes.push(format!(
                    "Kept caches and app data for {bundle_id}: another installed copy uses them."
                ));
            }
        }
        let bundle_id = app.bundle_id.as_deref().unwrap_or_default();
        for entry in &related.paths {
            match related_step(entry, home, bundle_id) {
                RelatedStep::Gone => {}
                RelatedStep::Refused => result.failures.push(format!(
                    "{}: changed since the scan, so it stays",
                    entry.path.display()
                )),
                RelatedStep::Move => {
                    move_one(&entry.path, entry.bytes, &mut result);
                }
            }
        }
    }
    result
}

#[derive(Debug, Eq, PartialEq)]
enum RelatedStep {
    /// Already gone, for example cleared from the caches list since the scan.
    Gone,
    Refused,
    Move,
}

/// Decides whether one scanned cache, log, or data path moves with its app:
/// it must still pass its guard and match the identity the scan recorded.
fn related_step(entry: &RelatedPath, home: &Path, bundle_id: &str) -> RelatedStep {
    if fs::symlink_metadata(&entry.path).is_err() {
        return RelatedStep::Gone;
    }
    let guarded = is_safe_cleanup_path(&entry.path, home)
        || is_safe_app_data_path(&entry.path, home, bundle_id);
    if guarded && identity_matches(&entry.path, entry.identity) {
        RelatedStep::Move
    } else {
        RelatedStep::Refused
    }
}

/// What uninstalling an app would also move.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RelatedData {
    pub paths: Vec<RelatedPath>,
    /// True when another installed copy that is not selected uses the same
    /// bundle ID, so its caches and data stay.
    pub kept_shared: bool,
}

/// Lists exactly what `trash_apps` moves with `app`, so a preview and the
/// operation cannot disagree: the scanned caches and logs, plus the app data
/// when `include_data` is ticked. `apps` supplies the current selection.
/// Reads nothing from disk, so drawing code may call it every frame.
pub fn related_app_data(apps: &[AppItem], app: &AppItem) -> RelatedData {
    if app.bundle_id.is_none() {
        return RelatedData::default();
    }
    if has_unselected_sibling(apps, app) {
        return RelatedData {
            paths: Vec::new(),
            kept_shared: true,
        };
    }
    let data: &[RelatedPath] = if app.include_data { &app.data } else { &[] };
    RelatedData {
        paths: app.related.iter().chain(data).cloned().collect(),
        kept_shared: false,
    }
}

pub fn trash_disk_item(item: &DiskItem, analyzed_root: &Path, home: &Path) -> OperationResult {
    let mut result = OperationResult::default();
    if is_protected_disk_path(&item.path, home) {
        result.failures.push(format!(
            "{}: protected folder; MacCleanUp does not move it",
            item.path.display()
        ));
        return result;
    }
    let valid = item.path.parent() == Some(analyzed_root)
        && item.path.file_name().is_some()
        && analyzed_root.is_absolute()
        && item.path.is_absolute()
        && identity_matches(&item.path, item.identity)
        && item
            .path
            .to_str()
            .is_some_and(|path| !path.chars().any(|character| character.is_control()));
    if !valid {
        result.failures.push(format!(
            "{}: path changed or is outside the analyzed folder",
            item.path.display()
        ));
        return result;
    }
    move_one(&item.path, item.bytes, &mut result);
    result
}

fn has_unselected_sibling(apps: &[AppItem], app: &AppItem) -> bool {
    apps.iter()
        .any(|other| !other.selected && other.path != app.path && other.bundle_id == app.bundle_id)
}

fn move_one(path: &Path, bytes: u64, result: &mut OperationResult) -> bool {
    match trash_path(path) {
        Ok(()) => {
            result.moved += 1;
            result.moved_paths.push(path.to_path_buf());
            result.bytes = result.bytes.saturating_add(bytes);
            true
        }
        Err(error) => {
            result.failures.push(format!("{}: {error}", path.display()));
            false
        }
    }
}

fn trash_path(path: &Path) -> Result<(), String> {
    objc2::rc::autoreleasepool(|_| {
        let path_string = NSString::from_str(&path.to_string_lossy());
        let url = NSURL::fileURLWithPath(&path_string);
        let manager = NSFileManager::defaultManager();
        manager
            .trashItemAtURL_resultingItemURL_error(&url, None)
            .map_err(|error| error.localizedDescription().to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner::item_identity;
    use std::path::PathBuf;

    fn fixture_home() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/home")
    }

    fn item(path: PathBuf, category: CleanCategory, selected: bool) -> CleanItem {
        CleanItem {
            id: path.to_string_lossy().into_owned(),
            category,
            name: "item".to_string(),
            identity: item_identity(&path),
            path,
            bytes: 10,
            selected,
            complete: true,
            modified: None,
        }
    }

    #[test]
    fn unselected_cleanup_items_do_nothing() {
        let home = fixture_home();
        let items = vec![item(
            home.join("Library/Caches/com.example.alpha"),
            CleanCategory::Cache,
            false,
        )];
        let result = trash_items(&items, &home);
        assert_eq!(result, OperationResult::default());
        assert!(items[0].path.exists());
    }

    #[test]
    fn missing_identity_blocks_selected_cleanup_item() {
        let home = fixture_home();
        let mut cache = item(
            home.join("Library/Caches/com.example.alpha"),
            CleanCategory::Cache,
            true,
        );
        cache.identity = None;
        let result = trash_items(&[cache.clone()], &home);
        assert_eq!(result.moved, 0);
        assert_eq!(result.failures.len(), 1);
        assert!(cache.path.exists());
    }

    #[test]
    fn each_category_uses_its_own_guard() {
        // Real fixture paths under the wrong category fail the guard. The
        // guard is called directly, so a regression cannot move a fixture.
        let home = fixture_home();
        let cases = [
            ("Downloads/Setup.dmg", CleanCategory::Cache),
            ("Library/Caches/com.example.alpha", CleanCategory::Installer),
            ("Library/Caches/com.example.alpha", CleanCategory::LargeFile),
            ("Documents/fake/target", CleanCategory::BuildFolder),
            ("Documents/proj", CleanCategory::BuildFolder),
            ("Library/Logs/example.log", CleanCategory::LargeFile),
        ];
        for (path, category) in cases {
            let selected = item(home.join(path), category, true);
            assert!(selected.path.exists(), "{path}");
            assert!(!passes_guard(&selected, &home), "{path}");
        }
        for (path, category) in [
            ("Library/Caches/com.example.alpha", CleanCategory::Cache),
            ("Downloads/Setup.dmg", CleanCategory::Installer),
            ("Downloads/Setup.dmg", CleanCategory::LargeFile),
            ("Documents/proj/target", CleanCategory::BuildFolder),
        ] {
            assert!(
                passes_guard(&item(home.join(path), category, false), &home),
                "{path}"
            );
        }
    }

    #[test]
    fn related_paths_move_only_with_a_matching_scan_identity() {
        let home = fixture_home();
        let id = "com.example.alpha";
        let entry = |path: &str| {
            let path = home.join(path);
            RelatedPath {
                identity: item_identity(&path),
                path,
                bytes: 1,
            }
        };
        let cache = entry("Library/Caches/com.example.alpha");
        let prefs = entry("Library/Preferences/com.example.alpha.plist");
        assert_eq!(related_step(&cache, &home, id), RelatedStep::Move);
        assert_eq!(related_step(&prefs, &home, id), RelatedStep::Move);
        assert_eq!(
            related_step(&entry("Library/Caches/com.example.gone"), &home, id),
            RelatedStep::Gone
        );
        // Another folder's identity: the path was replaced after the scan.
        let mut swapped = cache.clone();
        swapped.identity = item_identity(&home.join("Library/Caches/com.example.beta"));
        assert_eq!(related_step(&swapped, &home, id), RelatedStep::Refused);
        let mut unknown = cache;
        unknown.identity = None;
        assert_eq!(related_step(&unknown, &home, id), RelatedStep::Refused);
        // Outside every guarded root, even with a matching identity.
        assert_eq!(
            related_step(&entry("Downloads/Setup.dmg"), &home, id),
            RelatedStep::Refused
        );
        // App data under a different bundle ID than the app's.
        assert_eq!(
            related_step(&prefs, &home, "com.example.beta"),
            RelatedStep::Refused
        );
    }

    #[test]
    fn shared_bundle_cache_is_kept_only_for_unselected_siblings() {
        let make_app = |path: &str, selected: bool| AppItem {
            name: "Example".to_string(),
            bundle_id: Some("com.example.shared".to_string()),
            path: PathBuf::from(path),
            selected,
            ..AppItem::default()
        };
        let selected = make_app("/Applications/Example.app", true);
        let mut apps = vec![
            selected.clone(),
            make_app("/Applications/Other/Example.app", false),
        ];
        assert!(has_unselected_sibling(&apps, &selected));
        apps[1].selected = true;
        assert!(!has_unselected_sibling(&apps, &selected));
    }

    #[test]
    fn uninstall_preview_lists_scanned_caches_data_only_when_ticked_and_respects_shared_copies() {
        let home = fixture_home();
        let cache = RelatedPath {
            path: home.join("Library/Caches/com.example.alpha"),
            bytes: 10,
            identity: None,
        };
        let data = RelatedPath {
            path: home.join("Library/Preferences/com.example.alpha.plist"),
            bytes: 4,
            identity: None,
        };
        let mut app = AppItem {
            name: "Example".to_string(),
            bundle_id: Some("com.example.alpha".to_string()),
            path: home.join("Applications/Example.app"),
            related: vec![cache.clone()],
            data: vec![data.clone()],
            ..AppItem::default()
        };
        let related = related_app_data(std::slice::from_ref(&app), &app);
        assert!(!related.kept_shared);
        assert_eq!(related.paths, vec![cache.clone()]);

        app.include_data = true;
        let related = related_app_data(std::slice::from_ref(&app), &app);
        assert_eq!(related.paths, vec![cache, data]);

        let mut copy = app.clone();
        copy.path = home.join("Applications/Copy/Example.app");
        let related = related_app_data(&[app.clone(), copy], &app);
        assert!(related.kept_shared);
        assert!(related.paths.is_empty());
    }

    #[test]
    fn analyzer_refuses_non_child_paths() {
        let item = DiskItem {
            name: "hosts".to_string(),
            path: PathBuf::from("/etc/hosts"),
            bytes: 1,
            is_directory: false,
            complete: true,
            identity: None,
            modified: None,
        };
        let result = trash_disk_item(&item, Path::new("/tmp"), Path::new("/tmp"));
        assert_eq!(result.moved, 0);
        assert_eq!(result.failures.len(), 1);
        assert!(item.path.exists());
    }
}
