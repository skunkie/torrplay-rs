// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! Matroska and WebM: a segment's `Info`, `Tracks`, and `Cues`, found before
//! its first `Cluster` or through its `SeekHead`s, and the cue points of its
//! first video track.

use std::collections::HashMap;
use std::time::Duration;

use crate::ebml::{self, Element};
use crate::{IndexError, Reader, SeekIndex, SeekPoint, Source, TABLE_MAX_BYTES, nanos};

const SEGMENT: u64 = 0x1853_8067;
const SEEK_HEAD: u64 = 0x114D_9B74;
const SEEK: u64 = 0x4DBB;
const SEEK_ID: u64 = 0x53AB;
const SEEK_POSITION: u64 = 0x53AC;
const INFO: u64 = 0x1549_A966;
const TIMESTAMP_SCALE: u64 = 0x2A_D7B1;
const DURATION: u64 = 0x4489;
const TRACKS: u64 = 0x1654_AE6B;
const TRACK_ENTRY: u64 = 0xAE;
const TRACK_NUMBER: u64 = 0xD7;
const TRACK_TYPE: u64 = 0x83;
const CUES: u64 = 0x1C53_BB6B;
const CUE_POINT: u64 = 0xBB;
const CUE_TIME: u64 = 0xB3;
const CUE_TRACK_POSITIONS: u64 = 0xB7;
const CUE_TRACK: u64 = 0xF7;
const CUE_CLUSTER_POSITION: u64 = 0xF1;
const CLUSTER: u64 = 0x1F43_B675;

/// A video track's type.
const VIDEO_TRACK: u64 = 1;

/// The timestamp scale without an `Info` that sets it, in nanoseconds.
const DEFAULT_TIMESTAMP_SCALE: u64 = 1_000_000;

/// The most elements walked at the top level and in a segment before its
/// first cluster: headers, chapters, tags, attachments, and padding.
const ELEMENTS_MAX: usize = 256;

/// The most `SeekHead`s followed from the first.
const SEEK_HEADS_MAX: usize = 4;

/// The segment's elements before its first cluster, by ID, and the
/// segment-relative positions its seek heads list.
struct Segment {
    element: Element,
    elements: HashMap<u64, Element>,
    targets: HashMap<u64, u64>,
    first_cluster: Option<u64>,
}

pub(crate) async fn read_index<S: Source + Send>(
    reader: &mut Reader<'_, S>,
) -> Result<Option<SeekIndex>, IndexError> {
    let Some(element) = find_segment(reader).await? else {
        return Ok(None);
    };
    let segment = scan(reader, element).await?;

    let mut scale = DEFAULT_TIMESTAMP_SCALE;
    let mut duration_units = None;
    if let Some(info) = body(reader, &segment, INFO).await? {
        if let Some(data) = ebml::child(&info, TIMESTAMP_SCALE)? {
            scale = ebml::uint(data)?;
            if scale == 0 {
                return Err(IndexError::Malformed("the timestamp scale is zero"));
            }
        }
        duration_units = ebml::child(&info, DURATION)?.and_then(ebml::float);
    }
    let Some(tracks) = body(reader, &segment, TRACKS).await? else {
        return Ok(None);
    };
    let Some(video) = video_track(&tracks)? else {
        return Ok(None);
    };
    let Some(cues_element) = located(reader, &segment, CUES).await? else {
        return Ok(None);
    };
    let cues = read_body(reader, &cues_element).await?;

    let time = |units: u64| nanos(u128::from(units) * u128::from(scale));
    let start = segment.element.data.start;
    let mut points = Vec::new();
    for entry in ebml::children(&cues) {
        let (id, data) = entry?;
        if id != CUE_POINT {
            continue;
        }
        if let Some((units, position)) = cue_point(data, video)? {
            points.push(SeekPoint {
                time: time(units),
                offset: start.saturating_add(position),
            });
        }
    }

    let duration = match duration_units {
        Some(units) if units.is_finite() && units > 0.0 => {
            // A float of nanoseconds; durations past u128 do not occur.
            nanos((units * scale as f64) as u128)
        }
        _ => points
            .iter()
            .map(|point| point.time)
            .max()
            .unwrap_or(Duration::ZERO),
    };
    let segment_end = segment.element.data.end.min(reader.size());
    let media_start = segment
        .first_cluster
        .or_else(|| points.iter().map(|point| point.offset).min())
        .unwrap_or(segment_end);
    let media_end = if cues_element.offset > media_start {
        cues_element.offset.min(segment_end)
    } else {
        segment_end
    };
    Ok(SeekIndex::new(duration, media_start..media_end, points))
}

/// The first top-level segment.
async fn find_segment<S: Source + Send>(
    reader: &mut Reader<'_, S>,
) -> Result<Option<Element>, IndexError> {
    let size = reader.size();
    let mut offset = 0;
    for _ in 0..ELEMENTS_MAX {
        if offset >= size {
            break;
        }
        let element = ebml::read_header(reader, offset, size).await?;
        if element.id == SEGMENT {
            return Ok(Some(element));
        }
        offset = element.data.end;
    }
    Ok(None)
}

/// Walks the segment's elements up to its first cluster, never into the
/// clusters, which would read the whole file, and follows the seek heads.
async fn scan<S: Source + Send>(
    reader: &mut Reader<'_, S>,
    element: Element,
) -> Result<Segment, IndexError> {
    let mut segment = Segment {
        elements: HashMap::new(),
        targets: HashMap::new(),
        first_cluster: None,
        element,
    };
    let end = segment.element.data.end;
    let mut offset = segment.element.data.start;
    for _ in 0..ELEMENTS_MAX {
        if offset >= end.min(reader.size()) {
            break;
        }
        let child = ebml::read_header(reader, offset, end).await?;
        if child.id == CLUSTER {
            segment.first_cluster = Some(child.offset);
            break;
        }
        if child.id == SEEK_HEAD {
            let bytes = read_body(reader, &child).await?;
            for (id, position) in seek_targets(&bytes)? {
                segment.targets.entry(id).or_insert(position);
            }
        }
        offset = child.data.end;
        segment.elements.entry(child.id).or_insert(child);
    }
    // A seek head may list another, often after the clusters, which lists
    // what the first does not.
    for _ in 0..SEEK_HEADS_MAX {
        let Some(position) = segment.targets.remove(&SEEK_HEAD) else {
            break;
        };
        let Some(head) = at(reader, &segment, position, SEEK_HEAD).await? else {
            break;
        };
        let bytes = read_body(reader, &head).await?;
        for (id, position) in seek_targets(&bytes)? {
            segment.targets.entry(id).or_insert(position);
        }
    }
    Ok(segment)
}

/// The element with `id` at the segment-relative `position`, or None when
/// another is there.
async fn at<S: Source + Send>(
    reader: &mut Reader<'_, S>,
    segment: &Segment,
    position: u64,
    id: u64,
) -> Result<Option<Element>, IndexError> {
    let end = segment.element.data.end;
    let Some(offset) = segment.element.data.start.checked_add(position) else {
        return Ok(None);
    };
    if offset >= end.min(reader.size()) {
        return Ok(None);
    }
    let element = ebml::read_header(reader, offset, end).await?;
    Ok((element.id == id).then_some(element))
}

/// The segment's element with `id`, before its first cluster or where a
/// seek head lists it.
async fn located<S: Source + Send>(
    reader: &mut Reader<'_, S>,
    segment: &Segment,
    id: u64,
) -> Result<Option<Element>, IndexError> {
    if let Some(element) = segment.elements.get(&id) {
        return Ok(Some(element.clone()));
    }
    match segment.targets.get(&id) {
        Some(position) => at(reader, segment, *position, id).await,
        None => Ok(None),
    }
}

/// The data of the segment's element with `id`.
async fn body<S: Source + Send>(
    reader: &mut Reader<'_, S>,
    segment: &Segment,
    id: u64,
) -> Result<Option<Vec<u8>>, IndexError> {
    match located(reader, segment, id).await? {
        Some(element) => Ok(Some(read_body(reader, &element).await?)),
        None => Ok(None),
    }
}

/// Reads an element's data, at most [`TABLE_MAX_BYTES`].
async fn read_body<S: Source + Send>(
    reader: &mut Reader<'_, S>,
    element: &Element,
) -> Result<Vec<u8>, IndexError> {
    let length = element.data.end - element.data.start;
    if length > TABLE_MAX_BYTES {
        return Err(IndexError::TooLarge {
            limit: TABLE_MAX_BYTES,
        });
    }
    reader.read(element.data.start, length).await
}

/// The element IDs a seek head lists, with their segment-relative positions.
fn seek_targets(seek_head: &[u8]) -> Result<Vec<(u64, u64)>, IndexError> {
    let mut targets = Vec::new();
    for entry in ebml::children(seek_head) {
        let (id, seek) = entry?;
        if id != SEEK {
            continue;
        }
        if let (Some(target), Some(position)) = (
            ebml::child(seek, SEEK_ID)?,
            ebml::child(seek, SEEK_POSITION)?,
        ) {
            targets.push((ebml::uint(target)?, ebml::uint(position)?));
        }
    }
    Ok(targets)
}

/// The number of the first video track.
fn video_track(tracks: &[u8]) -> Result<Option<u64>, IndexError> {
    for entry in ebml::children(tracks) {
        let (id, track) = entry?;
        if id != TRACK_ENTRY {
            continue;
        }
        let kind = ebml::child(track, TRACK_TYPE)?
            .map(ebml::uint)
            .transpose()?;
        let number = ebml::child(track, TRACK_NUMBER)?
            .map(ebml::uint)
            .transpose()?;
        if let (Some(VIDEO_TRACK), Some(number)) = (kind, number) {
            return Ok(Some(number));
        }
    }
    Ok(None)
}

/// A cue point's time, in timestamp-scale units, and the segment-relative
/// position of the cluster it gives for `track`, if it gives one.
fn cue_point(point: &[u8], track: u64) -> Result<Option<(u64, u64)>, IndexError> {
    let Some(time) = ebml::child(point, CUE_TIME)? else {
        return Ok(None);
    };
    let time = ebml::uint(time)?;
    for entry in ebml::children(point) {
        let (id, positions) = entry?;
        if id != CUE_TRACK_POSITIONS {
            continue;
        }
        let cue_track = ebml::child(positions, CUE_TRACK)?
            .map(ebml::uint)
            .transpose()?;
        let cluster = ebml::child(positions, CUE_CLUSTER_POSITION)?
            .map(ebml::uint)
            .transpose()?;
        if let (Some(cue_track), Some(cluster)) = (cue_track, cluster)
            && cue_track == track
        {
            return Ok(Some((time, cluster)));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::read_index as index_of;
    use crate::testing::Mkv;

    async fn index(mkv: &Mkv, name: &str) -> Result<Option<SeekIndex>, IndexError> {
        let bytes = mkv.build();
        index_of(&mut bytes.as_slice(), name).await
    }

    #[tokio::test]
    async fn cues_of_the_video_track_give_cluster_offsets() {
        let mkv = Mkv::default();
        let bytes = mkv.build();
        let index = index_of(&mut bytes.as_slice(), "film.mkv")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(index.duration(), Duration::from_millis(10_500));
        let clusters = mkv.cluster_offsets();
        assert_eq!(
            index.points(),
            clusters
                .iter()
                .enumerate()
                .map(|(n, offset)| SeekPoint {
                    time: Duration::from_secs(2 * n as u64),
                    offset: *offset,
                })
                .collect::<Vec<_>>()
        );
        assert_eq!(index.media_range(), clusters[0]..mkv.cues_offset());
        let middle = (clusters[1] + clusters[2]) / 2;
        assert_eq!(index.time_at(middle), Some(Duration::from_secs(3)));
        assert_eq!(index.offset_at(Duration::from_secs(5)), clusters[2]);
    }

    #[tokio::test]
    async fn the_timestamp_scale_and_a_missing_duration_are_honored() {
        let mkv = Mkv {
            timestamp_scale: Some(10_000_000),
            duration: None,
            ..Mkv::default()
        };
        let index = index(&mkv, "film.webm").await.unwrap().unwrap();
        // Cue times are in hundredths of a second at this scale.
        assert_eq!(index.points()[1].time, Duration::from_secs(20));
        assert_eq!(
            index.duration(),
            Duration::from_secs(80),
            "the last cue stands for the duration"
        );
    }

    #[tokio::test]
    async fn cues_found_only_through_a_chained_seek_head_are_read() {
        let mkv = Mkv {
            chained_seek_head: true,
            ..Mkv::default()
        };
        let index = index(&mkv, "film.mkv").await.unwrap().unwrap();
        assert_eq!(index.points().len(), 5);
    }

    #[tokio::test]
    async fn files_without_cues_or_a_video_track_have_no_index() {
        for mkv in [
            Mkv {
                cues: false,
                ..Mkv::default()
            },
            Mkv {
                video: false,
                ..Mkv::default()
            },
        ] {
            assert_eq!(index(&mkv, "film.mkv").await.unwrap(), None);
        }
    }

    #[tokio::test]
    async fn a_matroska_file_named_mp4_is_identified_by_its_header() {
        let index = index(&Mkv::default(), "film.mp4").await.unwrap();
        assert_eq!(index.unwrap().points().len(), 5);
    }

    #[tokio::test]
    async fn truncated_files_fail_or_have_no_index() {
        let bytes = Mkv::default().build();
        for length in (0..bytes.len()).step_by(7) {
            let mut truncated = &bytes[..length];
            match index_of(&mut truncated, "film.mkv").await {
                Ok(index) => assert!(
                    index.is_none_or(|index| index.media_range().end <= length as u64),
                    "an index of {length} bytes stays inside them"
                ),
                Err(IndexError::Malformed(_) | IndexError::Read(_)) => {}
                Err(error) => panic!("{length} bytes: {error}"),
            }
        }
    }

    #[tokio::test]
    async fn oversized_cues_are_refused() {
        let mkv = Mkv {
            cues_padding: TABLE_MAX_BYTES as usize,
            ..Mkv::default()
        };
        assert!(matches!(
            index(&mkv, "film.mkv").await,
            Err(IndexError::TooLarge { .. })
        ));
    }
}
