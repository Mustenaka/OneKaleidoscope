use kaleido_proto::host::HostPlatform;
use std::path::{Path, PathBuf};

pub(super) fn host_platform() -> HostPlatform {
    HostPlatform::MacOs
}

pub(super) fn provider_path(path: &Path) -> PathBuf {
    path.to_path_buf()
}
