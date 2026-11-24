//! Torrent engine for Rivulet: a thin layer over librqbit that the UI will use.
//!
//! It owns the librqbit session, the three performance profiles, the
//! "preview before download" step and the extra data librqbit does not keep
//! (aliases, chosen profile).

use std::{
    collections::HashMap,
    num::NonZeroU32,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result, bail};
use librqbit::{
    AddTorrent, AddTorrentOptions, AddTorrentResponse, ListenerMode, ListenerOptions, Session,
    SessionOptions, SessionPersistenceConfig, TorrentStatsState, api::TorrentIdOrHash,
    dht::Id20, limits::LimitsConfig,
};
use serde::{Deserialize, Serialize};

use librqbit::storage::StorageFactoryExt;

use crate::storage::{Layout, Layouts, PartsStorageFactory};

/// Performance profile, chosen once instead of tuning the engine by hand.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Profile {
    /// Light: few connections, speed caps, gentle on the PC and the network.
    Calm,
    /// Default: no speed caps, moderate connection count.
    #[default]
    Steady,
    /// Everything the connection can take.
    Surge,
}

impl Profile {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "calm" => Some(Self::Calm),
            "steady" => Some(Self::Steady),
            "surge" => Some(Self::Surge),
            _ => None,
        }
    }

    /// Peers per torrent (librqbit has no session-wide connection cap).
    fn peer_limit(self) -> usize {
        match self {
            Self::Calm => 24,
            Self::Steady => 120,
            Self::Surge => 500,
        }
    }

    fn limits(self) -> LimitsConfig {
        match self {
            Self::Calm => LimitsConfig {
                download_bps: NonZeroU32::new(5 * 1024 * 1024),
                upload_bps: NonZeroU32::new(2560 * 1024),
            },
            Self::Steady | Self::Surge => LimitsConfig::default(),
        }
    }
}

/// One file inside a torrent, as shown in the "choose files" step.
#[derive(Clone, Debug, Serialize)]
pub struct DraftFile {
    pub index: usize,
    pub path: String,
    pub size: u64,
}

/// A torrent that has been read but not started yet.
#[derive(Clone, Debug, Serialize)]
pub struct Draft {
    pub name: String,
    pub info_hash: String,
    pub files: Vec<DraftFile>,
    pub total_size: u64,
    #[serde(skip)]
    torrent_bytes: bytes::Bytes,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Checking,
    Downloading,
    Seeding,
    Paused,
    Finished,
    Error,
}

/// What a download card needs, refreshed about once a second.
#[derive(Clone, Debug, Serialize)]
pub struct Snapshot {
    pub id: usize,
    pub info_hash: String,
    pub name: String,
    pub alias: Option<String>,
    pub folder: PathBuf,
    pub status: Status,
    /// 0.0 ..= 1.0 of the selected files.
    pub progress: f64,
    pub downloaded: u64,
    pub size: u64,
    pub uploaded: u64,
    pub down_bps: u64,
    pub up_bps: u64,
    pub eta_secs: Option<u64>,
    pub seeds: u32,
    pub peers: u32,
    pub error: Option<String>,
}

/// Settings and data librqbit does not store itself.
#[derive(Default, Serialize, Deserialize)]
struct Extras {
    profile: Profile,
    /// info hash (hex) -> alias
    aliases: HashMap<String, String>,
    /// Folder new downloads go to unless chosen otherwise.
    #[serde(default)]
    download_dir: Option<PathBuf>,
    /// info hash (hex) -> where files go and which were selected; read by
    /// the storage so unselected files never appear on disk.
    #[serde(default)]
    layouts: HashMap<String, Layout>,
}

pub struct Engine {
    session: Arc<Session>,
    data_dir: PathBuf,
    default_download_dir: PathBuf,
    extras: Mutex<Extras>,
    layouts: Layouts,
    storage: PartsStorageFactory,
}

impl Engine {
    /// Opens (or restores) the session that keeps its state in `data_dir`.
    pub async fn start(data_dir: &Path, default_download_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(data_dir)?;
        let extras = load_extras(data_dir);
        let profile = extras.profile;
        let layouts: Layouts = Arc::new(Mutex::new(extras.layouts.clone()));
        let storage = PartsStorageFactory { layouts: layouts.clone(), parts_dir: data_dir.join("parts") };
        let session = Session::new_with_opts(
            default_download_dir.to_path_buf(),
            SessionOptions {
                fastresume: true,
                persistence: Some(SessionPersistenceConfig::Json {
                    folder: Some(data_dir.join("session")),
                }),
                listen: Some(ListenerOptions {
                    mode: ListenerMode::TcpAndUtp,
                    listen_addr: (std::net::Ipv6Addr::UNSPECIFIED, 0).into(),
                    enable_upnp_port_forwarding: true,
                    ..Default::default()
                }),
                peer_limit: Some(profile.peer_limit()),
                ratelimits: profile.limits(),
                client_name_and_version: Some(format!("Rivulet {}", env!("CARGO_PKG_VERSION"))),
                default_storage_factory: Some(storage.clone().boxed()),
                ..Default::default()
            },
        )
        .await
        .context("could not start the torrent session")?;
        Ok(Self {
            session,
            data_dir: data_dir.to_path_buf(),
            default_download_dir: default_download_dir.to_path_buf(),
            extras: Mutex::new(extras),
            layouts,
            storage,
        })
    }

    /// Folder last used for a download, if any.
    pub fn download_dir(&self) -> Option<PathBuf> {
        self.extras.lock().unwrap().download_dir.clone()
    }

    pub fn set_download_dir(&self, dir: &Path) -> Result<()> {
        let mut extras = self.extras.lock().unwrap();
        extras.download_dir = Some(dir.to_path_buf());
        save_extras(&self.data_dir, &extras)
    }

    /// Gives a download a display name; empty clears it.
    pub fn set_alias(&self, info_hash: &str, alias: &str) -> Result<()> {
        let mut extras = self.extras.lock().unwrap();
        if alias.trim().is_empty() {
            extras.aliases.remove(info_hash);
        } else {
            extras.aliases.insert(info_hash.to_owned(), alias.trim().to_owned());
        }
        save_extras(&self.data_dir, &extras)
    }

    pub fn profile(&self) -> Profile {
        self.extras.lock().unwrap().profile
    }

    /// Switches the profile. Speed caps apply at once; the peer limit applies
    /// to torrents added from now on and to all of them after a restart.
    pub fn set_profile(&self, profile: Profile) -> Result<()> {
        let limits = profile.limits();
        self.session.ratelimits.set_download_bps(limits.download_bps);
        self.session.ratelimits.set_upload_bps(limits.upload_bps);
        let mut extras = self.extras.lock().unwrap();
        extras.profile = profile;
        save_extras(&self.data_dir, &extras)
    }

    /// Reads a .torrent path, a magnet link or an http(s) URL without starting
    /// the download. Magnets need peers to send the file list, so this can take
    /// a while.
    pub async fn preview(&self, source: &str) -> Result<Draft> {
        let add = to_add_torrent(source)?;
        let response = self
            .session
            .add_torrent(
                add,
                Some(AddTorrentOptions {
                    list_only: true,
                    ..Default::default()
                }),
            )
            .await?;
        let AddTorrentResponse::ListOnly(list) = response else {
            bail!("this torrent is already in the list");
        };
        let files: Vec<DraftFile> = list
            .info
            .iter_file_details()
            .enumerate()
            .map(|(index, f)| DraftFile {
                index,
                path: f.filename.to_vec().join("/"),
                size: f.len,
            })
            .collect();
        Ok(Draft {
            name: list
                .info
                .name()
                .map(|n| n.into_owned())
                .unwrap_or_else(|| list.info_hash.as_string()),
            info_hash: list.info_hash.as_string(),
            total_size: files.iter().map(|f| f.size).sum(),
            files,
            torrent_bytes: list.torrent_bytes,
        })
    }

    /// Starts a previewed torrent. `files: None` means all files.
    pub async fn add(
        &self,
        draft: &Draft,
        files: Option<Vec<usize>>,
        output_folder: Option<&Path>,
        alias: Option<String>,
    ) -> Result<usize> {
        if let Some(files) = &files {
            if files.is_empty() {
                bail!("no files selected");
            }
            if let Some(bad) = files.iter().find(|i| **i >= draft.files.len()) {
                bail!("file #{bad} does not exist in this torrent");
            }
        }
        if self.session.get(TorrentIdOrHash::Hash(Id20::from_str(&draft.info_hash)?)).is_some() {
            bail!("this torrent is already in the list");
        }
        // A torrent with several files gets its own folder, as librqbit does
        // for its default location.
        let base = output_folder.unwrap_or(&self.default_download_dir);
        let folder = if draft.files.len() > 1 { base.join(safe_folder_name(&draft.name)) } else { base.to_path_buf() };
        // The storage reads the layout when librqbit creates it during add.
        self.set_layout(&draft.info_hash, Some(Layout { folder: folder.clone(), selected: files.clone() }))?;
        let output_folder = Some(folder);
        let response = self
            .session
            .add_torrent(
                AddTorrent::from_bytes(draft.torrent_bytes.clone()),
                Some(AddTorrentOptions {
                    only_files: files,
                    output_folder: output_folder.map(|p| p.to_string_lossy().into_owned()),
                    peer_limit: Some(self.profile().peer_limit()),
                    overwrite: true,
                    ..Default::default()
                }),
            )
            .await
            .inspect_err(|_| {
                let _ = self.set_layout(&draft.info_hash, None);
            })?;
        let id = match response {
            AddTorrentResponse::Added(id, _) => id,
            AddTorrentResponse::AlreadyManaged(_, _) => bail!("this torrent is already in the list"),
            AddTorrentResponse::ListOnly(_) => unreachable!("list_only was not requested"),
        };
        if let Some(alias) = alias.filter(|a| !a.trim().is_empty()) {
            let mut extras = self.extras.lock().unwrap();
            extras.aliases.insert(draft.info_hash.clone(), alias.trim().to_owned());
            save_extras(&self.data_dir, &extras)?;
        }
        Ok(id)
    }

    pub fn snapshots(&self) -> Vec<Snapshot> {
        let extras = self.extras.lock().unwrap();
        let mut list = self.session.with_torrents(|torrents| {
            torrents
                .map(|(id, t)| {
                    let stats = t.stats();
                    let info_hash = t.info_hash().as_string();
                    let live = stats.live.as_ref();
                    let down_bps = live.map_or(0, |l| l.download_speed.as_bytes());
                    let up_bps = live.map_or(0, |l| l.upload_speed.as_bytes());
                    let peers = live.map(|l| &l.snapshot.peer_stats);
                    let status = match stats.state {
                        TorrentStatsState::Initializing { .. } => Status::Checking,
                        TorrentStatsState::Paused if stats.finished => Status::Finished,
                        TorrentStatsState::Paused => Status::Paused,
                        TorrentStatsState::Error => Status::Error,
                        TorrentStatsState::Live if stats.finished => Status::Seeding,
                        TorrentStatsState::Live => Status::Downloading,
                    };
                    let remaining = stats.total_bytes.saturating_sub(stats.progress_bytes);
                    Snapshot {
                        id,
                        name: t.name().unwrap_or_else(|| info_hash.clone()),
                        alias: extras.aliases.get(&info_hash).cloned(),
                        folder: t.output_folder().to_path_buf(),
                        info_hash,
                        status,
                        progress: if stats.total_bytes == 0 {
                            0.0
                        } else {
                            stats.progress_bytes as f64 / stats.total_bytes as f64
                        },
                        downloaded: stats.progress_bytes,
                        size: stats.total_bytes,
                        uploaded: stats.uploaded_bytes,
                        down_bps,
                        up_bps,
                        eta_secs: (status == Status::Downloading && down_bps > 0)
                            .then(|| remaining / down_bps),
                        seeds: peers.map_or(0, |p| p.live_seeders),
                        peers: peers.map_or(0, |p| p.live),
                        error: stats.error,
                    }
                })
                .collect::<Vec<_>>()
        });
        list.sort_by_key(|s| s.id);
        list
    }

    pub async fn pause(&self, id: usize) -> Result<()> {
        let t = self.handle(id)?;
        self.session.pause(&t).await
    }

    pub async fn resume(&self, id: usize) -> Result<()> {
        let t = self.handle(id)?;
        self.session.unpause(&t).await
    }

    pub async fn remove(&self, id: usize, delete_files: bool) -> Result<()> {
        let hash = self.handle(id)?.info_hash().as_string();
        self.session.delete(TorrentIdOrHash::Id(id), delete_files).await?;
        // Slivers of unselected files are only useful while the torrent is in
        // the list.
        let _ = std::fs::remove_file(self.storage.parts_file(&hash));
        self.extras.lock().unwrap().aliases.remove(&hash);
        self.set_layout(&hash, None)
    }

    fn set_layout(&self, info_hash: &str, layout: Option<Layout>) -> Result<()> {
        let mut layouts = self.layouts.lock().unwrap();
        match layout {
            Some(l) => layouts.insert(info_hash.to_owned(), l),
            None => layouts.remove(info_hash),
        };
        let mut extras = self.extras.lock().unwrap();
        extras.layouts = layouts.clone();
        save_extras(&self.data_dir, &extras)
    }

    pub async fn stop(&self) {
        self.session.stop().await;
    }

    fn handle(&self, id: usize) -> Result<Arc<librqbit::ManagedTorrent>> {
        self.session
            .get(TorrentIdOrHash::Id(id))
            .with_context(|| format!("no download with id {id}"))
    }
}

fn to_add_torrent(source: &str) -> Result<AddTorrent<'static>> {
    let s = source.trim();
    if s.starts_with("magnet:") || s.starts_with("http://") || s.starts_with("https://") {
        return Ok(AddTorrent::from_url(s.to_owned()));
    }
    if s.len() == 40 && s.chars().all(|c| c.is_ascii_hexdigit()) {
        Id20::from_str(s)?;
        return Ok(AddTorrent::from_url(format!("magnet:?xt=urn:btih:{s}")));
    }
    let bytes = std::fs::read(s).with_context(|| format!("could not read {s}"))?;
    Ok(AddTorrent::from_bytes(bytes))
}

/// A torrent name made safe as a Windows folder name.
fn safe_folder_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || c.is_control() { '_' } else { c })
        .collect();
    let trimmed = cleaned.trim().trim_end_matches(['.', ' ']);
    if trimmed.is_empty() { "torrent".into() } else { trimmed.chars().take(120).collect() }
}

fn extras_path(data_dir: &Path) -> PathBuf {
    data_dir.join("rivulet.json")
}

fn load_extras(data_dir: &Path) -> Extras {
    std::fs::read(extras_path(data_dir))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save_extras(data_dir: &Path, extras: &Extras) -> Result<()> {
    let tmp = data_dir.join("rivulet.json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(extras)?)?;
    std::fs::rename(tmp, extras_path(data_dir))?;
    Ok(())
}

use std::str::FromStr as _;
