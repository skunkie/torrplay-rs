// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

// The cases assert as tests do; clippy's test exemption covers only test
// functions.
#![allow(clippy::unwrap_used)]

use super::*;

const BASE: Base<'static> = Base {
    origin: "http://192.168.1.10:8642",
    token: None,
};

fn file(id: &str, name: &str, media_type: &'static str, episode: Option<(u32, u32)>) -> File {
    File {
        id: id.into(),
        name: name.into(),
        media_type,
        episode,
    }
}

fn torrent(id: &str, name: &str, kind: Kind, genres: &[&str], files: Vec<File>) -> Torrent {
    Torrent {
        id: id.into(),
        title: None,
        name: name.into(),
        kind,
        imdb_id: None,
        genres: genres.iter().map(|&genre| genre.to_owned()).collect(),
        has_poster: false,
        files,
    }
}

/// Newest first: a series, two movies, and a series.
fn library() -> Library {
    let mut sintel = torrent(
        "trr_b",
        "Sintel.2010.1080p",
        Kind::Movie,
        &["Fantasy"],
        vec![file("fil_1", "Sintel.mkv", "video/x-matroska", None)],
    );
    sintel.title = Some("Sintel".into());
    sintel.has_poster = true;
    Library {
        torrents: vec![
            torrent(
                "trr_a",
                "Show.S01",
                Kind::Series,
                &["Drama"],
                vec![
                    file("fil_e1", "Show.S01E01.mp4", "video/mp4", Some((1, 1))),
                    file("fil_e2", "Show.S01E02.mp4", "video/mp4", Some((1, 2))),
                    file("fil_x", "Extras.mp4", "video/mp4", None),
                ],
            ),
            sintel,
            torrent(
                "trr_c",
                "Big Buck Bunny",
                Kind::Movie,
                &["Animation", "Comedy"],
                vec![
                    file("fil_p1", "part1.webm", "video/webm", None),
                    file("fil_p2", "part2.webm", "video/webm", None),
                ],
            ),
            torrent(
                "trr_d",
                "Other Show",
                Kind::Series,
                &["Comedy"],
                vec![file("fil_1", "Other.1x01.mp4", "video/mp4", Some((1, 1)))],
            ),
        ],
    }
}

fn ids(metas: &[protocol::MetaPreview]) -> Vec<&str> {
    metas.iter().map(|meta| meta.id.as_str()).collect()
}

#[test]
fn catalogs_list_their_kind_newest_first() {
    let library = library();
    let movies = library.catalog(Kind::Movie, &Extra::default(), BASE);
    assert_eq!(ids(&movies), ["torrplay:trr_b", "torrplay:trr_c"]);
    assert_eq!(movies[0].name, "Sintel", "the library title");
    assert_eq!(movies[1].name, "Big Buck Bunny", "else the name");
    assert_eq!(
        movies[0].poster.as_deref(),
        Some("http://192.168.1.10:8642/api/v1/torrents/trr_b/poster")
    );
    assert_eq!(movies[1].poster, None);
    let series = library.catalog(Kind::Series, &Extra::default(), BASE);
    assert_eq!(ids(&series), ["torrplay:trr_a", "torrplay:trr_d"]);
}

#[test]
fn searches_rank_matches_and_tolerate_typos() {
    let library = library();
    let search = |text: &str, kind| {
        let extra = Extra {
            search: Some(text.into()),
            ..Extra::default()
        };
        ids(&library.catalog(kind, &extra, BASE))
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    assert_eq!(search("sintel", Kind::Movie), ["torrplay:trr_b"]);
    assert_eq!(
        search("sintle", Kind::Movie),
        ["torrplay:trr_b"],
        "a typo away"
    );
    assert_eq!(search("bunny", Kind::Movie), ["torrplay:trr_c"]);
    assert_eq!(
        search("show", Kind::Series),
        ["torrplay:trr_a", "torrplay:trr_d"],
        "equal matches stay newest first"
    );
    assert!(search("nothing", Kind::Movie).is_empty());
}

#[test]
fn genres_filter_and_skip_pages() {
    let library = library();
    let comedy = Extra {
        genre: Some("Comedy".into()),
        ..Extra::default()
    };
    assert_eq!(
        ids(&library.catalog(Kind::Movie, &comedy, BASE)),
        ["torrplay:trr_c"]
    );
    assert_eq!(
        library.genres(),
        ["Animation", "Comedy", "Drama", "Fantasy"]
    );

    let many = Library {
        torrents: (0..PAGE_SIZE + 5)
            .map(|n| {
                torrent(
                    &format!("trr_{n:03}"),
                    "Film",
                    Kind::Movie,
                    &[],
                    vec![file("fil_1", "film.mp4", "video/mp4", None)],
                )
            })
            .collect(),
    };
    let first = many.catalog(Kind::Movie, &Extra::default(), BASE);
    assert_eq!(first.len(), PAGE_SIZE);
    let rest = many.catalog(
        Kind::Movie,
        &Extra {
            skip: PAGE_SIZE,
            ..Extra::default()
        },
        BASE,
    );
    assert_eq!(ids(&rest)[0], format!("torrplay:trr_{PAGE_SIZE:03}"));
    assert_eq!(rest.len(), 5);
}

#[test]
fn extra_segments_parse_percent_encoded_pairs() {
    assert_eq!(
        Extra::parse("search=big%20buck&skip=100&genre=Sci-Fi%20%26%20Fantasy&x=1").unwrap(),
        Extra {
            search: Some("big buck".into()),
            genre: Some("Sci-Fi & Fantasy".into()),
            skip: 100,
        }
    );
    assert_eq!(Extra::parse("").unwrap(), Extra::default());
    assert!(Extra::parse("skip=-1").is_err());
    assert!(Extra::parse(&format!("search={}", "a".repeat(SEARCH_MAX_CHARS + 1))).is_err());
}

#[test]
fn metadata_lists_videos_with_their_episodes() {
    let library = library();
    let series = library.meta("torrplay:trr_a", BASE).unwrap();
    let episodes: Vec<_> = series
        .videos
        .iter()
        .map(|video| (video.id.as_str(), video.season, video.episode))
        .collect();
    assert_eq!(
        episodes,
        [
            ("torrplay:trr_a:fil_e1", Some(1), Some(1)),
            ("torrplay:trr_a:fil_e2", Some(1), Some(2)),
            ("torrplay:trr_a:fil_x", None, None),
        ]
    );
    assert_eq!(series.behavior_hints, None);
    let movie = library.meta("torrplay:trr_c", BASE).unwrap();
    assert_eq!(
        movie.behavior_hints.unwrap().default_video_id,
        "torrplay:trr_c:fil_p1"
    );
    assert_eq!(library.meta("torrplay:trr_zz", BASE), None);
    assert_eq!(library.meta("tt0133093", BASE), None);
}

#[test]
fn streams_carry_the_token_the_file_name_and_their_hints() {
    let library = library();
    let base = Base {
        token: Some("tpa_secret"),
        ..BASE
    };
    let one = library.streams("torrplay:trr_b:fil_1", base);
    assert_eq!(one.len(), 1);
    assert_eq!(
        one[0].url,
        "http://192.168.1.10:8642/stremio/tpa_secret/play/trr_b/fil_1/Sintel.mkv"
    );
    assert!(
        one[0].behavior_hints.not_web_ready,
        "Matroska is not web-ready"
    );
    assert_eq!(one[0].behavior_hints.binge_group, "torrplay-trr_b");
    assert_eq!(one[0].behavior_hints.filename, "Sintel.mkv");
    let all = library.streams("torrplay:trr_c", BASE);
    assert_eq!(all.len(), 2);
    assert!(!all[0].behavior_hints.not_web_ready);
    assert!(
        all[0]
            .url
            .ends_with("/stremio/play/trr_c/fil_p1/part1.webm")
    );
    assert!(library.streams("torrplay:trr_b:fil_9", BASE).is_empty());
    assert!(library.streams("torrplay:trr_zz", BASE).is_empty());
    assert!(library.streams("tt0133093:1:2", BASE).is_empty());

    let spaced = Library {
        torrents: vec![torrent(
            "trr_s",
            "S",
            Kind::Movie,
            &[],
            vec![file("fil_1", "My Film #1.mp4", "video/mp4", None)],
        )],
    };
    assert!(
        spaced.streams("torrplay:trr_s", BASE)[0]
            .url
            .ends_with("/play/trr_s/fil_1/My%20Film%20%231.mp4")
    );
}

#[test]
fn the_manifest_serializes_in_stremios_names() {
    let manifest = serde_json::to_value(manifest("1.2.3", vec!["Drama".into()])).unwrap();
    assert_eq!(
        manifest["idPrefixes"],
        serde_json::json!(["torrplay:", "tt"])
    );
    assert_eq!(
        manifest["resources"],
        serde_json::json!([
            "catalog",
            { "name": "meta", "types": ["movie", "series"], "idPrefixes": ["torrplay:"] },
            { "name": "stream", "types": ["movie", "series"], "idPrefixes": ["torrplay:", "tt"] },
        ])
    );
    assert_eq!(manifest["catalogs"][0]["type"], "movie");
    assert_eq!(manifest["catalogs"][1]["id"], "torrplay-series");
    assert_eq!(
        manifest["catalogs"][0]["extra"],
        serde_json::json!([
            { "name": "search" },
            { "name": "skip" },
            { "name": "genre", "options": ["Drama"] }
        ])
    );
    let stream =
        serde_json::to_value(&library().streams("torrplay:trr_c:fil_p1", BASE)[0]).unwrap();
    assert_eq!(
        stream["behaviorHints"],
        serde_json::json!({ "filename": "part1.webm", "bingeGroup": "torrplay-trr_c" })
    );
}

/// A library with two torrents of one IMDb movie and a season pack of an
/// IMDb series.
fn linked() -> Library {
    let mut library = library();
    let mut copy = torrent(
        "trr_m2",
        "Big.Buck.Bunny.720p",
        Kind::Movie,
        &[],
        vec![file("fil_1", "bbb.mp4", "video/mp4", None)],
    );
    copy.imdb_id = Some("tt1254207".into());
    library.torrents.insert(0, copy);
    for torrent in &mut library.torrents {
        match torrent.id.as_str() {
            "trr_c" => torrent.imdb_id = Some("tt1254207".into()),
            "trr_a" => torrent.imdb_id = Some("tt0944947".into()),
            _ => {}
        }
    }
    library
}

#[test]
fn linked_torrents_are_listed_once_by_their_imdb_id() {
    let library = linked();
    let movies = library.catalog(Kind::Movie, &Extra::default(), BASE);
    assert_eq!(
        ids(&movies),
        ["tt1254207", "torrplay:trr_b"],
        "the newest torrent of a title stands for it"
    );
    assert_eq!(movies[0].name, "Big.Buck.Bunny.720p");
    let series = library.catalog(Kind::Series, &Extra::default(), BASE);
    assert_eq!(ids(&series), ["tt0944947", "torrplay:trr_d"]);
    assert!(
        library.meta("torrplay:trr_a", BASE).is_some(),
        "its own ID still answers"
    );
    assert_eq!(
        library.meta("tt0944947", BASE),
        None,
        "Cinemeta's to answer"
    );
}

#[test]
fn imdb_ids_stream_the_linked_files() {
    let library = linked();
    let titles = |id: &str| {
        library
            .streams(id, BASE)
            .into_iter()
            .map(|stream| stream.title)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        titles("tt1254207"),
        [
            "Big.Buck.Bunny.720p\nbbb.mp4",
            "Big Buck Bunny\npart1.webm",
            "Big Buck Bunny\npart2.webm"
        ],
        "every movie torrent's files, newest torrent first"
    );
    assert_eq!(titles("tt0944947:1:2"), ["Show.S01\nShow.S01E02.mp4"]);
    assert!(
        titles("tt0944947:1:9").is_empty(),
        "an episode the pack lacks"
    );
    assert!(
        titles("tt0944947").is_empty(),
        "a series has no movie streams"
    );
    assert!(
        titles("tt1254207:1:1").is_empty(),
        "a movie has no episodes"
    );
    for malformed in [
        "tt0944947:1",
        "tt0944947:a:2",
        "tt0944947:1:2:3",
        "tt9999999",
    ] {
        assert!(titles(malformed).is_empty(), "{malformed}");
    }
    let stream = &library.streams("tt0944947:1:1", BASE)[0];
    assert!(
        stream
            .url
            .ends_with("/stremio/play/trr_a/fil_e1/Show.S01E01.mp4")
    );
    assert_eq!(stream.behavior_hints.binge_group, "torrplay-trr_a");
}
