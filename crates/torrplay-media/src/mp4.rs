// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! MP4 and QuickTime: the `moov` box's movie header and the sample tables
//! of its first video track, whose sync samples become seek points.

use std::ops::Range;
use std::time::Duration;

use crate::{
    IndexError, POINT_SPACING, Reader, SeekIndex, SeekPoint, Source, TABLE_MAX_BYTES, scaled,
};

/// The most top-level boxes walked looking for `moov`.
const BOXES_MAX: usize = 1024;

/// The longest box header: a size, a type, and a 64-bit size.
const HEADER_MAX_BYTES: u64 = 16;

/// A box: its type and the range of its data after its header.
#[derive(Clone, Debug)]
struct Mp4Box {
    kind: [u8; 4],
    data: Range<u64>,
}

/// The header at the start of `bytes`, of a box at `offset` that must end by
/// `limit`.
fn header(bytes: &[u8], offset: u64, limit: u64) -> Result<Mp4Box, IndexError> {
    let malformed = IndexError::Malformed("an MP4 box header is invalid");
    let size = u64::from(be32(bytes, 0).ok_or(malformed)?);
    let kind: [u8; 4] = bytes
        .get(4..8)
        .and_then(|kind| kind.try_into().ok())
        .ok_or(IndexError::Malformed("an MP4 box header is cut off"))?;
    let (size, header_size) = match size {
        0 => (limit - offset, 8),
        1 => (
            be64(bytes, 8).ok_or(IndexError::Malformed("an MP4 box size is cut off"))?,
            16,
        ),
        size => (size, 8),
    };
    let end = offset
        .checked_add(size)
        .filter(|end| size >= header_size && *end <= limit)
        .ok_or(IndexError::Malformed("an MP4 box overruns its parent"))?;
    Ok(Mp4Box {
        kind,
        data: offset + header_size..end,
    })
}

/// The boxes in `bytes`, as types and data.
fn boxes(bytes: &[u8]) -> impl Iterator<Item = Result<([u8; 4], &[u8]), IndexError>> {
    let mut offset = 0;
    std::iter::from_fn(move || {
        if offset >= bytes.len() {
            return None;
        }
        let parsed = header(&bytes[offset..], offset as u64, bytes.len() as u64);
        Some(parsed.map(|found| {
            // The box is inside `bytes`, so its bounds fit in usize.
            offset = found.data.end as usize;
            (
                found.kind,
                &bytes[found.data.start as usize..found.data.end as usize],
            )
        }))
        .inspect(|result| {
            if result.is_err() {
                // A malformed box ends the walk.
                offset = bytes.len();
            }
        })
    })
}

/// The first box in `bytes` of `kind`.
fn find<'a>(bytes: &'a [u8], kind: &[u8; 4]) -> Result<Option<&'a [u8]>, IndexError> {
    for entry in boxes(bytes) {
        let (found, data) = entry?;
        if &found == kind {
            return Ok(Some(data));
        }
    }
    Ok(None)
}

fn be32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

fn be64(bytes: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_be_bytes(bytes.get(at..at + 8)?.try_into().ok()?))
}

/// A full box's table: the entries after its version, flags, and count, of
/// `entry_size` bytes each, at most [`TABLE_MAX_BYTES`].
fn table(data: &[u8], entry_size: usize) -> Result<impl Iterator<Item = &[u8]>, IndexError> {
    let count = be32(data, 4).ok_or(IndexError::Malformed("an MP4 table is cut off"))?;
    let length = u64::from(count) * entry_size as u64;
    if length > TABLE_MAX_BYTES {
        return Err(IndexError::TooLarge {
            limit: TABLE_MAX_BYTES,
        });
    }
    let entries = data
        .get(8..8 + length as usize)
        .ok_or(IndexError::Malformed("an MP4 table overruns its box"))?;
    Ok(entries.chunks_exact(entry_size))
}

/// A movie or media header's timescale and duration, of either version.
fn time_header(data: &[u8]) -> Result<(u32, u64), IndexError> {
    let cut = IndexError::Malformed("an MP4 time header is cut off");
    match data.first() {
        Some(1) => Ok((
            be32(data, 20).ok_or(cut)?,
            be64(data, 24).ok_or(IndexError::Malformed("an MP4 time header is cut off"))?,
        )),
        Some(_) => Ok((
            be32(data, 12).ok_or(cut)?,
            u64::from(
                be32(data, 16).ok_or(IndexError::Malformed("an MP4 time header is cut off"))?,
            ),
        )),
        None => Err(cut),
    }
}

pub(crate) async fn read_index<S: Source + Send>(
    reader: &mut Reader<'_, S>,
) -> Result<Option<SeekIndex>, IndexError> {
    let size = reader.size();
    let mut moov = None;
    let mut media: Option<Range<u64>> = None;
    let mut offset = 0;
    for _ in 0..BOXES_MAX {
        if offset >= size {
            break;
        }
        let bytes = reader
            .read(offset, (size - offset).min(HEADER_MAX_BYTES))
            .await?;
        let found = header(&bytes, offset, size)?;
        match &found.kind {
            b"moov" => moov = Some(found.data.clone()),
            b"mdat" => {
                let data = found.data.clone();
                media = Some(match media {
                    Some(known) => known.start.min(data.start)..known.end.max(data.end),
                    None => data,
                });
            }
            // A fragmented file indexes its samples in its fragments, not
            // in moov, so its fragments are not walked.
            b"moof" => return Ok(None),
            _ => {}
        }
        offset = found.data.end;
    }
    let Some(moov) = moov else {
        return Ok(None);
    };
    let bytes = reader.read(moov.start, moov.end - moov.start).await?;
    let Some(movie) = Movie::parse(&bytes)? else {
        return Ok(None);
    };
    let (points, samples) = movie.track.points()?;
    let Some(samples) = samples else {
        return Ok(None);
    };
    let duration = [
        scaled(movie.duration, u64::from(movie.timescale)),
        scaled(movie.track.duration, u64::from(movie.track.timescale)),
    ]
    .into_iter()
    .flatten()
    .find(|duration| !duration.is_zero())
    .unwrap_or_else(|| movie.track.decoded_duration());
    Ok(SeekIndex::new(duration, media.unwrap_or(samples), points))
}

/// A movie's timescale and duration and its first video track.
struct Movie {
    timescale: u32,
    duration: u64,
    track: Track,
}

/// What a video track's tables say of its samples.
struct Track {
    timescale: u32,
    duration: u64,
    /// The movie time before the media starts, from leading empty edits.
    delay: Duration,
    /// The media time where presentation starts, from the first media edit.
    media_start: u64,
    /// Time-to-sample entries: a count of samples and their duration.
    times: Vec<(u32, u32)>,
    /// Sample-to-chunk entries: a 1-based first chunk and its samples.
    chunks: Vec<(u32, u32)>,
    offsets: Vec<u64>,
    sizes: Sizes,
    /// 1-based sync samples, ascending, or None when every sample is one.
    sync: Option<Vec<u32>>,
}

enum Sizes {
    Uniform { size: u32, count: u32 },
    Each(Vec<u32>),
}

impl Sizes {
    fn count(&self) -> u64 {
        match self {
            Self::Uniform { count, .. } => u64::from(*count),
            Self::Each(sizes) => sizes.len() as u64,
        }
    }

    fn get(&self, sample: u64) -> Option<u64> {
        match self {
            Self::Uniform { size, .. } => Some(u64::from(*size)),
            Self::Each(sizes) => usize::try_from(sample)
                .ok()
                .and_then(|index| sizes.get(index))
                .map(|size| u64::from(*size)),
        }
    }
}

impl Movie {
    /// The movie in `moov`'s data, or None without a video track.
    fn parse(moov: &[u8]) -> Result<Option<Self>, IndexError> {
        let mvhd = find(moov, b"mvhd")?.ok_or(IndexError::Malformed("moov has no mvhd"))?;
        let (timescale, duration) = time_header(mvhd)?;
        for entry in boxes(moov) {
            let (kind, trak) = entry?;
            if &kind != b"trak" {
                continue;
            }
            if let Some(track) = Track::parse(trak, timescale)? {
                return Ok(Some(Self {
                    timescale,
                    duration,
                    track,
                }));
            }
        }
        Ok(None)
    }
}

impl Track {
    /// The track in `trak`'s data, or None unless it is a video track.
    fn parse(trak: &[u8], movie_timescale: u32) -> Result<Option<Self>, IndexError> {
        let missing = |what| IndexError::Malformed(what);
        let Some(mdia) = find(trak, b"mdia")? else {
            return Ok(None);
        };
        let hdlr = find(mdia, b"hdlr")?.ok_or(missing("a track has no hdlr"))?;
        if hdlr.get(8..12) != Some(b"vide") {
            return Ok(None);
        }
        let mdhd = find(mdia, b"mdhd")?.ok_or(missing("a video track has no mdhd"))?;
        let (timescale, duration) = time_header(mdhd)?;
        if timescale == 0 {
            return Err(missing("a video track's timescale is zero"));
        }
        let (delay, media_start) = match find(trak, b"edts")?
            .map(|edts| find(edts, b"elst"))
            .transpose()?
            .flatten()
        {
            Some(elst) => edits(elst, movie_timescale)?,
            None => (Duration::ZERO, 0),
        };
        let stbl = find(mdia, b"minf")?
            .map(|minf| find(minf, b"stbl"))
            .transpose()?
            .flatten()
            .ok_or(missing("a video track has no sample table"))?;
        let mut track = Self {
            timescale,
            duration,
            delay,
            media_start,
            times: Vec::new(),
            chunks: Vec::new(),
            offsets: Vec::new(),
            sizes: Sizes::Each(Vec::new()),
            sync: None,
        };
        let pair = |entry: &[u8]| (be32(entry, 0).unwrap_or(0), be32(entry, 4).unwrap_or(0));
        for entry in boxes(stbl) {
            let (kind, data) = entry?;
            match &kind {
                b"stts" => track.times = table(data, 8)?.map(pair).collect(),
                b"stsc" => track.chunks = table(data, 12)?.map(pair).collect(),
                b"stco" => {
                    track.offsets = table(data, 4)?
                        .map(|entry| u64::from(be32(entry, 0).unwrap_or(0)))
                        .collect();
                }
                b"co64" => {
                    track.offsets = table(data, 8)?
                        .map(|entry| be64(entry, 0).unwrap_or(0))
                        .collect();
                }
                b"stsz" => track.sizes = sizes(data)?,
                b"stss" => {
                    track.sync = Some(
                        table(data, 4)?
                            .map(|entry| be32(entry, 0).unwrap_or(0))
                            .collect(),
                    );
                }
                _ => {}
            }
        }
        Ok(Some(track))
    }

    /// The presentation time of a sample decoded at `media` units.
    fn presentation(&self, media: u64) -> Duration {
        let shifted = media.saturating_sub(self.media_start);
        self.delay + scaled(shifted, u64::from(self.timescale)).unwrap_or(Duration::ZERO)
    }

    /// The decode time of every sample, for a duration no header gives.
    fn decoded_duration(&self) -> Duration {
        let units = self.times.iter().fold(0u64, |total, (count, delta)| {
            total.saturating_add(u64::from(*count) * u64::from(*delta))
        });
        self.presentation(units)
    }

    /// The sync samples' times and offsets, at least [`POINT_SPACING`]
    /// apart, and the byte range the samples span, None without samples.
    /// Samples are walked chunk by chunk in decode order.
    fn points(&self) -> Result<(Vec<SeekPoint>, Option<Range<u64>>), IndexError> {
        let mut points: Vec<SeekPoint> = Vec::new();
        let mut span: Option<Range<u64>> = None;
        let total = self.sizes.count();
        let mut sample = 0u64;
        let mut decoded = 0u64;
        let mut times = self
            .times
            .iter()
            .flat_map(|(count, delta)| std::iter::repeat_n(u64::from(*delta), *count as usize));
        let mut sync = self.sync.as_ref().map(|sync| sync.iter().peekable());
        let mut entry = 0;
        if self.chunks.first().is_some_and(|(first, _)| *first != 1) {
            return Err(IndexError::Malformed(
                "the sample-to-chunk table skips chunks",
            ));
        }
        'chunks: for (chunk, start) in self.offsets.iter().enumerate() {
            let number = chunk as u64 + 1;
            while self
                .chunks
                .get(entry + 1)
                .is_some_and(|(first, _)| u64::from(*first) <= number)
            {
                entry += 1;
            }
            let Some((_, per_chunk)) = self.chunks.get(entry) else {
                break;
            };
            let mut offset = *start;
            for _ in 0..*per_chunk {
                if sample >= total {
                    break 'chunks;
                }
                let Some(delta) = times.next() else {
                    break 'chunks;
                };
                let is_sync = match sync.as_mut() {
                    None => true,
                    Some(sync) => {
                        while sync
                            .next_if(|number| u64::from(**number) <= sample)
                            .is_some()
                        {}
                        sync.next_if(|number| u64::from(**number) == sample + 1)
                            .is_some()
                    }
                };
                let size = self
                    .sizes
                    .get(sample)
                    .ok_or(IndexError::Malformed("a sample has no size"))?;
                let end = offset
                    .checked_add(size)
                    .ok_or(IndexError::Malformed("a sample ends past 2^64"))?;
                span = Some(match span {
                    Some(known) => known.start.min(offset)..known.end.max(end),
                    None => offset..end,
                });
                if is_sync {
                    let time = self.presentation(decoded);
                    let spaced = points
                        .last()
                        .is_none_or(|last| time >= last.time + POINT_SPACING);
                    if spaced {
                        points.push(SeekPoint { time, offset });
                    }
                }
                offset = end;
                decoded = decoded.saturating_add(delta);
                sample += 1;
            }
        }
        Ok((points, span))
    }
}

/// The delay of the leading empty edits, in the movie timescale, and the
/// media time the first media edit starts at. Later edits, which cut or
/// repeat parts of the media, are not followed.
fn edits(elst: &[u8], movie_timescale: u32) -> Result<(Duration, u64), IndexError> {
    let version = *elst
        .first()
        .ok_or(IndexError::Malformed("elst is cut off"))?;
    let entries: Vec<(u64, i64)> = if version == 1 {
        table(elst, 20)?
            .map(|entry| {
                (
                    be64(entry, 0).unwrap_or(0),
                    be64(entry, 8).map_or(-1, |time| time as i64),
                )
            })
            .collect()
    } else {
        table(elst, 12)?
            .map(|entry| {
                (
                    u64::from(be32(entry, 0).unwrap_or(0)),
                    be32(entry, 4).map_or(-1, |time| i64::from(time as i32)),
                )
            })
            .collect()
    };
    let mut delay = 0u64;
    for (duration, media_time) in entries {
        match u64::try_from(media_time) {
            Ok(start) => {
                let delay = scaled(delay, u64::from(movie_timescale)).unwrap_or(Duration::ZERO);
                return Ok((delay, start));
            }
            Err(_) => delay = delay.saturating_add(duration),
        }
    }
    Ok((Duration::ZERO, 0))
}

/// An stsz box: one size for every sample, or each sample's.
fn sizes(stsz: &[u8]) -> Result<Sizes, IndexError> {
    let cut = IndexError::Malformed("stsz is cut off");
    let uniform = be32(stsz, 4).ok_or(cut)?;
    let count = be32(stsz, 8).ok_or(IndexError::Malformed("stsz is cut off"))?;
    if uniform > 0 {
        return Ok(Sizes::Uniform {
            size: uniform,
            count,
        });
    }
    // Each size follows the count, as a table's entries follow theirs.
    Ok(Sizes::Each(
        table(&stsz[4..], 4)?
            .map(|entry| be32(entry, 0).unwrap_or(0))
            .collect(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::read_index as index_of;
    use crate::testing::Mp4;

    async fn index(mp4: &Mp4, name: &str) -> Result<Option<SeekIndex>, IndexError> {
        let bytes = mp4.build();
        index_of(&mut bytes.as_slice(), name).await
    }

    #[tokio::test]
    async fn sync_samples_of_the_video_track_give_offsets() {
        for moov_last in [false, true] {
            for co64 in [false, true] {
                let mp4 = Mp4 {
                    moov_last,
                    co64,
                    ..Mp4::default()
                };
                let index = index(&mp4, "clip.mp4").await.unwrap().unwrap();
                let case = format!("moov last {moov_last}, co64 {co64}");
                assert_eq!(index.duration(), Duration::from_secs(12), "{case}");
                let (start, end) = mp4.mdat_range();
                assert_eq!(index.media_range(), start..end, "{case}");
                let expected: Vec<SeekPoint> = (0..12)
                    .map(|n| SeekPoint {
                        time: Duration::from_secs(n),
                        offset: mp4.sample_offset(25 * n as u32),
                    })
                    .collect();
                assert_eq!(index.points(), expected, "{case}");
                let middle = mp4.sample_offset(37);
                assert_eq!(
                    index.time_at(middle),
                    Some(Duration::from_millis(1480)),
                    "{case}: samples are 40 ms each"
                );
                assert_eq!(
                    index.offset_at(Duration::from_millis(2500)),
                    expected[2].offset
                );
            }
        }
    }

    #[tokio::test]
    async fn one_size_for_every_sample_gives_the_same_offsets() {
        let mp4 = Mp4 {
            uniform_sizes: true,
            ..Mp4::default()
        };
        let index = index(&mp4, "clip.mp4").await.unwrap().unwrap();
        let offsets: Vec<u64> = index.points().iter().map(|point| point.offset).collect();
        let expected: Vec<u64> = (0..12).map(|n| mp4.sample_offset(25 * n)).collect();
        assert_eq!(offsets, expected);
    }

    #[tokio::test]
    async fn every_sample_is_a_sync_sample_without_stss() {
        let mp4 = Mp4 {
            sync_every: None,
            ..Mp4::default()
        };
        let index = index(&mp4, "clip.mov").await.unwrap().unwrap();
        assert_eq!(index.points().len(), 12, "one a second is kept");
        assert_eq!(index.points()[1].offset, mp4.sample_offset(25));
    }

    #[tokio::test]
    async fn an_empty_edit_delays_and_a_media_edit_starts_presentation() {
        let mp4 = Mp4 {
            edit: Some((500, 40)),
            ..Mp4::default()
        };
        let index = index(&mp4, "clip.mp4").await.unwrap().unwrap();
        assert_eq!(
            index.points()[0].time,
            Duration::from_millis(500),
            "half a second of empty edit, then the media from its first sample"
        );
        // Sample 25, at 1.46 s, is closer than the spacing; sample 50 follows.
        assert_eq!(index.points()[1].time, Duration::from_millis(2460));
        assert_eq!(index.points()[1].offset, mp4.sample_offset(50));
    }

    #[tokio::test]
    async fn files_without_moov_a_video_track_or_samples_in_moov_have_no_index() {
        for mp4 in [
            Mp4 {
                moov: false,
                ..Mp4::default()
            },
            Mp4 {
                handler: *b"soun",
                ..Mp4::default()
            },
            Mp4 {
                fragmented: true,
                ..Mp4::default()
            },
        ] {
            assert_eq!(index(&mp4, "clip.mp4").await.unwrap(), None);
        }
    }

    #[tokio::test]
    async fn truncated_files_fail_or_have_no_index() {
        let bytes = Mp4::default().build();
        for length in (0..bytes.len()).step_by(97) {
            let mut truncated = &bytes[..length];
            match index_of(&mut truncated, "clip.mp4").await {
                Ok(index) => assert!(
                    index.is_none_or(|index| index.media_range().end <= length as u64),
                    "an index of {length} bytes stays inside them"
                ),
                Err(IndexError::Malformed(_) | IndexError::Read(_)) => {}
                Err(error) => panic!("{length} bytes: {error}"),
            }
        }
    }

    #[test]
    fn tables_past_the_bound_are_refused_before_they_are_read() {
        let entries = (TABLE_MAX_BYTES / 4) as u32;
        let mut stco = vec![0, 0, 0, 0];
        stco.extend((entries).to_be_bytes());
        assert!(
            matches!(table(&stco, 4), Err(IndexError::Malformed(_))),
            "at the bound, an absent table overruns its box"
        );
        stco[4..8].copy_from_slice(&(entries + 1).to_be_bytes());
        assert!(matches!(table(&stco, 4), Err(IndexError::TooLarge { .. })));
    }

    #[test]
    fn box_headers_must_end_inside_their_parent() {
        let mut large = vec![0, 0, 0, 1];
        large.extend(b"free");
        large.extend(20u64.to_be_bytes());
        assert_eq!(header(&large, 0, 20).unwrap().data, 16..20);
        assert!(header(&large, 0, 19).is_err());
        assert!(
            header(&[0, 0, 0, 4, b'f', b'r', b'e', b'e'], 0, 8).is_err(),
            "too small"
        );
        assert_eq!(
            header(&[0, 0, 0, 0, b'm', b'd', b'a', b't'], 0, 50)
                .unwrap()
                .data,
            8..50,
            "size 0 runs to the end"
        );
    }
}
