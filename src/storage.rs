//! Download storage that never touches files the user did not select.
//!
//! BitTorrent pieces do not line up with file boundaries, so a piece at the
//! edge of a selected file also carries the start or end of its neighbour.
//! librqbit's own storage creates every file of the torrent and writes those
//! slivers into them, leaving stub files of unselected files on disk.
//! Here selected files are written as usual and everything that belongs to
//! unselected files goes into one sparse "parts" file in Rivulet's data
//! folder (the same idea as qBittorrent's .parts file).

use std::{
    any::TypeId,
    collections::{HashMap, HashSet},
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};

use anyhow::{Context, Result};
use librqbit::{
    ManagedTorrentShared, TorrentMetadata,
    storage::{
        BoxStorageFactory, StorageFactory, StorageFactoryExt, TorrentStorage, filesystem::FilesystemStorageFactory,
    },
};
use serde::{Deserialize, Serialize};

/// Where a torrent's files go and which of them the user wants.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Layout {
    pub folder: PathBuf,
    /// None = all files.
    pub selected: Option<Vec<usize>>,
}

/// Layouts by info hash (hex), shared with the engine.
pub type Layouts = Arc<Mutex<HashMap<String, Layout>>>;

#[derive(Clone)]
pub struct PartsStorageFactory {
    pub layouts: Layouts,
    pub parts_dir: PathBuf,
}

impl PartsStorageFactory {
    pub fn parts_file(&self, info_hash: &str) -> PathBuf {
        self.parts_dir.join(format!("{info_hash}.parts"))
    }
}

impl StorageFactory for PartsStorageFactory {
    type Storage = Box<dyn TorrentStorage>;

    fn create(&self, shared: &ManagedTorrentShared, metadata: &TorrentMetadata) -> Result<Self::Storage> {
        let hash = shared.info_hash.as_string();
        let layout = self.layouts.lock().unwrap().get(&hash).cloned();
        // Torrents added before Rivulet tracked layouts keep the stock storage.
        let Some(layout) = layout else {
            return Ok(Box::new(FilesystemStorageFactory::default().create(shared, metadata)?));
        };
        let selected: Option<HashSet<usize>> = layout.selected.map(|v| v.into_iter().collect());
        let files = metadata
            .file_infos
            .iter()
            .enumerate()
            .map(|(i, fi)| {
                if fi.attrs.padding {
                    Slot::Padding
                } else if selected.as_ref().is_none_or(|s| s.contains(&i)) {
                    Slot::Real { path: layout.folder.join(&fi.relative_filename), file: OnceLock::new() }
                } else {
                    Slot::Part { torrent_offset: fi.offset_in_torrent }
                }
            })
            .collect();
        Ok(Box::new(PartsStorage {
            inner: Arc::new(Inner {
                folder: layout.folder,
                files,
                parts_path: self.parts_file(&hash),
                parts: OnceLock::new(),
            }),
        }))
    }

    /// librqbit's session persistence only accepts filesystem storage; this is
    /// one, with the same on-disk layout for selected files.
    fn is_type_id(&self, type_id: TypeId) -> bool {
        type_id == TypeId::of::<FilesystemStorageFactory>() || type_id == TypeId::of::<Self>()
    }

    fn clone_box(&self) -> BoxStorageFactory {
        self.clone().boxed()
    }
}

enum Slot {
    Padding,
    Real {
        path: PathBuf,
        file: OnceLock<File>,
    },
    /// Unselected file: its bytes live in the parts file at this offset.
    Part {
        torrent_offset: u64,
    },
}

struct Inner {
    folder: PathBuf,
    files: Vec<Slot>,
    parts_path: PathBuf,
    parts: OnceLock<File>,
}

#[derive(Clone)]
struct PartsStorage {
    inner: Arc<Inner>,
}

fn open_rw(path: &Path, create: bool) -> Result<File> {
    if create && let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let f = OpenOptions::new()
        .read(true)
        .write(true)
        .create(create)
        .truncate(false)
        .open(path)
        .with_context(|| format!("cannot open {}", path.display()))?;
    mark_sparse(&f);
    Ok(f)
}

impl Inner {
    fn real<'a>(&self, path: &Path, cell: &'a OnceLock<File>) -> Result<&'a File> {
        if let Some(f) = cell.get() {
            return Ok(f);
        }
        let f = open_rw(path, true)?;
        Ok(cell.get_or_init(|| f))
    }

    fn parts(&self, create: bool) -> Result<&File> {
        if let Some(f) = self.parts.get() {
            return Ok(f);
        }
        let f = open_rw(&self.parts_path, create)?;
        Ok(self.parts.get_or_init(|| f))
    }

    /// The file and position that hold byte `offset` of file `file_id`.
    fn locate(&self, file_id: usize, offset: u64, create: bool) -> Result<Option<(&File, u64)>> {
        match self.files.get(file_id).context("no such file")? {
            Slot::Padding => Ok(None),
            Slot::Real { path, file } => Ok(Some((self.real(path, file)?, offset))),
            Slot::Part { torrent_offset } => Ok(Some((self.parts(create)?, torrent_offset + offset))),
        }
    }
}

fn read_exact_at(f: &File, mut pos: u64, mut buf: &mut [u8]) -> Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        let n = f.seek_read(buf, pos)?;
        if n == 0 {
            anyhow::bail!("unexpected end of file");
        }
        buf = &mut buf[n..];
        pos += n as u64;
    }
    Ok(())
}

fn write_all_at(f: &File, mut pos: u64, mut buf: &[u8]) -> Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        let n = f.seek_write(buf, pos)?;
        if n == 0 {
            anyhow::bail!("write returned 0 bytes");
        }
        buf = &buf[n..];
        pos += n as u64;
    }
    Ok(())
}

impl TorrentStorage for PartsStorage {
    fn init(&mut self, _shared: &ManagedTorrentShared, _metadata: &TorrentMetadata) -> Result<()> {
        // Create the selected files up front, like the stock storage does, so
        // an empty selected file exists even if it has no data.
        for slot in &self.inner.files {
            if let Slot::Real { path, file } = slot {
                self.inner.real(path, file)?;
            }
        }
        Ok(())
    }

    fn pread_exact(&self, file_id: usize, offset: u64, buf: &mut [u8]) -> Result<()> {
        match self.inner.locate(file_id, offset, false)? {
            Some((f, pos)) => read_exact_at(f, pos, buf),
            None => {
                buf.fill(0);
                Ok(())
            }
        }
    }

    fn pwrite_all(&self, file_id: usize, offset: u64, buf: &[u8]) -> Result<()> {
        match self.inner.locate(file_id, offset, true)? {
            Some((f, pos)) => write_all_at(f, pos, buf),
            None => Ok(()),
        }
    }

    fn remove_file(&self, file_id: usize, filename: &Path) -> Result<()> {
        match self.inner.files.get(file_id) {
            Some(Slot::Real { .. }) => match std::fs::remove_file(self.inner.folder.join(filename)) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                r => Ok(r?),
            },
            // The parts file is removed by the engine with the torrent.
            _ => Ok(()),
        }
    }

    fn remove_directory_if_empty(&self, path: &Path) -> Result<()> {
        let path = self.inner.folder.join(path);
        if path.is_dir() && std::fs::read_dir(&path)?.next().is_none() {
            std::fs::remove_dir(&path)?;
        }
        Ok(())
    }

    fn ensure_file_length(&self, file_id: usize, length: u64) -> Result<()> {
        if let Some(Slot::Real { path, file }) = self.inner.files.get(file_id) {
            let f = self.inner.real(path, file)?;
            if f.metadata()?.len() != length {
                f.set_len(length)?;
            }
        }
        Ok(())
    }

    fn take(&self) -> Result<Box<dyn TorrentStorage>> {
        Ok(Box::new(self.clone()))
    }
}

/// Lets NTFS skip allocating the ranges that were never written, so a file
/// sized up front or a parts file with a few slivers far apart stays small.
fn mark_sparse(f: &File) {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::{
        Foundation::HANDLE,
        System::{IO::DeviceIoControl, Ioctl::FSCTL_SET_SPARSE},
    };
    unsafe {
        let _ = DeviceIoControl(HANDLE(f.as_raw_handle()), FSCTL_SET_SPARSE, None, 0, None, 0, None, None);
    }
}
