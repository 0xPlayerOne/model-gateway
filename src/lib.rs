//! Local model gateway library.

pub mod benchmarks;
pub mod config;
pub mod gateway;
pub mod providers;
pub mod routing;
pub mod secrets;
mod storage;

/// Returns the package version for the binary and tests.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::VERSION;

    #[test]
    fn exposes_package_version() {
        assert_eq!(VERSION, "0.5.1");
    }
}
