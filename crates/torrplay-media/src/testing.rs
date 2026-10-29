// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! Synthetic MP4 and Matroska files for the readers' tests: valid
//! containers around zeroed media data, with the indexes the tests vary.

/// A box of `kind` around `payload`.
fn mp4_box(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut bytes = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
    bytes.extend(kind);
    bytes.extend(payload);
    bytes
}

/// A full box of `kind` with version 0 around `payload`.
fn full_box(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut data = vec![0; 4];
    data.extend(payload);
    mp4_box(kind, &data)
}

fn words(values: impl IntoIterator<Item = u32>) -> Vec<u8> {
    values.into_iter().flat_map(u32::to_be_bytes).collect()
}

/// An MP4 file with an audio track and a video track of 300 samples of
/// 100 bytes, 40 ms each, in chunks of 10: 12 seconds at 25 frames a
/// second, whose movie and media timescales are 1000.
pub(crate) struct Mp4 {
    pub(crate) moov: bool,
    pub(crate) moov_last: bool,
    pub(crate) co64: bool,
    pub(crate) fragmented: bool,
    /// Whether stsz gives one size for every sample.
    pub(crate) uniform_sizes: bool,
    /// The video track's handler.
    pub(crate) handler: [u8; 4],
    /// The spacing of sync samples, from the first, or None without stss.
    pub(crate) sync_every: Option<u32>,
    /// An empty edit's duration and the media time a media edit starts at.
    pub(crate) edit: Option<(u32, i32)>,
}

impl Default for Mp4 {
    fn default() -> Self {
        Self {
            moov: true,
            moov_last: false,
            co64: false,
            fragmented: false,
            uniform_sizes: false,
            handler: *b"vide",
            sync_every: Some(25),
            edit: None,
        }
    }
}

const SAMPLES: u32 = 300;
const SAMPLE_BYTES: u32 = 100;
const SAMPLES_PER_CHUNK: u32 = 10;

impl Mp4 {
    fn trak(&self, handler: &[u8; 4], mdat_start: u64) -> Vec<u8> {
        let mut hdlr = vec![0; 4];
        hdlr.extend(handler);
        hdlr.extend([0; 13]);
        let mut mdhd = words([0, 0, 1000, SAMPLES * 40]);
        mdhd.extend([0; 4]);
        let chunks = SAMPLES / SAMPLES_PER_CHUNK;
        let offset = |chunk: u32| mdat_start + u64::from(chunk * SAMPLES_PER_CHUNK * SAMPLE_BYTES);
        let chunk_offsets = if self.co64 {
            let mut table = words([chunks]);
            table.extend((0..chunks).flat_map(|chunk| offset(chunk).to_be_bytes()));
            full_box(b"co64", &table)
        } else {
            let mut table = words([chunks]);
            table.extend(words((0..chunks).map(|chunk| offset(chunk) as u32)));
            full_box(b"stco", &table)
        };
        let mut stbl = [
            full_box(b"stts", &words([1, SAMPLES, 40])),
            full_box(b"stsc", &words([1, 1, SAMPLES_PER_CHUNK, 1])),
            chunk_offsets,
            if self.uniform_sizes {
                full_box(b"stsz", &words([SAMPLE_BYTES, SAMPLES]))
            } else {
                full_box(
                    b"stsz",
                    &words(
                        [0, SAMPLES]
                            .into_iter()
                            .chain(std::iter::repeat_n(SAMPLE_BYTES, SAMPLES as usize)),
                    ),
                )
            },
        ]
        .concat();
        if let Some(every) = self.sync_every {
            let sync: Vec<u32> = (0..SAMPLES)
                .step_by(every as usize)
                .map(|n| n + 1)
                .collect();
            stbl.extend(full_box(
                b"stss",
                &words(std::iter::once(sync.len() as u32).chain(sync)),
            ));
        }
        let minf = mp4_box(b"minf", &mp4_box(b"stbl", &stbl));
        let mdia = mp4_box(
            b"mdia",
            &[full_box(b"mdhd", &mdhd), full_box(b"hdlr", &hdlr), minf].concat(),
        );
        let mut trak = Vec::new();
        if let Some((empty, media_time)) = self.edit {
            let entries = words([
                2,
                empty,
                u32::MAX,
                1 << 16,
                SAMPLES * 40,
                media_time as u32,
                1 << 16,
            ]);
            trak.extend(mp4_box(b"edts", &full_box(b"elst", &entries)));
        }
        trak.extend(mdia);
        mp4_box(b"trak", &trak)
    }

    fn moov(&self, mdat_start: u64) -> Vec<u8> {
        let mut mvhd = words([0, 0, 1000, SAMPLES * 40]);
        mvhd.extend([0; 80]);
        let mut moov = full_box(b"mvhd", &mvhd);
        moov.extend(self.trak(b"soun", mdat_start));
        moov.extend(self.trak(&self.handler, mdat_start));
        mp4_box(b"moov", &moov)
    }

    fn ftyp() -> Vec<u8> {
        mp4_box(b"ftyp", b"isom\0\0\0\0isom")
    }

    fn moof() -> Vec<u8> {
        mp4_box(b"moof", &full_box(b"mfhd", &words([1])))
    }

    /// Where the media data starts and ends.
    pub(crate) fn mdat_range(&self) -> (u64, u64) {
        let mut start = Self::ftyp().len() as u64 + 8;
        if self.moov && !self.moov_last {
            start += self.moov(0).len() as u64;
        }
        if self.fragmented {
            start += Self::moof().len() as u64;
        }
        (start, start + u64::from(SAMPLES * SAMPLE_BYTES))
    }

    /// The offset of the 0-based video `sample`.
    pub(crate) fn sample_offset(&self, sample: u32) -> u64 {
        self.mdat_range().0 + u64::from(sample * SAMPLE_BYTES)
    }

    pub(crate) fn build(&self) -> Vec<u8> {
        let (mdat_start, _) = self.mdat_range();
        let moov = self.moov(mdat_start);
        let mdat = mp4_box(b"mdat", &vec![0; (SAMPLES * SAMPLE_BYTES) as usize]);
        let mut bytes = Self::ftyp();
        if self.moov && !self.moov_last {
            bytes.extend(&moov);
        }
        if self.fragmented {
            bytes.extend(Self::moof());
        }
        bytes.extend(mdat);
        if self.moov && self.moov_last {
            bytes.extend(&moov);
        }
        bytes
    }
}

/// An EBML element of `id` around `payload`, with an eight-byte size, so
/// its length does not depend on the values inside it.
fn element(id: u32, payload: &[u8]) -> Vec<u8> {
    let id_bytes = id.to_be_bytes();
    let skip = id_bytes.iter().take_while(|byte| **byte == 0).count();
    let mut bytes = id_bytes[skip..].to_vec();
    bytes.push(0x01);
    bytes.extend(&(payload.len() as u64).to_be_bytes()[1..]);
    bytes.extend(payload);
    bytes
}

fn uint(id: u32, value: u64) -> Vec<u8> {
    element(id, &value.to_be_bytes())
}

/// A Matroska file with an audio track 1, a video track 2, and five clusters
/// of a thousand bytes at times 0, 2000, 4000, 6000, and 8000 in
/// timestamp-scale units, its seek head listing its cues after them.
pub(crate) struct Mkv {
    pub(crate) cues: bool,
    pub(crate) video: bool,
    pub(crate) timestamp_scale: Option<u64>,
    /// The duration in timestamp-scale units.
    pub(crate) duration: Option<f64>,
    /// Whether the first seek head lists only a second one, after the
    /// clusters, which lists the cues.
    pub(crate) chained_seek_head: bool,
    /// Padding inside the cues, to make them large.
    pub(crate) cues_padding: usize,
}

impl Default for Mkv {
    fn default() -> Self {
        Self {
            cues: true,
            video: true,
            timestamp_scale: None,
            duration: Some(10_500.0),
            chained_seek_head: false,
            cues_padding: 0,
        }
    }
}

const CLUSTERS: u64 = 5;

impl Mkv {
    fn header() -> Vec<u8> {
        element(0x1A45_DFA3, &element(0x4282, b"matroska"))
    }

    fn seek_head(target: u32, position: u64) -> Vec<u8> {
        element(
            0x114D_9B74,
            &element(
                0x4DBB,
                &[
                    element(0x53AB, &target.to_be_bytes()),
                    uint(0x53AC, position),
                ]
                .concat(),
            ),
        )
    }

    fn head(&self) -> Vec<u8> {
        let mut info = Vec::new();
        if let Some(scale) = self.timestamp_scale {
            info.extend(uint(0x2A_D7B1, scale));
        }
        if let Some(duration) = self.duration {
            info.extend(element(0x4489, &duration.to_be_bytes()));
        }
        let track = |number: u64, kind: u64| {
            element(0xAE, &[uint(0xD7, number), uint(0x83, kind)].concat())
        };
        let mut tracks = track(1, 2);
        if self.video {
            tracks.extend(track(2, 1));
        }
        [element(0x1549_A966, &info), element(0x1654_AE6B, &tracks)].concat()
    }

    fn cluster(time: u64) -> Vec<u8> {
        element(
            0x1F43_B675,
            &[uint(0xE7, time), element(0xA3, &[0; 1000])].concat(),
        )
    }

    fn cue_element(&self, clusters: &[u64]) -> Vec<u8> {
        let point = |time: u64, track: u64, cluster: u64| {
            element(
                0xBB,
                &[
                    uint(0xB3, time),
                    element(0xB7, &[uint(0xF7, track), uint(0xF1, cluster)].concat()),
                ]
                .concat(),
            )
        };
        let mut cues: Vec<u8> = clusters
            .iter()
            .enumerate()
            .flat_map(|(n, cluster)| point(2000 * n as u64, 2, *cluster))
            .collect();
        // A cue of the audio track only, which the index ignores.
        cues.extend(point(1000, 1, clusters[0]));
        if self.cues_padding > 0 {
            cues.extend(element(0xEC, &vec![0; self.cues_padding]));
        }
        element(0x1C53_BB6B, &cues)
    }

    /// The segment's data and where, relative to it, its clusters and cues
    /// start.
    fn segment(&self) -> (Vec<u8>, Vec<u64>, u64) {
        let seek_head_length = Self::seek_head(0, 0).len() as u64;
        let head = self.head();
        let mut position = seek_head_length + head.len() as u64;
        let mut clusters = Vec::new();
        let mut cluster_bytes = Vec::new();
        for n in 0..CLUSTERS {
            clusters.push(position);
            let cluster = Self::cluster(2000 * n);
            position += cluster.len() as u64;
            cluster_bytes.extend(cluster);
        }
        let mut tail = Vec::new();
        let first_target = if self.chained_seek_head {
            let second = position;
            let cues_at = second + seek_head_length;
            tail.extend(Self::seek_head(0x1C53_BB6B, cues_at));
            (0x114D_9B74, second)
        } else {
            (0x1C53_BB6B, position)
        };
        let cues_at = position + tail.len() as u64;
        if self.cues {
            tail.extend(self.cue_element(&clusters));
        }
        let data = [
            Self::seek_head(first_target.0, first_target.1),
            head,
            cluster_bytes,
            tail,
        ]
        .concat();
        (data, clusters, cues_at)
    }

    fn data_start() -> u64 {
        Self::header().len() as u64 + 4 + 8
    }

    /// The clusters' file offsets.
    pub(crate) fn cluster_offsets(&self) -> Vec<u64> {
        let start = Self::data_start();
        self.segment().1.iter().map(|at| start + at).collect()
    }

    /// The cues' file offset.
    pub(crate) fn cues_offset(&self) -> u64 {
        Self::data_start() + self.segment().2
    }

    pub(crate) fn build(&self) -> Vec<u8> {
        [Self::header(), element(0x1853_8067, &self.segment().0)].concat()
    }
}
