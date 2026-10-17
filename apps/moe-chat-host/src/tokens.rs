//! Process-scoped random material: browser session, CSRF token and process generation.
//!
//! Every value comes from the operating system random source. Nothing in this module
//! formats a token for diagnostics; [`Tokens`] deliberately redacts both secrets in its
//! `Debug` output so an accidental `{:?}` cannot write them to process output.

use std::fmt;
use std::io;
use std::mem::size_of;

use getrandom::fill;

/// Cookie name carrying the browser session token.
pub(crate) const COOKIE_NAME: &str = "moe_chat_session";

const SESSION_BYTES: usize = 32;
const CSRF_BYTES: usize = 32;
const HEX: &[u8; 16] = b"0123456789abcdef";

/// Session and CSRF secrets plus the process generation of one host process.
#[derive(Clone)]
pub(crate) struct Tokens {
    session: String,
    csrf: String,
    process_generation: u64,
}

impl Tokens {
    /// Draws a fresh session token, CSRF token and non-zero process generation.
    pub(crate) fn generate() -> io::Result<Self> {
        Ok(Self {
            session: random_hex(SESSION_BYTES)?,
            csrf: random_hex(CSRF_BYTES)?,
            process_generation: random_u64()? | 1,
        })
    }

    pub(crate) fn session(&self) -> &str {
        &self.session
    }

    pub(crate) fn csrf(&self) -> &str {
        &self.csrf
    }

    pub(crate) fn process_generation(&self) -> u64 {
        self.process_generation
    }
}

impl fmt::Debug for Tokens {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Tokens")
            .field("session", &"<redacted>")
            .field("csrf", &"<redacted>")
            .field("process_generation", &self.process_generation)
            .finish()
    }
}

/// Compares two secrets without an early exit, so a wrong value leaks no matching prefix.
pub(crate) fn constant_time_eq(expected: &str, presented: &str) -> bool {
    let (expected, presented) = (expected.as_bytes(), presented.as_bytes());
    if expected.len() != presented.len() {
        return false;
    }
    let mut difference = 0_u8;
    for (left, right) in expected.iter().zip(presented) {
        difference |= left ^ right;
    }
    difference == 0
}

fn random_hex(len: usize) -> io::Result<String> {
    let mut bytes = vec![0_u8; len];
    fill(&mut bytes).map_err(random_failure)?;
    let mut encoded = String::with_capacity(len * 2);
    for byte in bytes {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    Ok(encoded)
}

/// Draws the process generation from the same operating system random source as the tokens.
fn random_u64() -> io::Result<u64> {
    let mut bytes = [0_u8; size_of::<u64>()];
    fill(&mut bytes).map_err(random_failure)?;
    Ok(u64::from_le_bytes(bytes))
}

/// Replaces the random-source failure with a fixed message: the host never surfaces or
/// records the underlying error text.
fn random_failure(_: getrandom::Error) -> io::Error {
    io::Error::other("operating system random source unavailable")
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn generation_is_never_zero_and_secrets_are_distinct() {
        let first = Tokens::generate().expect("random source");
        let second = Tokens::generate().expect("random source");
        assert_ne!(first.process_generation(), 0);
        assert_ne!(second.process_generation(), 0);
        assert_ne!(first.session(), second.session());
        assert_ne!(first.csrf(), second.csrf());
        assert_ne!(first.session(), first.csrf());
        assert_eq!(first.session().len(), SESSION_BYTES * 2);
        assert_eq!(first.csrf().len(), CSRF_BYTES * 2);
        let alphabet: HashSet<char> = "0123456789abcdef".chars().collect();
        assert!(first.session().chars().all(|c| alphabet.contains(&c)));
    }

    #[test]
    fn debug_output_never_contains_a_secret() {
        let tokens = Tokens::generate().expect("random source");
        let rendered = format!("{tokens:?}");
        assert!(!rendered.contains(tokens.session()));
        assert!(!rendered.contains(tokens.csrf()));
        assert!(rendered.contains("redacted"));
    }

    #[test]
    fn constant_time_comparison_rejects_prefixes_and_length_changes() {
        assert!(constant_time_eq("abc123", "abc123"));
        assert!(!constant_time_eq("abc123", "abc124"));
        assert!(!constant_time_eq("abc123", "abc12"));
        assert!(!constant_time_eq("", "a"));
        assert!(constant_time_eq("", ""));
    }
}
