// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! Page sizes and opaque cursors, as the API conventions' Collections and
//! pagination section defines them.
//!
//! A collection sorts its items by a [`SortKey`] that is unique per item and
//! never changes while the item exists. A cursor encodes the last returned
//! item's key, and the next page holds the items whose keys follow it. Pages
//! therefore neither repeat nor skip items that exist for the whole
//! traversal, whatever is created or deleted between requests.
//!
//! A cursor is lowercase base32 without padding, so it is URL-safe. It holds
//! a format version, the collection's key kind, a fingerprint of the filters
//! it was issued for, and the key. Cursors are not authenticated: a client
//! that forges one only chooses where its page starts.

use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::time::{Duration, SystemTime};

use sha2::{Digest, Sha256};

use crate::base32;
use crate::id::TorrentId;
use crate::lifecycle::ObservedState;
use crate::registry::TorrentRecord;

/// The query parameter that sets the page length.
pub const PAGE_SIZE_PARAMETER: &str = "page_size";

/// The query parameter that carries a cursor.
pub const CURSOR_PARAMETER: &str = "cursor";

/// The longest cursor accepted, which bounds decoding work. Issued cursors
/// are far shorter.
const MAX_CURSOR_CHARS: usize = 1024;

/// The cursor format version.
const CURSOR_VERSION: u8 = 1;

/// The number of bytes of a filter fingerprint.
const FINGERPRINT_BYTES: usize = 8;

/// A pagination parameter that cannot be used. Each is a
/// `malformed-request` naming [`parameter`](Self::parameter).
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PageError {
    /// `page_size` is not an integer from 1 to 200.
    #[error("Must be an integer from 1 to 200.")]
    InvalidPageSize,
    /// `cursor` was not issued by this collection.
    #[error("Must be a cursor from a next link.")]
    MalformedCursor,
    /// `cursor` was issued for other filters.
    #[error("Must be used with the filters it was issued for.")]
    CursorFilterMismatch,
}

impl PageError {
    /// Returns the name of the query parameter at fault.
    pub fn parameter(self) -> &'static str {
        match self {
            Self::InvalidPageSize => PAGE_SIZE_PARAMETER,
            Self::MalformedCursor | Self::CursorFilterMismatch => CURSOR_PARAMETER,
        }
    }
}

/// The length of a page, from 1 to 200.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageSize(u8);

impl PageSize {
    /// The smallest page length.
    pub const MIN: u8 = 1;
    /// The largest page length.
    pub const MAX: u8 = 200;
    /// The page length when `page_size` is omitted.
    pub const DEFAULT: Self = Self(50);

    /// Accepts a page length within the bounds; values outside them are
    /// rejected rather than clamped.
    pub fn new(value: i64) -> Result<Self, PageError> {
        u8::try_from(value)
            .ok()
            .filter(|value| (Self::MIN..=Self::MAX).contains(value))
            .map(Self)
            .ok_or(PageError::InvalidPageSize)
    }

    /// Parses the `page_size` query parameter, or returns the default when
    /// it is absent.
    pub fn parse(value: Option<&str>) -> Result<Self, PageError> {
        match value {
            None => Ok(Self::DEFAULT),
            Some(text) => text
                .parse::<i64>()
                .map_err(|_| PageError::InvalidPageSize)
                .and_then(Self::new),
        }
    }

    /// Returns the page length.
    pub fn get(self) -> usize {
        usize::from(self.0)
    }
}

impl Default for PageSize {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// A fingerprint of a request's filters, carried in cursors so that a
/// cursor works only with the filters it was issued for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FilterFingerprint([u8; FINGERPRINT_BYTES]);

impl FilterFingerprint {
    /// Fingerprints filters given as parameter names and values. Order and
    /// repetition do not matter, so `?a=1&a=2` and `?a=2&a=1&a=1` match.
    pub fn of<'a>(filters: impl IntoIterator<Item = (&'a str, &'a str)>) -> Self {
        let filters: BTreeSet<_> = filters.into_iter().collect();
        let mut hasher = Sha256::new();
        for (name, value) in filters {
            for part in [name, value] {
                hasher.update((part.len() as u64).to_be_bytes());
                hasher.update(part);
            }
        }
        let digest = hasher.finalize();
        let mut fingerprint = [0; FINGERPRINT_BYTES];
        fingerprint.copy_from_slice(&digest[..FINGERPRINT_BYTES]);
        Self(fingerprint)
    }

    /// Fingerprints a request without filters.
    pub fn none() -> Self {
        Self::of([])
    }
}

/// The sort key of a collection's items. Keys must be unique per item and
/// must not change while the item exists; items are listed in ascending key
/// order.
pub trait SortKey: Ord + Sized {
    /// Distinguishes the cursors of different collections.
    const KIND: u8;

    /// Appends the key's encoding.
    fn encode(&self, out: &mut Vec<u8>);

    /// Decodes a key from its complete encoding.
    fn decode(bytes: &[u8]) -> Option<Self>;
}

/// A validated page request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageRequest<K> {
    page_size: PageSize,
    after: Option<K>,
    filters: FilterFingerprint,
}

impl<K: SortKey> PageRequest<K> {
    /// Validates a request's cursor against its filters. A request without a
    /// cursor starts at the first item.
    pub fn new(
        page_size: PageSize,
        cursor: Option<&str>,
        filters: FilterFingerprint,
    ) -> Result<Self, PageError> {
        let after = cursor
            .map(|cursor| decode_cursor(cursor, filters))
            .transpose()?;
        Ok(Self {
            page_size,
            after,
            filters,
        })
    }

    /// Returns the page length.
    pub fn page_size(&self) -> PageSize {
        self.page_size
    }
}

/// One page of a collection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Page<T> {
    /// The items, in key order.
    pub items: Vec<T>,
    /// The cursor of the next page, or `None` on the last page.
    pub next_cursor: Option<String>,
}

/// Returns the page `request` asks for from `items`, which must already
/// match the request's filters and may come in any order.
pub fn paginate<T, K: SortKey>(
    items: impl IntoIterator<Item = T>,
    key: impl Fn(&T) -> K,
    request: &PageRequest<K>,
) -> Page<T> {
    let mut remaining: Vec<(K, T)> = items
        .into_iter()
        .map(|item| (key(&item), item))
        .filter(|(key, _)| request.after.as_ref().is_none_or(|after| key > after))
        .collect();
    remaining.sort_unstable_by(|(a, _), (b, _)| a.cmp(b));
    let more = remaining.len() > request.page_size.get();
    remaining.truncate(request.page_size.get());
    let next_cursor = more
        .then(|| remaining.last())
        .flatten()
        .map(|(key, _)| encode_cursor(key, request.filters));
    Page {
        items: remaining.into_iter().map(|(_, item)| item).collect(),
        next_cursor,
    }
}

fn encode_cursor<K: SortKey>(key: &K, filters: FilterFingerprint) -> String {
    let mut bytes = vec![CURSOR_VERSION, K::KIND];
    bytes.extend_from_slice(&filters.0);
    key.encode(&mut bytes);
    base32::encode(&bytes)
}

fn decode_cursor<K: SortKey>(cursor: &str, filters: FilterFingerprint) -> Result<K, PageError> {
    if cursor.len() > MAX_CURSOR_CHARS {
        return Err(PageError::MalformedCursor);
    }
    let bytes = base32::decode(cursor).ok_or(PageError::MalformedCursor)?;
    let (header, key) = bytes
        .split_at_checked(2 + FINGERPRINT_BYTES)
        .ok_or(PageError::MalformedCursor)?;
    if header[..2] != [CURSOR_VERSION, K::KIND] {
        return Err(PageError::MalformedCursor);
    }
    let key = K::decode(key).ok_or(PageError::MalformedCursor)?;
    if header[2..] != filters.0 {
        return Err(PageError::CursorFilterMismatch);
    }
    Ok(key)
}

/// The sort key of the torrents collection: newest first by creation time,
/// then by torrent ID.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TorrentSortKey {
    /// The creation time since the Unix epoch; earlier times count as the
    /// epoch.
    created_at: Duration,
    id: TorrentId,
}

impl TorrentSortKey {
    /// Returns the key of a registered torrent.
    pub fn of(record: &TorrentRecord) -> Self {
        Self {
            created_at: record
                .created_at
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default(),
            id: record.id.clone(),
        }
    }
}

impl Ord for TorrentSortKey {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .created_at
            .cmp(&self.created_at)
            .then_with(|| self.id.cmp(&other.id))
    }
}

impl PartialOrd for TorrentSortKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl SortKey for TorrentSortKey {
    const KIND: u8 = 1;

    fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.created_at.as_secs().to_be_bytes());
        out.extend_from_slice(&self.created_at.subsec_nanos().to_be_bytes());
        out.extend_from_slice(self.id.as_str().as_bytes());
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        let (secs, rest) = bytes.split_first_chunk::<8>()?;
        let (nanos, id) = rest.split_first_chunk::<4>()?;
        let nanos = u32::from_be_bytes(*nanos);
        if nanos >= 1_000_000_000 || id.is_empty() {
            return None;
        }
        let id = String::from_utf8(id.to_vec()).ok()?;
        Some(Self {
            created_at: Duration::new(u64::from_be_bytes(*secs), nanos),
            id: TorrentId::from_issued(id),
        })
    }
}

/// The filters of the torrents collection.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TorrentFilter {
    observed_states: BTreeSet<ObservedState>,
}

impl TorrentFilter {
    /// Matches torrents in any of `observed_states`, or every torrent when
    /// it is empty.
    pub fn new(observed_states: impl IntoIterator<Item = ObservedState>) -> Self {
        Self {
            observed_states: observed_states.into_iter().collect(),
        }
    }

    /// Returns whether a torrent in `observed_state` matches.
    pub fn matches(&self, observed_state: ObservedState) -> bool {
        self.observed_states.is_empty() || self.observed_states.contains(&observed_state)
    }

    /// Returns the fingerprint cursors carry.
    pub fn fingerprint(&self) -> FilterFingerprint {
        FilterFingerprint::of(
            self.observed_states
                .iter()
                .map(|state| ("observed_state", state.as_str())),
        )
    }
}

/// The sort key of a torrent's files collection: metadata order, as the
/// position in [`Metadata::files`](crate::metadata::Metadata::files).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct FileSortKey(pub u64);

impl SortKey for FileSortKey {
    const KIND: u8 = 2;

    fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.0.to_be_bytes());
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        Some(Self(u64::from_be_bytes(bytes.try_into().ok()?)))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::id::InfoHashV1;
    use crate::lifecycle::{DesiredState, StorageMode};
    use crate::registry::{NewTorrent, Registration, Registry};

    fn request<K: SortKey>(
        page_size: i64,
        cursor: Option<&str>,
        filters: FilterFingerprint,
    ) -> Result<PageRequest<K>, PageError> {
        PageRequest::new(PageSize::new(page_size)?, cursor, filters)
    }

    #[test]
    fn page_size_accepts_only_1_to_200() {
        let cases = [
            (Some("1"), Ok(1)),
            (Some("200"), Ok(200)),
            (None, Ok(50)),
            (Some("0"), Err(PageError::InvalidPageSize)),
            (Some("201"), Err(PageError::InvalidPageSize)),
            (Some("-1"), Err(PageError::InvalidPageSize)),
            (Some("256"), Err(PageError::InvalidPageSize)),
            (Some("1.5"), Err(PageError::InvalidPageSize)),
            (Some(""), Err(PageError::InvalidPageSize)),
            (
                Some("99999999999999999999"),
                Err(PageError::InvalidPageSize),
            ),
        ];
        for (text, expected) in cases {
            assert_eq!(
                PageSize::parse(text).map(PageSize::get),
                expected,
                "page_size {text:?}"
            );
        }
        assert_eq!(PageError::InvalidPageSize.parameter(), "page_size");
    }

    #[test]
    fn files_are_paged_in_metadata_order() {
        let positions: Vec<u64> = (0..7).collect();
        let mut cursor = None;
        let mut seen = Vec::new();
        loop {
            let request =
                request::<FileSortKey>(3, cursor.as_deref(), FilterFingerprint::none()).unwrap();
            let page = paginate(
                positions.iter().rev().copied(),
                |&p| FileSortKey(p),
                &request,
            );
            seen.extend(page.items);
            match page.next_cursor {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        assert_eq!(seen, positions);
    }

    #[test]
    fn the_last_page_has_no_next_cursor() {
        let request = request::<FileSortKey>(3, None, FilterFingerprint::none()).unwrap();
        let exact = paginate(0..3u64, |&p| FileSortKey(p), &request);
        assert_eq!(exact.items, [0, 1, 2]);
        assert_eq!(exact.next_cursor, None, "a full last page has no next link");
        let empty = paginate(Vec::<u64>::new(), |&p| FileSortKey(p), &request);
        assert_eq!(empty.items, Vec::<u64>::new());
        assert_eq!(empty.next_cursor, None);
    }

    #[test]
    fn rejects_malformed_and_foreign_cursors() {
        let none = FilterFingerprint::none();
        let files_cursor = paginate(
            0..5u64,
            |&p| FileSortKey(p),
            &request(2, None, none).unwrap(),
        )
        .next_cursor
        .unwrap();
        let mut bad_version = base32::decode(&files_cursor).unwrap();
        bad_version[0] = 9;
        let mut truncated = base32::decode(&files_cursor).unwrap();
        truncated.pop();
        let cases = [
            "".to_owned(),
            "!!!".to_owned(),
            files_cursor.to_uppercase(),
            base32::encode(&bad_version),
            base32::encode(&truncated),
            "a".repeat(MAX_CURSOR_CHARS + 8),
            base32::encode(b"\x01\x02"),
        ];
        for cursor in &cases {
            assert_eq!(
                request::<FileSortKey>(2, Some(cursor), none),
                Err(PageError::MalformedCursor),
                "{cursor:?}"
            );
        }
        assert_eq!(
            request::<TorrentSortKey>(2, Some(&files_cursor), none),
            Err(PageError::MalformedCursor),
            "a files cursor is not a torrents cursor"
        );
        assert_eq!(PageError::MalformedCursor.parameter(), "cursor");
    }

    #[test]
    fn a_cursor_works_only_with_its_filters() {
        let paused = TorrentFilter::new([ObservedState::Paused]);
        let both = TorrentFilter::new([ObservedState::Downloading, ObservedState::Paused]);
        let reordered = TorrentFilter::new([
            ObservedState::Paused,
            ObservedState::Downloading,
            ObservedState::Paused,
        ]);
        assert_eq!(both.fingerprint(), reordered.fingerprint());
        assert_ne!(paused.fingerprint(), both.fingerprint());
        assert_ne!(TorrentFilter::default().fingerprint(), paused.fingerprint());

        let records = registered(&Registry::new(), 3, u64::from);
        let first = paginate(
            records,
            TorrentSortKey::of,
            &request(1, None, both.fingerprint()).unwrap(),
        );
        let cursor = first.next_cursor.unwrap();
        assert!(request::<TorrentSortKey>(1, Some(&cursor), reordered.fingerprint()).is_ok());
        for other in [paused, TorrentFilter::default()] {
            assert_eq!(
                request::<TorrentSortKey>(1, Some(&cursor), other.fingerprint()),
                Err(PageError::CursorFilterMismatch)
            );
        }
        assert_eq!(PageError::CursorFilterMismatch.parameter(), "cursor");
    }

    #[test]
    fn torrent_filters_match_any_listed_state() {
        let filter = TorrentFilter::new([ObservedState::Downloading, ObservedState::Paused]);
        assert!(filter.matches(ObservedState::Paused));
        assert!(!filter.matches(ObservedState::Failed));
        assert!(
            ObservedState::ALL
                .into_iter()
                .all(|state| TorrentFilter::default().matches(state))
        );
    }

    #[test]
    fn torrents_sort_newest_first_with_the_id_as_tie_breaker() {
        let registry = Registry::with_id_source({
            let mut n = 0u8;
            move || {
                n += 1;
                Ok([10 - n; 16])
            }
        });
        // Creation times 0, 1, 1, 2: the two torrents created at second 1
        // sort by ID, whatever their creation order.
        let records = registered(&registry, 4, |n| [0, 1, 1, 2][usize::from(n)]);
        let request = request(10, None, FilterFingerprint::none()).unwrap();
        let page = paginate(records.clone(), TorrentSortKey::of, &request);
        let order: Vec<_> = page.items.iter().map(|record| record.id.clone()).collect();
        let (tied_low, tied_high) = if records[1].id < records[2].id {
            (records[1].id.clone(), records[2].id.clone())
        } else {
            (records[2].id.clone(), records[1].id.clone())
        };
        assert_eq!(
            order,
            [
                records[3].id.clone(),
                tied_low,
                tied_high,
                records[0].id.clone()
            ]
        );
    }

    #[test]
    fn torrent_keys_round_trip_through_cursors() {
        let key = TorrentSortKey {
            created_at: Duration::new(1_792_411_200, 123_456_789),
            id: TorrentId::from_random_bytes([3; 16]),
        };
        let filters = FilterFingerprint::none();
        let cursor = encode_cursor(&key, filters);
        assert!(
            cursor
                .bytes()
                .all(|byte| matches!(byte, b'a'..=b'z' | b'2'..=b'7')),
            "cursors are URL-safe"
        );
        assert_eq!(decode_cursor::<TorrentSortKey>(&cursor, filters), Ok(key));
    }

    /// Registers `count` torrents, the `n`th created at second
    /// `created_at(n)`, and returns their records.
    fn registered(
        registry: &Registry,
        count: u8,
        created_at: impl Fn(u8) -> u64,
    ) -> Vec<TorrentRecord> {
        (0..count)
            .map(|n| register(registry, u32::from(n), created_at(n)))
            .collect()
    }

    fn register(registry: &Registry, n: u32, created_at_secs: u64) -> TorrentRecord {
        let mut hash = [0; 20];
        hash[..4].copy_from_slice(&n.to_be_bytes());
        let new = NewTorrent {
            info_hash: InfoHashV1::from_bytes(hash),
            metadata: None,
            desired_state: DesiredState::Started,
            storage_mode: StorageMode::Memory,
            created_at: SystemTime::UNIX_EPOCH + Duration::from_secs(created_at_secs),
        };
        match registry.register(new).unwrap() {
            Registration::Created(record) => record,
            Registration::Duplicate { .. } => panic!("info hash {n} was already registered"),
        }
    }

    /// A small deterministic generator for the traversal test.
    struct XorShift(u64);

    impl XorShift {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn below(&mut self, bound: u64) -> u64 {
            self.next() % bound
        }
    }

    #[test]
    fn traversals_neither_repeat_nor_skip_torrents_that_exist_throughout() {
        for seed in 1..=200u64 {
            let mut random = XorShift(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
            let registry = Registry::new();
            let mut next_hash = 0u32;
            let mut clock = 1_000u64;
            for _ in 0..random.below(40) {
                // Few distinct creation times, so ties are common.
                clock += random.below(2);
                register(&registry, next_hash, clock);
                next_hash += 1;
            }
            let initial: HashSet<TorrentId> = registry
                .list()
                .into_iter()
                .map(|record| record.id)
                .collect();
            let mut deleted = HashSet::new();
            let mut seen = Vec::new();
            let page_size = 1 + random.below(7) as i64;
            let mut cursor: Option<String> = None;
            let mut pages = 0;
            loop {
                let request =
                    request(page_size, cursor.as_deref(), FilterFingerprint::none()).unwrap();
                let page = paginate(registry.list(), TorrentSortKey::of, &request);
                assert!(page.items.len() <= page_size as usize);
                seen.extend(page.items.into_iter().map(|record| record.id));
                pages += 1;
                // Between pages, create torrents (some with creation times
                // in the past) and delete random ones.
                for _ in 0..random.below(4) {
                    let created_at = clock + random.below(3) - 1;
                    register(&registry, next_hash, created_at);
                    next_hash += 1;
                }
                for _ in 0..random.below(3) {
                    let records = registry.list();
                    if records.is_empty() {
                        break;
                    }
                    let victim = &records[random.below(records.len() as u64) as usize];
                    deleted.insert(victim.id.clone());
                    registry.remove(victim.id.as_str());
                }
                match page.next_cursor {
                    Some(next) => cursor = Some(next),
                    None => break,
                }
                assert!(pages < 1_000, "seed {seed}: the traversal must end");
            }
            let mut unique = HashSet::new();
            for id in &seen {
                assert!(unique.insert(id.clone()), "seed {seed}: {id} repeated");
            }
            for id in initial.difference(&deleted) {
                assert!(unique.contains(id), "seed {seed}: {id} skipped");
            }
        }
    }
}
