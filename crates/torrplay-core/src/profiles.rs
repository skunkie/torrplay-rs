// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! Profiles, their progress, and up next, as the lifecycle's Profiles and
//! progress section defines them.
//!
//! [`Viewing`] holds the application's own record of them, as the registry
//! does for torrents: callers store a change first and record it here only
//! after the store committed. Every operation takes one short synchronous
//! lock and returns snapshots.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::SystemTime;

use crate::id::{FileId, ProfileId, TorrentId, UpNextEntryId};
use crate::image::ImageInfo;
use crate::library::{self, TextError};

/// The most person profiles the application keeps; the device profile is
/// not counted.
pub const PROFILES_MAX: usize = 32;

/// The most characters a profile's name has.
pub const PROFILE_NAME_MAX_CHARS: usize = 64;

/// The name of the person profile created when none is stored.
pub const DEFAULT_PROFILE_NAME: &str = "Default";

/// The device profile's name, or the first of it with a number that no
/// person profile has, since names are unique.
pub const DEVICE_PROFILE_NAME: &str = "Devices";

/// The longest duration a file's progress may have, in milliseconds.
pub const DURATION_MAX_MS: u64 = 1_000_000_000;

/// Normalizes a profile's name as a category's.
pub fn normalize_profile_name(text: &str) -> Result<String, TextError> {
    library::normalize_line(text, PROFILE_NAME_MAX_CHARS)
}

/// What a profile is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ProfileKind {
    /// A person's viewing identity, which clients create and write.
    #[default]
    Person,
    /// The one profile holding the progress the application infers from
    /// clients that report none. It cannot be renamed or deleted, and
    /// clients cannot save its progress, only clear it.
    Device,
}

impl ProfileKind {
    /// The kind's stored and serialized name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Person => "person",
            Self::Device => "device",
        }
    }

    /// The kind with the stored name.
    pub fn from_stored(text: &str) -> Option<Self> {
        match text {
            "person" => Some(Self::Person),
            "device" => Some(Self::Device),
            _ => None,
        }
    }
}

/// A viewing identity: a person's, or the device profile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Profile {
    /// The profile's ID.
    pub id: ProfileId,
    /// Its normalized name, unique among profiles.
    pub name: String,
    /// Whether it is a person's or the device profile.
    pub kind: ProfileKind,
    /// When it was created.
    pub created_at: SystemTime,
    /// Its avatar, if one was uploaded.
    pub avatar: Option<ImageInfo>,
}

/// A profile's progress in one file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Progress {
    /// The playback position.
    pub position_ms: u64,
    /// The file's playback duration, above zero.
    pub duration_ms: u64,
    /// When the file last became viewed, while it is viewed.
    pub viewed_at: Option<SystemTime>,
    /// When the progress was last saved.
    pub updated_at: SystemTime,
}

impl Progress {
    /// Returns whether the file is in progress: its position is at least 1%
    /// of its duration.
    pub fn in_progress(&self) -> bool {
        self.position_ms > 0 && u128::from(self.position_ms) * 100 >= u128::from(self.duration_ms)
    }

    /// Returns whether the file is viewed.
    pub fn viewed(&self) -> bool {
        self.viewed_at.is_some()
    }

    /// Returns the position as a fraction of the duration.
    pub fn completion_ratio(&self) -> f64 {
        (self.position_ms as f64 / self.duration_ms as f64).clamp(0.0, 1.0)
    }
}

/// A file's progress as a player saves it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProgressUpdate {
    /// The playback position, at most the duration.
    pub position_ms: u64,
    /// The playback duration, above zero and at most [`DURATION_MAX_MS`].
    pub duration_ms: u64,
    /// Whether the file is viewed.
    pub viewed: bool,
}

/// Returns the progress `update` stores over `previous` at `now`. Reaching
/// 90% of the duration, or marking a file viewed that was not, marks it
/// viewed and resets the position; a viewed file keeps the positions saved
/// for it.
pub fn apply_update(
    previous: Option<&Progress>,
    update: ProgressUpdate,
    now: SystemTime,
) -> Progress {
    let was_viewed = previous.and_then(|previous| previous.viewed_at);
    let reached = u128::from(update.position_ms) * 10 >= u128::from(update.duration_ms) * 9;
    let (position_ms, viewed_at) = match (reached, update.viewed, was_viewed) {
        (true, _, _) => (0, Some(now)),
        (false, true, Some(since)) => (update.position_ms, Some(since)),
        (false, true, None) => (0, Some(now)),
        (false, false, _) => (update.position_ms, None),
    };
    Progress {
        position_ms,
        duration_ms: update.duration_ms,
        viewed_at,
        updated_at: now,
    }
}

/// Why an up next entry is listed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    /// A file in progress.
    Resume,
    /// The next unviewed episode of a series after a viewed one.
    NextEpisode,
}

/// A torrent as up next sees it: whether it is a series and its video files
/// in metadata order, with their names.
#[derive(Clone, Debug)]
pub struct VideoFiles<'a> {
    /// The torrent's ID.
    pub torrent: &'a TorrentId,
    /// Whether its reported kind is series.
    pub series: bool,
    /// Its video files' IDs and names, in metadata order.
    pub videos: Vec<(&'a FileId, &'a str)>,
}

/// An up next entry, without the representations it embeds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// The entry's ID.
    pub id: UpNextEntryId,
    /// Why it is listed.
    pub reason: Reason,
    /// The torrent.
    pub torrent: TorrentId,
    /// The file to play.
    pub file: FileId,
    /// When the progress that put it here was saved.
    pub activity_at: SystemTime,
}

/// Returns a profile's up next entries, most recent activity first, then by
/// ID, from its progress per torrent and its dismissals.
pub fn up_next<'a>(
    torrents: impl IntoIterator<Item = VideoFiles<'a>>,
    progress: &HashMap<TorrentId, BTreeMap<FileId, Progress>>,
    dismissals: &HashMap<UpNextEntryId, SystemTime>,
) -> Vec<Entry> {
    let mut entries = Vec::new();
    for torrent in torrents {
        let Some(saved) = progress.get(torrent.torrent) else {
            continue;
        };
        if torrent.series {
            entries.extend(series_entry(&torrent, saved));
        } else {
            for (file, _) in &torrent.videos {
                if let Some(progress) = saved.get(*file).filter(|progress| progress.in_progress()) {
                    entries.push(Entry {
                        id: UpNextEntryId::of_file(torrent.torrent, file),
                        reason: Reason::Resume,
                        torrent: torrent.torrent.clone(),
                        file: (*file).clone(),
                        activity_at: progress.updated_at,
                    });
                }
            }
        }
    }
    entries.retain(|entry| {
        dismissals
            .get(&entry.id)
            .is_none_or(|&dismissed| entry.activity_at > dismissed)
    });
    entries.sort_by(|a, b| {
        b.activity_at
            .cmp(&a.activity_at)
            .then_with(|| a.id.cmp(&b.id))
    });
    entries
}

/// The entry of a series, from its most recently saved video file.
fn series_entry(torrent: &VideoFiles<'_>, saved: &BTreeMap<FileId, Progress>) -> Option<Entry> {
    let (latest, progress) = torrent
        .videos
        .iter()
        .filter_map(|(file, _)| saved.get(*file).map(|progress| (*file, progress)))
        .max_by_key(|(_, progress)| progress.updated_at)?;
    let id = UpNextEntryId::of_series(torrent.torrent);
    let entry = |reason, file: &FileId| Entry {
        id: id.clone(),
        reason,
        torrent: torrent.torrent.clone(),
        file: file.clone(),
        activity_at: progress.updated_at,
    };
    if progress.in_progress() {
        return Some(entry(Reason::Resume, latest));
    }
    if !progress.viewed() {
        return None;
    }
    let episodes = episode_order(&torrent.videos);
    let at = episodes.iter().position(|file| *file == latest)?;
    episodes[at + 1..]
        .iter()
        .find(|file| saved.get(**file).is_none_or(|progress| !progress.viewed()))
        .map(|file| entry(Reason::NextEpisode, file))
}

/// Orders a series' video files by season and episode from their names,
/// with metadata order breaking ties; files without an episode pattern
/// follow in metadata order.
fn episode_order<'a>(videos: &[(&'a FileId, &str)]) -> Vec<&'a FileId> {
    let mut ordered: Vec<_> = videos
        .iter()
        .enumerate()
        .map(|(index, (file, name))| (library::episode_number(name), index, *file))
        .collect();
    ordered.sort_by_key(|&(number, index, _)| (number.is_none(), number, index));
    ordered.into_iter().map(|(_, _, file)| file).collect()
}

/// The profiles, their progress, and their dismissals.
#[derive(Default)]
pub struct Viewing {
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    profiles: HashMap<ProfileId, Profile>,
    progress: HashMap<ProfileId, HashMap<TorrentId, BTreeMap<FileId, Progress>>>,
    dismissals: HashMap<ProfileId, HashMap<UpNextEntryId, SystemTime>>,
}

impl Viewing {
    /// Records a profile, replacing one with its ID.
    pub fn set_profile(&self, profile: Profile) {
        self.lock().profiles.insert(profile.id.clone(), profile);
    }

    /// Returns the profile with the ID.
    pub fn profile(&self, id: &str) -> Option<Profile> {
        self.lock().profiles.get(id).cloned()
    }

    /// Returns every profile, oldest first, then by ID.
    pub fn profiles(&self) -> Vec<Profile> {
        let mut profiles: Vec<Profile> = self.lock().profiles.values().cloned().collect();
        profiles.sort_by(|a, b| (a.created_at, &a.id).cmp(&(b.created_at, &b.id)));
        profiles
    }

    /// Returns the number of person profiles.
    pub fn people(&self) -> usize {
        self.lock()
            .profiles
            .values()
            .filter(|profile| profile.kind == ProfileKind::Person)
            .count()
    }

    /// Returns the device profile, once it is recorded.
    pub fn device(&self) -> Option<Profile> {
        self.lock()
            .profiles
            .values()
            .find(|profile| profile.kind == ProfileKind::Device)
            .cloned()
    }

    /// Returns the profile other than `except` with the normalized `name`.
    pub fn named(&self, name: &str, except: Option<&ProfileId>) -> Option<ProfileId> {
        self.lock()
            .profiles
            .values()
            .find(|profile| profile.name == name && Some(&profile.id) != except)
            .map(|profile| profile.id.clone())
    }

    /// Forgets a profile with its progress and dismissals.
    pub fn remove_profile(&self, id: &ProfileId) {
        let mut state = self.lock();
        state.profiles.remove(id);
        state.progress.remove(id);
        state.dismissals.remove(id);
    }

    /// Returns a profile's progress of a torrent's files.
    pub fn torrent_progress(
        &self,
        profile: &ProfileId,
        torrent: &TorrentId,
    ) -> BTreeMap<FileId, Progress> {
        self.lock()
            .progress
            .get(profile)
            .and_then(|torrents| torrents.get(torrent))
            .cloned()
            .unwrap_or_default()
    }

    /// Returns a profile's progress of every torrent.
    pub fn profile_progress(
        &self,
        profile: &ProfileId,
    ) -> HashMap<TorrentId, BTreeMap<FileId, Progress>> {
        self.lock()
            .progress
            .get(profile)
            .cloned()
            .unwrap_or_default()
    }

    /// Records a file's progress.
    pub fn set_progress(
        &self,
        profile: &ProfileId,
        torrent: &TorrentId,
        file: &FileId,
        progress: Progress,
    ) {
        self.lock()
            .progress
            .entry(profile.clone())
            .or_default()
            .entry(torrent.clone())
            .or_default()
            .insert(file.clone(), progress);
    }

    /// Forgets a file's progress and returns whether there was any.
    pub fn remove_progress(&self, profile: &ProfileId, torrent: &TorrentId, file: &FileId) -> bool {
        let mut state = self.lock();
        let Some(torrents) = state.progress.get_mut(profile) else {
            return false;
        };
        let removed = torrents
            .get_mut(torrent)
            .is_some_and(|files| files.remove(file).is_some());
        if torrents.get(torrent).is_some_and(BTreeMap::is_empty) {
            torrents.remove(torrent);
        }
        removed
    }

    /// Forgets every profile's progress of a deleted torrent.
    pub fn forget_torrent(&self, torrent: &TorrentId) {
        for torrents in self.lock().progress.values_mut() {
            torrents.remove(torrent);
        }
    }

    /// Returns a profile's dismissals.
    pub fn dismissals(&self, profile: &ProfileId) -> HashMap<UpNextEntryId, SystemTime> {
        self.lock()
            .dismissals
            .get(profile)
            .cloned()
            .unwrap_or_default()
    }

    /// Records a dismissal.
    pub fn set_dismissal(&self, profile: &ProfileId, entry: &UpNextEntryId, at: SystemTime) {
        self.lock()
            .dismissals
            .entry(profile.clone())
            .or_default()
            .insert(entry.clone(), at);
    }

    /// Locks the state. Every update leaves it consistent before it can
    /// panic, so a poisoned lock's state is still usable.
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::id::TORRENT_ID_RANDOM_BYTES;

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn update(position_ms: u64, viewed: bool) -> ProgressUpdate {
        ProgressUpdate {
            position_ms,
            duration_ms: 1000,
            viewed,
        }
    }

    #[test]
    fn reaching_ninety_percent_marks_a_file_viewed() {
        let partway = apply_update(None, update(500, false), at(1));
        assert_eq!((partway.position_ms, partway.viewed_at), (500, None));
        assert!(partway.in_progress());
        let viewed = apply_update(Some(&partway), update(900, false), at(2));
        assert_eq!((viewed.position_ms, viewed.viewed_at), (0, Some(at(2))));
        assert!(!viewed.in_progress());
        let below = apply_update(None, update(899, false), at(3));
        assert_eq!(below.viewed_at, None, "just below 90%");
    }

    #[test]
    fn a_viewed_file_keeps_its_positions_until_unmarked() {
        let marked = apply_update(None, update(300, true), at(1));
        assert_eq!((marked.position_ms, marked.viewed_at), (0, Some(at(1))));
        let rewatching = apply_update(Some(&marked), update(400, true), at(2));
        assert_eq!(
            (rewatching.position_ms, rewatching.viewed_at),
            (400, Some(at(1))),
            "the mark keeps its time"
        );
        let unmarked = apply_update(Some(&rewatching), update(400, false), at(3));
        assert_eq!((unmarked.position_ms, unmarked.viewed_at), (400, None));
    }

    #[test]
    fn one_percent_starts_progress() {
        let progress = |position_ms| apply_update(None, update(position_ms, false), at(1));
        assert!(!progress(9).in_progress());
        assert!(progress(10).in_progress());
        assert!(!progress(0).in_progress());
    }

    fn torrent(n: u8) -> TorrentId {
        TorrentId::from_random_bytes([n; TORRENT_ID_RANDOM_BYTES])
    }

    fn file(name: &str) -> FileId {
        FileId::derive(&[name])
    }

    fn saved(position_ms: u64, viewed: bool, secs: u64) -> Progress {
        Progress {
            position_ms,
            duration_ms: 1000,
            viewed_at: viewed.then(|| at(secs)),
            updated_at: at(secs),
        }
    }

    #[test]
    fn a_series_resumes_or_moves_to_its_next_unviewed_episode() {
        let show = torrent(1);
        let names = ["Show.S01E02.mkv", "Show.S01E01.mkv", "Show.S01E03.mkv"];
        let files: Vec<FileId> = names.iter().map(|name| file(name)).collect();
        let videos = || VideoFiles {
            torrent: &show,
            series: true,
            videos: files.iter().zip(names).collect(),
        };
        let entries = |saved: Vec<(usize, Progress)>| {
            let saved: BTreeMap<FileId, Progress> = saved
                .into_iter()
                .map(|(index, progress)| (files[index].clone(), progress))
                .collect();
            up_next(
                [videos()],
                &HashMap::from([(show.clone(), saved)]),
                &HashMap::new(),
            )
        };
        let resume = entries(vec![(1, saved(0, true, 1)), (0, saved(300, false, 2))]);
        assert_eq!(resume.len(), 1, "one entry per series");
        assert_eq!(
            (resume[0].reason, &resume[0].file),
            (Reason::Resume, &files[0])
        );
        let next = entries(vec![(1, saved(0, true, 3))]);
        assert_eq!(
            (next[0].reason, &next[0].file, next[0].activity_at),
            (Reason::NextEpisode, &files[0], at(3)),
            "S01E02 follows S01E01 whatever the metadata order"
        );
        let skips_viewed = entries(vec![(0, saved(0, true, 1)), (1, saved(0, true, 2))]);
        assert_eq!(skips_viewed[0].file, files[2], "S01E02 was viewed already");
        assert!(
            entries(vec![(2, saved(0, true, 4))]).is_empty(),
            "the last episode"
        );
        assert!(
            entries(vec![(0, saved(5, false, 4))]).is_empty(),
            "neither in progress nor viewed"
        );
        assert_eq!(resume[0].id, next[0].id, "a series keeps its entry ID");
    }

    #[test]
    fn other_torrents_list_each_file_in_progress_and_dismissals_hide_them() {
        let films = torrent(2);
        let names = ["One.mkv", "Two.mkv", "Three.mkv"];
        let files: Vec<FileId> = names.iter().map(|name| file(name)).collect();
        let saved: BTreeMap<FileId, Progress> = [
            (files[0].clone(), saved(300, false, 5)),
            (files[1].clone(), saved(5, false, 6)),
            (files[2].clone(), saved(400, false, 7)),
        ]
        .into();
        let progress = HashMap::from([(films.clone(), saved)]);
        let list = |dismissals: &HashMap<UpNextEntryId, SystemTime>| {
            up_next(
                [VideoFiles {
                    torrent: &films,
                    series: false,
                    videos: files.iter().zip(names).collect(),
                }],
                &progress,
                dismissals,
            )
        };
        let entries = list(&HashMap::new());
        let listed: Vec<&FileId> = entries.iter().map(|entry| &entry.file).collect();
        assert_eq!(
            listed,
            [&files[2], &files[0]],
            "most recent first, above 1%"
        );
        let dismissed = HashMap::from([(entries[0].id.clone(), at(7))]);
        assert_eq!(list(&dismissed).len(), 1);
        let earlier = HashMap::from([(entries[0].id.clone(), at(6))]);
        assert_eq!(
            list(&earlier).len(),
            2,
            "progress after a dismissal returns"
        );
    }
}
