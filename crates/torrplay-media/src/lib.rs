// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! Seek indexes of media files: where in a file each moment of its playback
//! lies, read from the container's own index.
//!
//! MP4 and QuickTime files index their samples in the `moov` box, and
//! Matroska and WebM files list keyframe clusters in their `Cues`. Both sit
//! at a file's head or tail, so reading them costs little more than the
//! pieces a player fetches first. [`read_index`] reads only element headers
//! until it finds those elements, then reads each of them whole, within
//! [`READ_BUDGET_BYTES`], and parses them in memory.
//!
//! A [`SeekIndex`] answers both ways: the playback time at a byte offset,
//! for inferring how far a client that reports nothing has watched, and the
//! keyframe offset at a playback time, for preloading from a position.
//!
//! This crate knows nothing of torrents, engines, or HTTP; a [`Source`]
//! supplies the file's bytes.

use std::future::Future;
use std::io;
use std::ops::Range;
use std::time::Duration;

mod ebml;
mod matroska;
mod mp4;

/// The most bytes one index table may take, so a crafted file cannot make
/// the reader allocate or download more. It holds the sample sizes of four
/// million samples, over 18 hours of video at 60 frames per second.
pub const TABLE_MAX_BYTES: u64 = 16 << 20;

/// The most bytes reading one file's index may read.
pub const READ_BUDGET_BYTES: u64 = 32 << 20;

/// The most seek points an index keeps.
pub const POINTS_MAX: usize = 100_000;

/// The least playback time between kept seek points.
pub const POINT_SPACING: Duration = Duration::from_secs(1);

/// The bytes of a media file, read at offsets.
pub trait Source {
    /// The file's size in bytes.
    fn size(&self) -> u64;

    /// Fills `buffer` with the file's bytes from `offset`, failing unless
    /// all of them are read.
    fn read_at(
        &mut self,
        offset: u64,
        buffer: &mut [u8],
    ) -> impl Future<Output = io::Result<()>> + Send;
}

impl Source for &[u8] {
    fn size(&self) -> u64 {
        self.len() as u64
    }

    async fn read_at(&mut self, offset: u64, buffer: &mut [u8]) -> io::Result<()> {
        let bytes = usize::try_from(offset)
            .ok()
            .and_then(|start| Some(start..start.checked_add(buffer.len())?))
            .and_then(|range| self.get(range))
            .ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof))?;
        buffer.copy_from_slice(bytes);
        Ok(())
    }
}

/// Why a file's index could not be read.
#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    /// The source failed.
    #[error("reading the file failed")]
    Read(#[from] io::Error),
    /// The container is malformed where its index should be.
    #[error("the container is malformed: {0}")]
    Malformed(&'static str),
    /// The index is larger than the reader allows.
    #[error("the index is larger than {limit} bytes")]
    TooLarge {
        /// The bound it passed, in bytes.
        limit: u64,
    },
}

/// A moment of playback and where its data starts: a keyframe's, or the
/// cluster's holding it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SeekPoint {
    /// Its presentation time from the start of playback.
    pub time: Duration,
    /// Its byte offset in the file.
    pub offset: u64,
}

/// A file's seek index: its playback duration, the byte range of its media
/// data, and seek points ascending in both offset and time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeekIndex {
    duration: Duration,
    media: Range<u64>,
    points: Vec<SeekPoint>,
}

impl SeekIndex {
    /// An index from what a container lists, keeping the points inside the
    /// media range and the duration, ascending in both offset and time, at
    /// least [`POINT_SPACING`] apart, and at most [`POINTS_MAX`]. None
    /// without a duration, media data, or a point.
    pub(crate) fn new(
        duration: Duration,
        media: Range<u64>,
        mut points: Vec<SeekPoint>,
    ) -> Option<Self> {
        if duration.is_zero() || media.is_empty() {
            return None;
        }
        points.retain(|point| media.contains(&point.offset) && point.time <= duration);
        points.sort_by_key(|point| (point.offset, point.time));
        let mut kept: Vec<SeekPoint> = Vec::with_capacity(points.len().min(POINTS_MAX));
        for point in points {
            let spaced = kept
                .last()
                .is_none_or(|last| point.time >= last.time + POINT_SPACING);
            if spaced {
                kept.push(point);
            }
        }
        if kept.len() > POINTS_MAX {
            let step = kept.len().div_ceil(POINTS_MAX);
            kept = kept.into_iter().step_by(step).collect();
        }
        if kept.is_empty() {
            return None;
        }
        Some(Self {
            duration,
            media,
            points: kept,
        })
    }

    /// The file's playback duration.
    pub fn duration(&self) -> Duration {
        self.duration
    }

    /// The byte range holding the file's media data. Reads outside it are
    /// of headers and indexes.
    pub fn media_range(&self) -> Range<u64> {
        self.media.clone()
    }

    /// The seek points, ascending in both offset and time.
    pub fn points(&self) -> &[SeekPoint] {
        &self.points
    }

    /// The playback time at a byte offset, interpolated linearly between the
    /// seek points around it, the start of the media data counting as time
    /// zero and its end as the duration. None outside the media data.
    pub fn time_at(&self, offset: u64) -> Option<Duration> {
        if !self.media.contains(&offset) {
            return None;
        }
        let after = self.points.partition_point(|point| point.offset <= offset);
        let before = match after.checked_sub(1) {
            Some(index) => self.points[index],
            None => SeekPoint {
                time: Duration::ZERO,
                offset: self.media.start,
            },
        };
        let next = self.points.get(after).copied().unwrap_or(SeekPoint {
            time: self.duration,
            offset: self.media.end,
        });
        let span = next.offset.saturating_sub(before.offset);
        if span == 0 {
            return Some(before.time);
        }
        let elapsed = next.time.saturating_sub(before.time).as_nanos()
            * u128::from(offset - before.offset)
            / u128::from(span);
        let time = before.time + nanos(elapsed);
        Some(time.min(self.duration))
    }

    /// The offset of the last seek point at or before `time`, where playback
    /// from `time` can start, or the start of the media data before the
    /// first point.
    pub fn offset_at(&self, time: Duration) -> u64 {
        let after = self.points.partition_point(|point| point.time <= time);
        match after.checked_sub(1) {
            Some(index) => self.points[index].offset,
            None => self.media.start,
        }
    }
}

/// A duration of `nanos` nanoseconds, saturating past the largest.
pub(crate) fn nanos(nanos: u128) -> Duration {
    const PER_SECOND: u128 = 1_000_000_000;
    let seconds = u64::try_from(nanos / PER_SECOND).unwrap_or(u64::MAX);
    // The remainder is below a billion, so it fits.
    Duration::new(seconds, (nanos % PER_SECOND) as u32)
}

/// A duration of `units` at `per_second` units a second, or None for a zero
/// scale.
pub(crate) fn scaled(units: u64, per_second: u64) -> Option<Duration> {
    let per_second = u128::from(per_second);
    // A u64 times a billion fits in a u128.
    (per_second > 0).then(|| nanos(u128::from(units) * 1_000_000_000 / per_second))
}

/// A file container with a seek index this crate reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Container {
    Mp4,
    Matroska,
}

impl Container {
    /// The container a file name's extension names.
    fn by_extension(file_name: &str) -> Option<Self> {
        let (_, extension) = file_name.rsplit_once('.')?;
        match extension.to_ascii_lowercase().as_str() {
            "mp4" | "m4v" | "mov" => Some(Self::Mp4),
            "mkv" | "webm" => Some(Self::Matroska),
            _ => None,
        }
    }

    /// The container a file's first bytes identify.
    fn by_header(header: &[u8]) -> Option<Self> {
        if header.starts_with(&ebml::EBML_MAGIC) {
            Some(Self::Matroska)
        } else if header.get(4..8) == Some(b"ftyp") {
            Some(Self::Mp4)
        } else {
            None
        }
    }

    async fn read<S: Source + Send>(
        self,
        reader: &mut Reader<'_, S>,
    ) -> Result<Option<SeekIndex>, IndexError> {
        match self {
            Self::Mp4 => mp4::read_index(reader).await,
            Self::Matroska => matroska::read_index(reader).await,
        }
    }
}

/// Reads the seek index of a file named `file_name`. The extension picks the
/// likely container, and the file's header identifies the real one when that
/// container's reader finds nothing, as in a Matroska file named `.mp4`.
/// None for a file without a supported container, without a video track, or
/// without an index, such as a fragmented MP4 file or a Matroska file
/// without `Cues`.
pub async fn read_index<S: Source + Send>(
    source: &mut S,
    file_name: &str,
) -> Result<Option<SeekIndex>, IndexError> {
    let mut reader = Reader::new(source);
    let tried = Container::by_extension(file_name);
    if let Some(container) = tried {
        match container.read(&mut reader).await {
            Ok(Some(index)) => return Ok(Some(index)),
            // A file named for another container is malformed as this one.
            Ok(None) | Err(IndexError::Malformed(_)) => {}
            Err(error) => return Err(error),
        }
    }
    let length = reader.size().min(12);
    let header = reader.read(0, length).await?;
    match Container::by_header(&header) {
        Some(sniffed) if Some(sniffed) != tried => sniffed.read(&mut reader).await,
        _ => Ok(None),
    }
}

/// Reads from a source within its size and [`READ_BUDGET_BYTES`].
pub(crate) struct Reader<'a, S> {
    source: &'a mut S,
    spent: u64,
}

impl<'a, S: Source + Send> Reader<'a, S> {
    fn new(source: &'a mut S) -> Self {
        Self { source, spent: 0 }
    }

    pub(crate) fn size(&self) -> u64 {
        self.source.size()
    }

    /// The bytes [offset, offset + length), which must be in the file.
    pub(crate) async fn read(&mut self, offset: u64, length: u64) -> Result<Vec<u8>, IndexError> {
        let end = offset
            .checked_add(length)
            .filter(|end| *end <= self.size())
            .ok_or(IndexError::Malformed("an element ends past the file"))?;
        let spent = self.spent + (end - offset);
        if spent > READ_BUDGET_BYTES {
            return Err(IndexError::TooLarge {
                limit: READ_BUDGET_BYTES,
            });
        }
        self.spent = spent;
        let length = usize::try_from(length).map_err(|_| IndexError::TooLarge {
            limit: READ_BUDGET_BYTES,
        })?;
        let mut buffer = vec![0; length];
        self.source.read_at(offset, &mut buffer).await?;
        Ok(buffer)
    }
}

#[cfg(test)]
pub(crate) mod testing;

#[cfg(test)]
mod tests {
    use super::*;

    fn point(seconds: u64, offset: u64) -> SeekPoint {
        SeekPoint {
            time: Duration::from_secs(seconds),
            offset,
        }
    }

    fn index() -> SeekIndex {
        SeekIndex::new(
            Duration::from_secs(40),
            100..500,
            vec![point(10, 200), point(0, 100), point(20, 300)],
        )
        .unwrap()
    }

    #[test]
    fn time_interpolates_between_points_and_the_media_ends() {
        let index = index();
        for (offset, expected) in [
            (99, None),
            (100, Some(0)),
            (150, Some(5_000)),
            (200, Some(10_000)),
            (250, Some(15_000)),
            (400, Some(30_000)),
            (499, Some(39_900)),
            (500, None),
        ] {
            assert_eq!(
                index.time_at(offset).map(|time| time.as_millis()),
                expected,
                "at offset {offset}"
            );
        }
    }

    #[test]
    fn offset_is_the_last_point_at_or_before_a_time() {
        let index = index();
        for (seconds, expected) in [(0, 100), (9, 100), (10, 200), (35, 300), (99, 300)] {
            assert_eq!(
                index.offset_at(Duration::from_secs(seconds)),
                expected,
                "at {seconds} s"
            );
        }
    }

    #[test]
    fn points_are_kept_ordered_spaced_and_inside_the_media() {
        let index = SeekIndex::new(
            Duration::from_secs(30),
            100..500,
            vec![
                point(0, 100),
                SeekPoint {
                    time: Duration::from_millis(500),
                    offset: 120,
                },
                point(5, 50),
                point(8, 260),
                point(6, 280),
                point(40, 300),
                point(10, 600),
            ],
        )
        .unwrap();
        assert_eq!(
            index.points(),
            [point(0, 100), point(8, 260)],
            "closer than the spacing, backwards in time, past the duration, or outside the media"
        );
    }

    #[test]
    fn points_past_the_most_are_thinned_evenly() {
        let count = POINTS_MAX as u64 + 10;
        let points = (0..count).map(|n| point(n, 1000 + n)).collect();
        let index = SeekIndex::new(Duration::from_secs(count), 1000..1000 + count, points).unwrap();
        assert!(index.points().len() <= POINTS_MAX);
        assert_eq!(index.points()[0], point(0, 1000));
        assert_eq!(index.points()[1], point(2, 1002));
    }

    #[test]
    fn an_index_needs_a_duration_media_and_a_point() {
        assert_eq!(
            SeekIndex::new(Duration::ZERO, 0..10, vec![point(0, 0)]),
            None
        );
        assert_eq!(
            SeekIndex::new(Duration::from_secs(1), 5..5, vec![point(0, 5)]),
            None
        );
        assert_eq!(
            SeekIndex::new(Duration::from_secs(1), 0..10, vec![point(0, 20)]),
            None
        );
    }

    #[test]
    fn containers_are_named_by_extension_and_identified_by_header() {
        assert_eq!(
            Container::by_extension("Film.MKV"),
            Some(Container::Matroska)
        );
        assert_eq!(
            Container::by_extension("clip.webm"),
            Some(Container::Matroska)
        );
        assert_eq!(Container::by_extension("clip.m4v"), Some(Container::Mp4));
        assert_eq!(Container::by_extension("clip.avi"), None);
        assert_eq!(Container::by_extension("noextension"), None);
        assert_eq!(
            Container::by_header(&[0x1A, 0x45, 0xDF, 0xA3, 0, 0, 0, 0]),
            Some(Container::Matroska)
        );
        assert_eq!(
            Container::by_header(b"\0\0\0\x18ftypisom"),
            Some(Container::Mp4)
        );
        assert_eq!(Container::by_header(b"RIFF\0\0\0\0AVI "), None);
    }

    #[tokio::test]
    async fn reads_are_bounded_by_the_file_and_the_budget() {
        let bytes = vec![0; 64];
        let mut source = bytes.as_slice();
        let mut reader = Reader::new(&mut source);
        assert!(matches!(
            reader.read(60, 8).await,
            Err(IndexError::Malformed(_))
        ));
        assert!(matches!(
            reader.read(u64::MAX, 2).await,
            Err(IndexError::Malformed(_))
        ));
        assert_eq!(reader.read(0, 64).await.unwrap().len(), 64);
        reader.spent = READ_BUDGET_BYTES - 1;
        assert!(matches!(
            reader.read(0, 2).await,
            Err(IndexError::TooLarge { .. })
        ));
        assert!(reader.read(0, 1).await.is_ok(), "the budget's last byte");
    }

    #[test]
    fn reading_an_index_can_move_between_threads() {
        fn sendable<T: Send>(_: T) {}
        let bytes = Vec::new();
        let mut source = bytes.as_slice();
        sendable(read_index(&mut source, "film.mkv"));
    }

    #[test]
    fn scaled_durations_round_down_and_refuse_a_zero_scale() {
        assert_eq!(scaled(3, 2), Some(Duration::from_millis(1500)));
        assert_eq!(scaled(1, 3), Some(Duration::from_nanos(333_333_333)));
        assert_eq!(scaled(1, 0), None);
        assert_eq!(scaled(u64::MAX, 1), Some(Duration::from_secs(u64::MAX)));
    }
}
