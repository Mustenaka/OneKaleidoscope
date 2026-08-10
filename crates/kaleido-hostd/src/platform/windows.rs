use kaleido_proto::host::HostPlatform;
use std::path::{Path, PathBuf};

pub(super) fn host_platform() -> HostPlatform {
    HostPlatform::Windows
}

pub(super) fn provider_path(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        path.to_path_buf()
    }
}
