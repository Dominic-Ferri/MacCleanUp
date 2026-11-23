use crate::model::{AppItem, CleanItem, DiskItem, OperationResult};
use crate::scanner::{
    identity_matches, is_protected_disk_path, is_safe_app_path, is_safe_cleanup_path,
    is_safe_installer_path, item_identity, measure,
};
use objc2_foundation::{NSFileManager, NSString, NSURL};
use std::path::{Path, PathBuf};

pub fn trash_clean_items(items: &[CleanItem], home: &Path) -> OperationResult {
    let mut result = OperationResult::default();
    for item in items.iter().filter(|item| item.selected) {
        if !is_safe_cleanup_path(&item.path, home) || !identity_matches(&item.path, item.identity) {
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
        let related = related_app_data(apps, app, home);
        if related.kept_shared {
            if let Some(bundle_id) = &app.bundle_id {
                result.notes.push(format!(
                    "Kept caches for {bundle_id}: another installed copy uses them."
                ));
            }
        }
        for (path, bytes) in related.paths {
            let identity = item_identity(&path);
            if identity.is_some() && identity_matches(&path, identity) {
                move_one(&path, bytes, &mut result);
            }
        }
    }
    result
}

/// The cache and log folders that uninstalling `app` would also move.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RelatedData {
    pub paths: Vec<(PathBuf, u64)>,
    /// True when another installed copy that is not selected uses the same
    /// bundle ID, so its caches stay.
    pub kept_shared: bool,
}

/// Lists exactly what `trash_apps` moves with `app`, so a preview and the
/// operation cannot disagree. `apps` supplies the current selection.
pub fn related_app_data(apps: &[AppItem], app: &AppItem, home: &Path) -> RelatedData {
    let Some(bundle_id) = &app.bundle_id else {
        return RelatedData::default();
    };
    let mut selection = apps.to_vec();
    if let Some(entry) = selection.iter_mut().find(|other| other.path == app.path) {
        entry.selected = true;
    }
    let mut this = app.clone();
    this.selected = true;
    if has_unselected_sibling(&selection, &this) {
        return RelatedData {
            paths: Vec::new(),
            kept_shared: true,
        };
    }
    RelatedData {
        paths: exact_bundle_caches(home, bundle_id)
            .into_iter()
            .filter(|path| is_safe_cleanup_path(path, home))
            .map(|path| {
                let bytes = measure(&path).bytes;
                (path, bytes)
            })
            .collect(),
        kept_shared: false,
    }
}

pub fn trash_installers(items: &[CleanItem], home: &Path) -> OperationResult {
    let mut result = OperationResult::default();
    for item in items.iter().filter(|item| item.selected) {
        if !is_safe_installer_path(&item.path, home) || !identity_matches(&item.path, item.identity)
        {
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

fn exact_bundle_caches(home: &Path, bundle_id: &str) -> [PathBuf; 2] {
    [
        home.join("Library/Caches").join(bundle_id),
        home.join("Library/Logs").join(bundle_id),
    ]
}

fn move_one(path: &Path, bytes: u64, result: &mut OperationResult) -> bool {
    match trash_path(path) {
        Ok(()) => {
            result.moved += 1;
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

    #[test]
    fn unselected_cleanup_items_do_nothing() {
        let home = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/home");
        let items = vec![CleanItem {
            id: "test".to_string(),
            category: crate::model::CleanCategory::Cache,
            name: "cache".to_string(),
            path: home.join("Library/Caches/com.example.alpha"),
            bytes: 10,
            selected: false,
            complete: true,
            identity: None,
            modified: None,
        }];
        let result = trash_clean_items(&items, &home);
        assert_eq!(result, OperationResult::default());
        assert!(items[0].path.exists());
    }

    #[test]
    fn missing_identity_blocks_selected_cleanup_item() {
        let home = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/home");
        let path = home.join("Library/Caches/com.example.alpha");
        let items = vec![CleanItem {
            id: "test".to_string(),
            category: crate::model::CleanCategory::Cache,
            name: "cache".to_string(),
            path: path.clone(),
            bytes: 10,
            selected: true,
            complete: true,
            identity: None,
            modified: None,
        }];
        let result = trash_clean_items(&items, &home);
        assert_eq!(result.moved, 0);
        assert_eq!(result.failures.len(), 1);
        assert!(path.exists());
    }

    #[test]
    fn shared_bundle_cache_is_kept_only_for_unselected_siblings() {
        let make_app = |path: &str, selected: bool| AppItem {
            name: "Example".to_string(),
            bundle_id: Some("com.example.shared".to_string()),
            version: None,
            path: PathBuf::from(path),
            bytes: 1,
            selected,
            identity: None,
            modified: None,
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
    fn uninstall_preview_lists_exact_caches_and_respects_shared_copies() {
        let home = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/home");
        let app = AppItem {
            name: "Example".to_string(),
            bundle_id: Some("com.example.alpha".to_string()),
            version: None,
            path: home.join("Applications/Example.app"),
            bytes: 1,
            selected: false,
            identity: None,
            modified: None,
        };
        let related = related_app_data(std::slice::from_ref(&app), &app, &home);
        assert!(!related.kept_shared);
        assert_eq!(
            related.paths,
            vec![(home.join("Library/Caches/com.example.alpha"), 10)]
        );

        let mut copy = app.clone();
        copy.path = home.join("Applications/Copy/Example.app");
        let related = related_app_data(&[app.clone(), copy], &app, &home);
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
