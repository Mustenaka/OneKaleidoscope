#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
mod unsupported;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
pub(crate) use linux::{
    atomic_replace, prepare_private_directory, secure_private_file, verify_private_path,
};
#[cfg(target_os = "macos")]
pub(crate) use macos::{
    atomic_replace, prepare_private_directory, secure_private_file, verify_private_path,
};
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub(crate) use unsupported::{
    atomic_replace, prepare_private_directory, secure_private_file, verify_private_path,
};
#[cfg(windows)]
pub(crate) use windows::{
    atomic_replace, prepare_private_directory, secure_private_file, verify_private_path,
};
