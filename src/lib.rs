//! Local model gateway library.

pub mod config;
pub mod secrets;

/// Returns the package version for the binary and tests.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::VERSION;

    #[test]
    fn exposes_package_version() {
        assert_eq!(VERSION, "0.1.0");
    }
}
