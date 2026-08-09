//! Fail-closed host persistence stubs for mobile client targets.
//!
//! Android and iOS compile the framing, pinning and client authentication
//! portions of `kaleido-transport`. They must never accidentally instantiate a
//! host TLS identity or device registry with ordinary app-sandbox file APIs.

use std::fs::File;
use std::io;
use std::path::Path;

fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "host security persistence is unavailable on this target",
    )
}

pub(crate) fn prepare_private_directory(_path: &Path) -> io::Result<()> {
    Err(unsupported())
}

pub(crate) fn secure_private_file(_path: &Path, _create_new: bool) -> io::Result<File> {
    Err(unsupported())
}

pub(crate) fn verify_private_path(_path: &Path) -> io::Result<()> {
    Err(unsupported())
}

pub(crate) fn atomic_replace(_source: &Path, _target: &Path) -> io::Result<()> {
    Err(unsupported())
}
