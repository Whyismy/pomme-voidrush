use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::resource_pack::ResourcePackManager;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AssetId<'a> {
    pub namespace: &'a str,
    pub path: &'a str,
}

impl<'a> AssetId<'a> {
    pub fn parse(value: &'a str) -> Self {
        match value.split_once(':') {
            Some((namespace, path)) if !namespace.is_empty() && !path.is_empty() => {
                Self { namespace, path }
            }
            _ => Self {
                namespace: "minecraft",
                path: value,
            },
        }
    }

    pub fn asset_key(self, category: &str, suffix: &str) -> String {
        format!("{}/{category}/{}{}", self.namespace, self.path, suffix)
    }

    pub fn canonical(self) -> String {
        if self.namespace == "minecraft" {
            self.path.to_string()
        } else {
            format!("{}:{}", self.namespace, self.path)
        }
    }
}

/// `Identifier.toString` omits the default namespace, so ids written either
/// way compare equal once stripped.
pub(crate) fn strip_default_namespace(value: &str) -> &str {
    value.strip_prefix("minecraft:").unwrap_or(value)
}

/// `Identifier` namespace (`path` false) or path characters; only the path
/// allows `/`.
pub(crate) fn identifier_chars(text: &str, path: bool) -> bool {
    text.bytes().all(|byte| {
        byte.is_ascii_lowercase()
            || byte.is_ascii_digit()
            || matches!(byte, b'_' | b'.' | b'-')
            || path && byte == b'/'
    })
}

pub(crate) fn valid_asset_key(asset_key: &str) -> bool {
    let Some((namespace, path)) = asset_key.split_once('/') else {
        return false;
    };
    !matches!(namespace, "" | "." | "..")
        && identifier_chars(namespace, false)
        && identifier_chars(path, true)
        && !path
            .split('/')
            .any(|component| matches!(component, "" | "." | ".."))
}

/// Pomme's brand mark, embedded rather than resolved from the vanilla asset
/// tree: the window icon and the credits roll's logo both come from it.
pub const POMME_ICON_PNG: &[u8] =
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/icon.png"));

/// The "Rust Edition" banner under the title logo, laid out like the jar's
/// `edition.png`.
pub const POMME_EDITION_PNG: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/gui/title/edition.png"
));

pub fn load_image(path: &Path) -> Result<image::DynamicImage, image::ImageError> {
    image::open(path).or_else(|_| {
        let data = std::fs::read(path).map_err(image::ImageError::IoError)?;
        image::load_from_memory(&data)
    })
}

pub fn resolve_asset_path(
    jar_assets_dir: &Path,
    asset_index: &Option<AssetIndex>,
    asset_key: &str,
) -> PathBuf {
    resolve_asset_path_with_packs(jar_assets_dir, asset_index, asset_key, None)
}

pub fn resource_stack_paths(
    jar_assets_dir: &Path,
    asset_index: &Option<AssetIndex>,
    asset_key: &str,
    packs: Option<&ResourcePackManager>,
) -> Vec<PathBuf> {
    if !checked_asset_key(asset_key) {
        return Vec::new();
    }
    let mut stack: Vec<_> = builtin_asset(jar_assets_dir, asset_index, asset_key)
        .into_iter()
        .collect();
    if let Some(packs) = packs {
        for root in packs.active_pack_dirs() {
            let path = root.join("assets").join(asset_key);
            if path.is_file() {
                stack.push(path);
            }
        }
    }
    stack
}

pub fn resolve_asset_path_with_packs(
    jar_assets_dir: &Path,
    asset_index: &Option<AssetIndex>,
    asset_key: &str,
    packs: Option<&ResourcePackManager>,
) -> PathBuf {
    if !checked_asset_key(asset_key) {
        return jar_assets_dir.join("__invalid_asset_key__");
    }
    packs
        .and_then(|packs| packs.resolve_asset(asset_key))
        .or_else(|| builtin_asset(jar_assets_dir, asset_index, asset_key))
        .unwrap_or_else(|| jar_assets_dir.join(asset_key))
}

fn checked_asset_key(asset_key: &str) -> bool {
    let valid = valid_asset_key(asset_key);
    if !valid {
        tracing::warn!("Rejecting invalid Minecraft asset key {asset_key:?}");
    }
    valid
}

/// The jar and the asset index are one built-in pack, the index first.
fn builtin_asset(
    jar_assets_dir: &Path,
    asset_index: &Option<AssetIndex>,
    asset_key: &str,
) -> Option<PathBuf> {
    asset_index
        .as_ref()
        .and_then(|idx| idx.resolve(asset_key))
        .or_else(|| {
            let jar = jar_assets_dir.join(asset_key);
            jar.is_file().then_some(jar)
        })
}

/// How many files [`prefetch`] reads at once.
const PREFETCH_THREADS: usize = 32;

/// The folders whose files loading the models and the atlas reads one at a
/// time: the jar's models, item and block definitions and their textures,
/// and every active pack's assets.
pub fn prefetch_roots(jar_assets_dir: &Path, packs: Option<&ResourcePackManager>) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = [
        "models",
        "items",
        "blockstates",
        "textures/block",
        "textures/item",
    ]
    .iter()
    .map(|folder| jar_assets_dir.join("minecraft").join(folder))
    .collect();
    if let Some(packs) = packs {
        roots.extend(packs.active_pack_dirs().map(|dir| dir.join("assets")));
    }
    roots
}

/// Reads every file under `roots` on many threads at once, and drops what
/// it read; `progress` hears how many of how many are done.
///
/// A file the system has not cached costs milliseconds to open (the disk,
/// and whatever scans files as they are opened), and a pack's thousands of
/// small files opened in turn held a reload up for twenty seconds. Opened
/// together they take a second or two, and the loading that follows finds
/// each of them cached.
pub fn prefetch(roots: &[PathBuf], progress: &(dyn Fn(usize, usize) + Sync)) {
    prefetch_on(PREFETCH_THREADS, roots, progress);
}

fn prefetch_on(threads: usize, roots: &[PathBuf], progress: &(dyn Fn(usize, usize) + Sync)) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let start = std::time::Instant::now();
    let mut files = Vec::new();
    let mut folders = roots.to_vec();
    while let Some(folder) = folders.pop() {
        let Ok(entries) = std::fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.flatten() {
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => folders.push(entry.path()),
                Ok(kind) if kind.is_file() => files.push(entry.path()),
                _ => {}
            }
        }
    }
    let (next, done) = (AtomicUsize::new(0), AtomicUsize::new(0));
    std::thread::scope(|scope| {
        for _ in 0..threads.min(files.len()) {
            scope.spawn(|| {
                while let Some(path) = files.get(next.fetch_add(1, Ordering::Relaxed)) {
                    let _ = std::fs::read(path);
                    progress(done.fetch_add(1, Ordering::Relaxed) + 1, files.len());
                }
            });
        }
    });
    tracing::info!(
        "Read {} asset files ahead in {:.2?}",
        files.len(),
        start.elapsed()
    );
}

#[derive(Clone)]
pub struct AssetIndex {
    objects_dir: PathBuf,
    hashes: HashMap<String, String>,
}

impl AssetIndex {
    pub fn load(indexes_dir: &Path, objects_dir: &Path, version: &str) -> Option<Self> {
        let index_path = indexes_dir.join(format!("{version}.json"));

        let content = std::fs::read_to_string(&index_path)
            .map_err(|e| tracing::warn!("Failed to read asset index: {e}"))
            .ok()?;
        let parsed: serde_json::Value = serde_json::from_str(&content)
            .map_err(|e| tracing::warn!("Failed to parse asset index: {e}"))
            .ok()?;

        let objects = parsed.get("objects")?.as_object()?;
        let hashes = objects
            .iter()
            .filter_map(|(k, v)| {
                let hash = v.get("hash")?.as_str()?;
                Some((k.clone(), hash.to_owned()))
            })
            .collect();

        Some(Self {
            objects_dir: objects_dir.to_path_buf(),
            hashes,
        })
    }

    pub fn resolve(&self, asset_key: &str) -> Option<PathBuf> {
        let hash = self.hashes.get(asset_key)?;
        let path = self.objects_dir.join(&hash[..2]).join(hash);
        path.exists().then_some(path)
    }

    pub(crate) fn keys(&self) -> impl Iterator<Item = &str> {
        self.hashes.keys().map(String::as_str)
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    /// Every file under the roots is read, nested ones too, and the count
    /// reaches the total; a root that isn't there is passed over.
    #[test]
    fn prefetch_reads_every_file_under_its_roots() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let root = crate::test_util::test_temp_dir("prefetch");
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        for name in ["one.json", "a/two.png", "a/b/three.mcmeta"] {
            std::fs::write(root.join(name), name).unwrap();
        }
        let (most, calls, total) = (
            AtomicUsize::new(0),
            AtomicUsize::new(0),
            AtomicUsize::new(0),
        );
        prefetch(&[root.clone(), root.join("missing")], &|done, of| {
            most.fetch_max(done, Ordering::Relaxed);
            calls.fetch_add(1, Ordering::Relaxed);
            total.store(of, Ordering::Relaxed);
        });
        assert_eq!(most.load(Ordering::Relaxed), 3);
        assert_eq!(calls.load(Ordering::Relaxed), 3);
        assert_eq!(total.load(Ordering::Relaxed), 3);
        prefetch(&[], &|_, _| panic!("nothing to read"));
        let _ = std::fs::remove_dir_all(root);
    }

    /// Times the read-ahead over a real folder: `POMME_PREFETCH_DIR=<folder>
    /// cargo test --release -p pomme-client --lib prefetch_timing --
    /// --ignored --nocapture`.
    #[test]
    #[ignore = "needs a folder to read"]
    fn prefetch_timing() {
        let root = std::path::PathBuf::from(std::env::var("POMME_PREFETCH_DIR").unwrap());
        let threads = std::env::var("POMME_PREFETCH_THREADS")
            .ok()
            .and_then(|threads| threads.parse().ok())
            .unwrap_or(PREFETCH_THREADS);
        let start = std::time::Instant::now();
        let read = std::sync::atomic::AtomicUsize::new(0);
        prefetch_on(threads, &[root], &|done, _| {
            read.fetch_max(done, std::sync::atomic::Ordering::Relaxed);
        });
        println!(
            "read {read:?} files on {threads} threads in {:.2?}",
            start.elapsed()
        );
    }

    #[test]
    fn asset_keys_reject_traversal_and_invalid_resource_location_syntax() {
        for valid in [
            "minecraft/font/default.json",
            "example/textures/font/custom.png",
            "example/font/subdir/test.ttf",
        ] {
            assert!(valid_asset_key(valid), "expected valid asset key: {valid}");
        }
        for invalid in [
            "../outside",
            "minecraft/../outside",
            "minecraft/font/../../outside",
            "minecraft/./font/default.json",
            "minecraft//font/default.json",
            "minecraft/\\outside",
            "/minecraft/font/default.json",
            "Minecraft/font/default.json",
            "minecraft/CAPS.ttf",
            "minecraft/",
        ] {
            assert!(
                !valid_asset_key(invalid),
                "accepted invalid asset key: {invalid}"
            );
        }
    }

    #[test]
    fn invalid_asset_key_never_joins_outside_root() {
        let root = std::env::temp_dir().join(format!("pomme-assets-{}", uuid::Uuid::new_v4()));
        let resolved = resolve_asset_path_with_packs(&root, &None, "minecraft/../../outside", None);
        assert_eq!(resolved, root.join("__invalid_asset_key__"));
        assert!(resource_stack_paths(&root, &None, "minecraft/../../outside", None).is_empty());
    }
}
