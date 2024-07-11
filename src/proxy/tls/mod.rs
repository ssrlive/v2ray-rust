#[cfg(target_os = "macos")]
mod macos;
mod tls_stream;
#[cfg(all(unix, not(target_os = "macos")))]
mod unix;
#[cfg(windows)]
mod windows;
// todo: provide Mozilla's root certs as optional

#[cfg(feature = "enable_useless")]
pub use tls_stream::TlsStreamBuilder;
