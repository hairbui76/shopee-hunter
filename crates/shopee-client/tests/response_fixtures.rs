//! Contract tests for every known Shopee response class.
//!
//! Each file in `tests/fixtures/shopee/` is a sanitized response plus the class
//! it must produce. Adding a newly observed response is a data change, not a
//! code change, and the coverage assertions below fail if a domain class ever
//! loses its fixture.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use serde::Deserialize;
use shopee_hunter_client::{classify_probe_response, classify_save_response, SessionProbe};
use shopee_hunter_domain::ClaimResultClass;

#[derive(Debug, Deserialize)]
struct FixtureMeta {
    source: String,
    /// `null` until a real redacted capture replaces the authored body.
    capture_date: Option<String>,
    origin: String,
    purpose: String,
    parser_version: String,
    redactions_applied: Vec<String>,
    assumption: String,
}

#[derive(Debug, Deserialize)]
struct Fixture {
    #[serde(rename = "_meta")]
    meta: FixtureMeta,
    kind: String,
    name: String,
    http_status: u16,
    expected_class: String,
    body: String,
}

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/shopee")
}

fn load_fixtures() -> Vec<(PathBuf, Fixture)> {
    let dir = fixture_dir();
    let entries = fs::read_dir(&dir)
        .unwrap_or_else(|err| panic!("fixture dir {} is unreadable: {err}", dir.display()));

    let mut fixtures: Vec<(PathBuf, Fixture)> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .map(|path| {
            let raw = fs::read_to_string(&path)
                .unwrap_or_else(|err| panic!("cannot read {}: {err}", path.display()));
            let fixture: Fixture = serde_json::from_str(&raw)
                .unwrap_or_else(|err| panic!("cannot parse {}: {err}", path.display()));
            (path, fixture)
        })
        .collect();

    fixtures.sort_by(|a, b| a.0.cmp(&b.0));
    assert!(
        !fixtures.is_empty(),
        "no fixtures found in {}",
        dir.display()
    );
    fixtures
}

#[test]
fn every_fixture_classifies_as_documented() {
    for (path, fixture) in load_fixtures() {
        let where_ = path.display();
        match fixture.kind.as_str() {
            "save" => {
                let classified = classify_save_response(fixture.http_status, &fixture.body);
                assert_eq!(
                    classified.class.as_str(),
                    fixture.expected_class,
                    "{where_}: wrong class (diagnostic: {:?})",
                    classified.diagnostic
                );
                assert_eq!(
                    classified.diagnostic.http_status, fixture.http_status,
                    "{where_}: diagnostic dropped the http status"
                );
            }
            "probe" => {
                let probe = classify_probe_response(fixture.http_status, &fixture.body);
                assert_eq!(
                    probe.as_str(),
                    fixture.expected_class,
                    "{where_}: wrong probe"
                );
            }
            other => panic!("{where_}: unknown fixture kind {other:?}"),
        }
    }
}

#[test]
fn every_claim_result_class_has_a_fixture() {
    let covered: BTreeSet<String> = load_fixtures()
        .into_iter()
        .filter(|(_, f)| f.kind == "save")
        .map(|(_, f)| f.expected_class)
        .collect();

    for class in [
        ClaimResultClass::Success,
        ClaimResultClass::AlreadySaved,
        ClaimResultClass::NotActive,
        ClaimResultClass::Expired,
        ClaimResultClass::Exhausted,
        ClaimResultClass::Ineligible,
        ClaimResultClass::InvalidVoucher,
        ClaimResultClass::SessionExpired,
        ClaimResultClass::VerificationRequired,
        ClaimResultClass::RateLimited,
        ClaimResultClass::TransientFailure,
        ClaimResultClass::UnknownResponse,
    ] {
        assert!(
            covered.contains(class.as_str()),
            "no save fixture covers {}",
            class.as_str()
        );
    }
}

#[test]
fn every_session_probe_state_has_a_fixture() {
    let covered: BTreeSet<String> = load_fixtures()
        .into_iter()
        .filter(|(_, f)| f.kind == "probe")
        .map(|(_, f)| f.expected_class)
        .collect();

    for probe in [
        SessionProbe::Healthy,
        SessionProbe::Expired,
        SessionProbe::LoginRequired,
        SessionProbe::VerificationRequired,
        SessionProbe::Transient,
        SessionProbe::Unknown,
    ] {
        assert!(
            covered.contains(probe.as_str()),
            "no probe fixture covers {}",
            probe.as_str()
        );
    }
}

#[test]
fn fixtures_carry_provenance_metadata() {
    for (path, fixture) in load_fixtures() {
        let where_ = path.display();
        assert!(!fixture.meta.source.is_empty(), "{where_}: missing source");
        assert!(
            !fixture.meta.purpose.is_empty(),
            "{where_}: missing purpose"
        );
        assert!(
            !fixture.meta.parser_version.is_empty(),
            "{where_}: missing parser_version"
        );
        assert!(
            !fixture.meta.assumption.is_empty(),
            "{where_}: missing assumption note"
        );
        assert!(
            !fixture.meta.redactions_applied.is_empty(),
            "{where_}: redactions_applied must state what was removed, even if nothing"
        );
        // `origin: synthetic` means the body was authored from documented
        // shapes and still needs a real redacted capture; a real capture must
        // record when it was taken.
        if fixture.meta.origin != "synthetic" {
            assert!(
                fixture.meta.capture_date.is_some(),
                "{where_}: a captured fixture must record capture_date"
            );
        }
        assert_eq!(
            path.file_stem().and_then(|s| s.to_str()),
            Some(fixture.name.as_str()),
            "{where_}: fixture name must match its filename"
        );
    }
}

/// Fixtures live in Git forever. A single leaked cookie would be unrecoverable,
/// so this guard runs over the raw file text rather than the parsed body.
#[test]
fn fixtures_contain_no_session_material() {
    const FORBIDDEN: &[&str] = &[
        "spc_ec",
        "spc_st",
        "spc_u",
        "spc_f",
        "set-cookie",
        "authorization",
        "bearer ",
        "csrftoken",
        "sso_token",
        "access_token",
    ];

    for (path, _) in load_fixtures() {
        let raw = fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("cannot read {}: {err}", path.display()))
            .to_lowercase();
        for needle in FORBIDDEN {
            assert!(
                !raw.contains(needle),
                "{}: contains forbidden session material {needle:?}",
                path.display()
            );
        }
    }
}

/// The classifier is on the claim path: it must be total for any byte sequence.
#[test]
fn classification_is_total_for_hostile_bodies() {
    let oversized = "a".repeat(200_000);
    let hostile = [
        "",
        " ",
        "\0\0\0",
        "null",
        "[]",
        "\"just a string\"",
        "{\"error\":",
        "<",
        "<html",
        oversized.as_str(),
        "{\"error\":0,\"error_msg\":\"\\u0000\\u0001\"}",
    ];
    for body in hostile {
        for status in [0_u16, 200, 204, 302, 400, 401, 403, 418, 429, 500, 599] {
            let classified = classify_save_response(status, body);
            assert_eq!(classified.diagnostic.http_status, status);
            if let Some(excerpt) = &classified.diagnostic.message_excerpt {
                assert!(excerpt.chars().count() <= 120, "excerpt exceeded the cap");
            }
            let _ = classify_probe_response(status, body);
        }
    }
}
