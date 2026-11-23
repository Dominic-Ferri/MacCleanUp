use std::path::PathBuf;
use std::time::SystemTime;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ItemIdentity {
    pub device: u64,
    pub inode: u64,
    pub parent_device: u64,
    pub parent_inode: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CleanItem {
    pub id: String,
    pub category: CleanCategory,
    pub name: String,
    pub path: PathBuf,
    pub bytes: u64,
    pub selected: bool,
    pub complete: bool,
    pub identity: Option<ItemIdentity>,
    pub modified: Option<SystemTime>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CleanCategory {
    Cache,
    Log,
    Temporary,
    Developer,
    Installer,
}

impl CleanCategory {
    pub fn label(self) -> &'static str {
        match self {
            Self::Cache => "App cache",
            Self::Log => "Activity log",
            Self::Temporary => "Tool downloads",
            Self::Developer => "Developer files",
            Self::Installer => "Installer",
        }
    }

    /// What the item is and what happens after it moves to Trash, in words
    /// for people who do not know the technical terms.
    pub fn explanation(self) -> &'static str {
        match self {
            Self::Cache => "A cache is a set of copies an app keeps so it can open things faster, such as pictures from websites it has already shown you. It is safe to remove. The app makes a new one when it needs it, so it may be a little slower the first time.",
            Self::Log => "A log is a record an app or macOS keeps of what it did, mostly to help fix problems. Old logs are not needed. Removing one does not change your files, settings, or apps.",
            Self::Temporary => "Leftover downloads from tools you have installed. A tool downloads them again if it needs them.",
            Self::Developer => "Files that programming tools, such as Xcode, make while building apps. They are safe to remove; the next build takes longer. If you do not write software, you can ignore these.",
            Self::Installer => "A disk image or package you downloaded to install an app. The installed app keeps working without it.",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppItem {
    pub name: String,
    pub bundle_id: Option<String>,
    pub version: Option<String>,
    pub path: PathBuf,
    pub bytes: u64,
    pub selected: bool,
    pub identity: Option<ItemIdentity>,
    pub modified: Option<SystemTime>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiskItem {
    pub name: String,
    pub path: PathBuf,
    pub bytes: u64,
    pub is_directory: bool,
    pub complete: bool,
    pub identity: Option<ItemIdentity>,
    pub modified: Option<SystemTime>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct OperationResult {
    pub moved: usize,
    pub bytes: u64,
    pub failures: Vec<String>,
    pub notes: Vec<String>,
}

pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if bytes < 1000 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if value >= 100.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else if value >= 10.0 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.2} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::format_bytes;

    #[test]
    fn formats_decimal_sizes() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(999), "999 B");
        assert_eq!(format_bytes(1_000), "1.00 KB");
        assert_eq!(format_bytes(12_500), "12.5 KB");
        assert_eq!(format_bytes(150_000_000), "150 MB");
        assert_eq!(format_bytes(2_400_000_000), "2.40 GB");
    }
}
