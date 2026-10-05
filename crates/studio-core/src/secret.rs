//! Secrets by reference.
//!
//! Config holds references (`op://vault/item/field`, `env:NAME`); a value is
//! read at the moment of use, kept in memory, and never printed. [`Secret`]'s
//! `Debug` and `Display` are redacted so it can't leak through logs or errors.

use std::fmt;
use std::process::Command;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Clone, PartialEq, Eq)]
pub enum SecretRef {
    /// 1Password secret reference, read with `op read`.
    OnePassword(String),
    /// An environment variable (CI, tests, headless use).
    Env(String),
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SecretError {
    #[error("not a secret reference (want op://… or env:NAME): {0}")]
    BadRef(String),
    #[error("1Password is locked or timed out (`op` said: authorization timeout); unlock the app and retry")]
    Locked,
    #[error("`op` is not installed or not on PATH")]
    NoOp,
    #[error("`op read {reference}` failed: {message}")]
    Op { reference: String, message: String },
    #[error("environment variable {0} is not set")]
    MissingEnv(String),
}

impl SecretRef {
    pub fn parse(s: &str) -> Result<Self, SecretError> {
        if s.starts_with("op://") && s.len() > 5 {
            Ok(Self::OnePassword(s.to_string()))
        } else if let Some(name) = s.strip_prefix("env:").filter(|n| !n.is_empty()) {
            Ok(Self::Env(name.to_string()))
        } else {
            Err(SecretError::BadRef(s.to_string()))
        }
    }

    /// Read the value. Blocks on `op` for 1Password references.
    pub fn resolve(&self) -> Result<Secret, SecretError> {
        match self {
            Self::Env(name) => std::env::var(name)
                .map(Secret)
                .map_err(|_| SecretError::MissingEnv(name.clone())),
            Self::OnePassword(reference) => {
                let out = Command::new("op")
                    .args(["read", "--no-newline", reference])
                    .output()
                    .map_err(|_| SecretError::NoOp)?;
                if out.status.success() {
                    return Ok(Secret(String::from_utf8_lossy(&out.stdout).into_owned()));
                }
                let message = String::from_utf8_lossy(&out.stderr).trim().to_string();
                if message.contains("authorization timeout") {
                    Err(SecretError::Locked)
                } else {
                    Err(SecretError::Op {
                        reference: reference.clone(),
                        message,
                    })
                }
            }
        }
    }
}

impl fmt::Display for SecretRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OnePassword(r) => f.write_str(r),
            Self::Env(n) => write!(f, "env:{n}"),
        }
    }
}

impl fmt::Debug for SecretRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SecretRef({self})")
    }
}

impl Serialize for SecretRef {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for SecretRef {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::parse(&s).map_err(serde::de::Error::custom)
    }
}

/// A resolved secret value. Never printed.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
    /// The value, for putting into a request. Don't format it anywhere else.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_both_schemes() {
        assert_eq!(
            SecretRef::parse("op://V/item/field").unwrap(),
            SecretRef::OnePassword("op://V/item/field".into())
        );
        assert_eq!(SecretRef::parse("env:TOKEN").unwrap(), SecretRef::Env("TOKEN".into()));
        assert!(SecretRef::parse("plaintext-token").is_err());
        assert!(SecretRef::parse("env:").is_err());
        assert!(SecretRef::parse("op://").is_err());
    }

    #[test]
    fn a_literal_value_in_config_is_refused() {
        #[derive(serde::Deserialize, Debug)]
        struct T {
            #[allow(dead_code)]
            t: SecretRef,
        }
        let e = toml::from_str::<T>("t = \"ghp_abc123\"").unwrap_err();
        assert!(e.to_string().contains("not a secret reference"));
    }

    #[test]
    fn secrets_never_format() {
        let s = Secret::new("hunter2");
        assert_eq!(format!("{s} {s:?}"), "<redacted> Secret(<redacted>)");
        assert_eq!(s.expose(), "hunter2");
    }

    #[test]
    fn env_refs_resolve() {
        // SAFETY-free: a unique name no other test touches.
        let name = "STUDIO_CORE_TEST_SECRET_ENV";
        assert_eq!(
            SecretRef::Env(name.into()).resolve().unwrap_err(),
            SecretError::MissingEnv(name.into())
        );
    }
}
