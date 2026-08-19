//! Access to the CycloneDX SBOM that ships inside the binary.
//!
//! Two representations are embedded, for two different jobs:
//!
//! * [`RAW_JSON`] — the byte-for-byte CycloneDX document, so **Export SBOM** in
//!   the About dialog writes exactly the file that was published with the
//!   release. An SBOM the user cannot extract and diff is not much of an SBOM.
//! * [`crate::sbom_data::COMPONENTS`] — the same data pre-flattened into a Rust
//!   table by `scripts/make-sbom.py`, so rendering the About list needs no JSON
//!   parser at runtime. That keeps a parser off the attack surface of a
//!   permanently-resident process.
//!
//! Both come from the same generator run, and CI runs
//! `python scripts/make-sbom.py --check` so they cannot drift apart or go stale
//! against `Cargo.lock`.

pub use crate::sbom_data::{Component, COMPONENTS, SBOM_SERIAL, SBOM_SPEC_VERSION, SBOM_TIMESTAMP};

/// The published CycloneDX 1.5 document, verbatim.
pub const RAW_JSON: &str = include_str!("../docs/compliance/sbom.cdx.json");

/// Where the CRA documentation lives, surfaced as links in the About dialog.
pub const CRA_DOC_URL: &str =
    "https://github.com/andreaswiren/supertile/blob/main/docs/compliance/EU-CRA.md";
pub const SBOM_URL: &str =
    "https://github.com/andreaswiren/supertile/blob/main/docs/compliance/sbom.cdx.json";
pub const SECURITY_URL: &str = "https://github.com/andreaswiren/supertile/blob/main/SECURITY.md";
/// The Regulation itself, for anyone who wants the primary source.
pub const CRA_REGULATION_URL: &str = "https://eur-lex.europa.eu/eli/reg/2024/2847/oj";

pub fn component_count() -> usize {
    COMPONENTS.len()
}

/// Distinct licence expressions, sorted, with a count each.
pub fn license_summary() -> Vec<(&'static str, usize)> {
    let mut out: Vec<(&'static str, usize)> = Vec::new();
    for c in COMPONENTS {
        let lic = if c.license.is_empty() {
            "unspecified"
        } else {
            c.license
        };
        match out.iter_mut().find(|(l, _)| *l == lic) {
            Some((_, n)) => *n += 1,
            None => out.push((lic, 1)),
        }
    }
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    out
}

/// Write the embedded SBOM to disk.
pub fn export_to(path: &std::path::Path) -> std::io::Result<()> {
    std::fs::write(path, RAW_JSON.as_bytes())
}

/// A short abbreviation of a SHA-256, for display in a narrow column.
pub fn short_hash(sha: &str) -> &str {
    if sha.len() >= 12 {
        &sha[..12]
    } else {
        sha
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sbom_is_not_empty() {
        assert!(component_count() > 0, "no components embedded");
        assert!(!RAW_JSON.trim().is_empty());
    }

    #[test]
    fn every_component_is_fully_described() {
        for c in COMPONENTS {
            assert!(!c.name.is_empty(), "component with no name");
            assert!(!c.version.is_empty(), "{} has no version", c.name);
            assert!(!c.purl.is_empty(), "{} has no purl", c.name);
            // A licence is what makes the SBOM useful for compliance review.
            assert!(!c.license.is_empty(), "{} has no licence", c.name);
        }
    }

    #[test]
    fn registry_components_carry_a_sha256() {
        // Crates fetched from crates.io must be pinned by hash; that is the
        // integrity evidence CRA Annex I expects for third-party components.
        for c in COMPONENTS {
            if c.purl.starts_with("pkg:cargo/") && !c.purl.contains("supertile") {
                assert_eq!(c.sha256.len(), 64, "{} has a malformed SHA-256", c.name);
                assert!(
                    c.sha256.chars().all(|ch| ch.is_ascii_hexdigit()),
                    "{} SHA-256 is not hex",
                    c.name
                );
            }
        }
    }

    #[test]
    fn the_embedded_json_agrees_with_the_generated_table() {
        // Cheap structural check that the two embeddings came from one run.
        assert!(RAW_JSON.contains(SBOM_SERIAL), "serial number mismatch");
        assert!(RAW_JSON.contains(SBOM_TIMESTAMP), "timestamp mismatch");
        assert!(
            RAW_JSON.contains(r#""specVersion": "1.5""#),
            "unexpected spec version"
        );
        for c in COMPONENTS {
            assert!(
                RAW_JSON.contains(c.purl),
                "{} missing from the JSON",
                c.purl
            );
        }
    }

    #[test]
    fn the_json_is_a_cyclonedx_document() {
        assert!(RAW_JSON.contains(r#""bomFormat": "CycloneDX""#));
        assert!(
            RAW_JSON.contains(r#""cra:supportPeriodYears""#),
            "CRA properties missing"
        );
        assert!(
            RAW_JSON.contains(r#""supplier""#),
            "supplier identification missing"
        );
    }

    #[test]
    fn no_build_machine_paths_leaked_into_the_sbom() {
        // The raw cargo-cyclonedx output embeds absolute source paths.
        let lower = RAW_JSON.to_lowercase();
        assert!(!lower.contains("c:/users"), "absolute build path leaked");
        assert!(!lower.contains("c:\\\\users"), "absolute build path leaked");
        assert!(!lower.contains("/home/"), "absolute build path leaked");
    }

    #[test]
    fn the_key_dependencies_are_present() {
        let names: Vec<&str> = COMPONENTS.iter().map(|c| c.name).collect();
        for want in ["windows", "serde", "toml"] {
            assert!(
                names.contains(&want),
                "{want} missing from the SBOM: {names:?}"
            );
        }
    }

    #[test]
    fn license_summary_covers_every_component() {
        let total: usize = license_summary().iter().map(|(_, n)| n).sum();
        assert_eq!(total, COMPONENTS.len());
        assert!(!license_summary().is_empty());
    }

    #[test]
    fn short_hash_is_safe_on_odd_input() {
        assert_eq!(short_hash("0123456789abcdef"), "0123456789ab");
        assert_eq!(short_hash("abc"), "abc");
        assert_eq!(short_hash(""), "");
    }

    #[test]
    fn export_writes_the_document_verbatim() {
        let mut p = std::env::temp_dir();
        p.push(format!("supertile-sbom-{}.json", std::process::id()));
        export_to(&p).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), RAW_JSON);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn documentation_links_are_https() {
        for url in [CRA_DOC_URL, SBOM_URL, SECURITY_URL, CRA_REGULATION_URL] {
            assert!(url.starts_with("https://"), "{url} is not https");
        }
    }
}
