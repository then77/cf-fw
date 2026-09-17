use rand::Rng;

use crate::error::{FwError, Result};

pub const MAX_SLUG_LEN: usize = 63;
pub const GENERATED_SLUG_RANDOM_ATTEMPTS: usize = 32;

pub const ADJECTIVES: &[&str] = &[
    "amber", "apple", "azure", "bold", "bright", "brisk", "calm", "clear", "cool", "cozy", "crisp",
    "dawn", "deep", "eager", "fair", "fast", "fresh", "gentle", "golden", "green", "happy", "kind",
    "light", "lively", "lucky", "mellow", "merry", "misty", "neat", "noble", "orange", "peaceful",
    "pink", "plain", "proud", "pure", "quick", "quiet", "rapid", "red", "rosy", "safe", "sharp",
    "silent", "silver", "small", "soft", "solar", "still", "sunny", "sweet", "swift", "tiny",
    "true", "vivid", "warm", "white", "wild", "wise", "young",
];

pub const NOUNS: &[&str] = &[
    "badger", "bay", "bear", "birch", "brook", "cedar", "cloud", "comet", "coral", "cove", "crane",
    "dawn", "deer", "dove", "eagle", "falcon", "fern", "field", "finch", "forest", "fox", "grove",
    "harbor", "hawk", "hill", "island", "lake", "leaf", "lynx", "maple", "meadow", "moon", "oak",
    "ocean", "orbit", "otter", "owl", "panda", "peak", "pebble", "pen", "pine", "pond", "rain",
    "reef", "river", "robin", "sparrow", "star", "stone", "storm", "sun", "tiger", "trail",
    "valley", "wave", "willow", "wolf", "wood", "wren",
];

/// Normalize a user-provided slug to lowercase and validate it as a DNS label.
pub fn normalize_slug(input: &str) -> Result<String> {
    let normalized = input.to_ascii_lowercase();
    validate_normalized_slug(&normalized)
        .map_err(|reason| FwError::InvalidSlug(input.to_owned(), reason))?;
    Ok(normalized)
}

/// Validate an already-normalized slug, returning a concise reason on failure.
pub fn validate_normalized_slug(slug: &str) -> std::result::Result<(), String> {
    if slug.is_empty() || slug.len() > MAX_SLUG_LEN {
        return Err(format!("must be between 1 and {MAX_SLUG_LEN} characters"));
    }
    if !slug.is_ascii() {
        return Err("must contain only lowercase ASCII letters, digits, and hyphens".into());
    }
    if slug.starts_with('-') || slug.ends_with('-') {
        return Err("must not start or end with a hyphen".into());
    }
    if !slug
        .bytes()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err("must contain only lowercase ASCII letters, digits, and hyphens".into());
    }
    if slug.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("must not be all numeric".into());
    }
    Ok(())
}

/// Generate one adjective-noun candidate from the embedded word lists.
pub fn generate_slug<R: Rng + ?Sized>(rng: &mut R) -> String {
    let adjective = ADJECTIVES[rng.random_range(0..ADJECTIVES.len())];
    let noun = NOUNS[rng.random_range(0..NOUNS.len())];
    format!("{adjective}-{noun}")
}

/// Find an unused generated slug.
///
/// Random choices keep normal allocation varied. A deterministic exhaustive fallback guarantees
/// progress whenever any embedded adjective-noun combination remains available.
pub fn generate_unique_slug<R, F>(rng: &mut R, mut is_taken: F) -> Result<String>
where
    R: Rng + ?Sized,
    F: FnMut(&str) -> bool,
{
    for _ in 0..GENERATED_SLUG_RANDOM_ATTEMPTS {
        let candidate = generate_slug(rng);
        if !is_taken(&candidate) {
            return Ok(candidate);
        }
    }

    for adjective in ADJECTIVES {
        for noun in NOUNS {
            let candidate = format!("{adjective}-{noun}");
            if !is_taken(&candidate) {
                return Ok(candidate);
            }
        }
    }

    Err(FwError::Other(
        "all generated slug combinations are already active".into(),
    ))
}

#[cfg(test)]
mod tests {
    use rand::{SeedableRng, rngs::StdRng};

    use super::*;

    #[test]
    fn normalizes_uppercase_before_validation() {
        assert_eq!(normalize_slug("Green-Apple").unwrap(), "green-apple");
    }

    #[test]
    fn accepts_dns_label_characters() {
        assert_eq!(normalize_slug("a").unwrap(), "a");
        assert_eq!(normalize_slug("route-42").unwrap(), "route-42");
        assert_eq!(normalize_slug(&"a".repeat(63)).unwrap().len(), 63);
    }

    #[test]
    fn rejects_invalid_custom_slugs() {
        for slug in [
            "",
            "-apple",
            "apple-",
            "apple_pen",
            "apple.pen",
            "a/b",
            "two words",
            "é",
        ] {
            assert!(normalize_slug(slug).is_err(), "accepted {slug:?}");
        }
        assert!(normalize_slug(&"a".repeat(64)).is_err());
    }

    #[test]
    fn rejects_numeric_slugs() {
        let error = normalize_slug("8080").unwrap_err();
        assert!(error.to_string().contains("must not be all numeric"));
    }

    #[test]
    fn generated_slug_is_valid() {
        let mut rng = StdRng::seed_from_u64(7);
        let slug = generate_slug(&mut rng);
        assert!(validate_normalized_slug(&slug).is_ok());
        assert_eq!(slug.matches('-').count(), 1);
    }

    #[test]
    fn retries_generated_slug_collisions() {
        let mut preview_rng = StdRng::seed_from_u64(11);
        let collision = generate_slug(&mut preview_rng);
        let mut rng = StdRng::seed_from_u64(11);

        let generated = generate_unique_slug(&mut rng, |candidate| candidate == collision).unwrap();

        assert_ne!(generated, collision);
        assert!(validate_normalized_slug(&generated).is_ok());
    }

    #[test]
    fn reports_exhausted_generated_space() {
        let mut rng = StdRng::seed_from_u64(1);
        assert!(generate_unique_slug(&mut rng, |_| true).is_err());
    }
}
