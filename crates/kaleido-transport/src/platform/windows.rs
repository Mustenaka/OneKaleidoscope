#![allow(unsafe_code)]

use std::ffi::c_void;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::mem::size_of;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, LocalFree, ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS, GENERIC_ALL,
    HANDLE,
};
use windows_sys::Win32::Security::Authorization::{
    GetNamedSecurityInfoW, SetEntriesInAclW, SetNamedSecurityInfoW, EXPLICIT_ACCESS_W,
    GRANT_ACCESS, NO_MULTIPLE_TRUSTEE, SE_FILE_OBJECT, TRUSTEE_IS_SID, TRUSTEE_IS_USER,
    TRUSTEE_IS_WELL_KNOWN_GROUP, TRUSTEE_W,
};
use windows_sys::Win32::Security::{
    AclSizeInformation, CreateWellKnownSid, EqualSid, GetAce, GetAclInformation,
    GetSecurityDescriptorControl, GetTokenInformation, IsValidSid, TokenUser, WinLocalSystemSid,
    ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, ACL_SIZE_INFORMATION, DACL_SECURITY_INFORMATION,
    NO_INHERITANCE, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
    SECURITY_MAX_SID_SIZE, SE_DACL_PROTECTED, TOKEN_QUERY, TOKEN_USER,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
const FILE_ALL_ACCESS: u32 = 0x001f_01ff;

pub(crate) fn prepare_private_directory(path: &Path) -> io::Result<()> {
    if path.exists() {
        return verify_private_acl(path);
    }
    fs::create_dir_all(path)?;
    apply_private_acl(path)?;
    verify_private_acl(path)
}

pub(crate) fn secure_private_file(path: &Path, create_new: bool) -> io::Result<File> {
    let existed = path.exists();
    if existed {
        verify_private_acl(path)?;
    }
    let mut options = OpenOptions::new();
    options.write(true);
    if create_new {
        options.create_new(true);
    } else {
        options.create(true).truncate(true);
    }
    let file = options.open(path)?;
    if !existed {
        apply_private_acl(path)?;
    } else {
        verify_private_acl(path)?;
    }
    Ok(file)
}

pub(crate) fn verify_private_path(path: &Path) -> io::Result<()> {
    verify_private_acl(path)
}

pub(crate) fn atomic_replace(source: &Path, target: &Path) -> io::Result<()> {
    let source_wide = wide_path(source);
    let target_wide = wide_path(target);
    // SAFETY: both paths are NUL-terminated and remain live throughout the
    // call. REPLACE_EXISTING and WRITE_THROUGH provide the required atomic,
    // durable replacement semantics on the same volume.
    let succeeded = unsafe {
        windows_sys::Win32::Storage::FileSystem::MoveFileExW(
            source_wide.as_ptr(),
            target_wide.as_ptr(),
            windows_sys::Win32::Storage::FileSystem::MOVEFILE_REPLACE_EXISTING
                | windows_sys::Win32::Storage::FileSystem::MOVEFILE_WRITE_THROUGH,
        )
    };
    if succeeded == 0 {
        Err(io::Error::last_os_error())
    } else {
        verify_private_acl(target)
    }
}

fn apply_private_acl(path: &Path) -> io::Result<()> {
    let user = CurrentUserSid::load()?;
    let system = system_sid()?;
    let mut entries = [
        explicit_access(user.as_psid(), TRUSTEE_IS_USER),
        explicit_access(
            system.as_ptr().cast_mut().cast(),
            TRUSTEE_IS_WELL_KNOWN_GROUP,
        ),
    ];
    let mut acl: *mut ACL = null_mut();
    // SAFETY: both trustee SID pointers remain valid for the duration of this
    // call, the two-entry array is initialized, and `acl` is an out-pointer
    // released with LocalFree below.
    let status =
        unsafe { SetEntriesInAclW(entries.len() as u32, entries.as_mut_ptr(), null(), &mut acl) };
    if status != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    let wide_path = wide_path(path);
    // SAFETY: `wide_path` is NUL-terminated, `acl` was returned by
    // SetEntriesInAclW, and owner/group/SACL are intentionally unchanged.
    let set_status = unsafe {
        SetNamedSecurityInfoW(
            wide_path.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            acl,
            null(),
        )
    };
    // SAFETY: `acl` is the LocalAlloc-owned pointer returned by
    // SetEntriesInAclW and is no longer used after this call.
    let _ = unsafe { LocalFree(acl.cast()) };
    if set_status == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(set_status as i32))
    }
}

fn verify_private_acl(path: &Path) -> io::Result<()> {
    let user = CurrentUserSid::load()?;
    let wide_path = wide_path(path);
    let mut acl: *mut ACL = null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
    // SAFETY: the path is NUL-terminated and all optional output pointers are
    // null except for the DACL and descriptor outputs owned by this function.
    let status = unsafe {
        GetNamedSecurityInfoW(
            wide_path.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            &mut acl,
            null_mut(),
            &mut descriptor,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    let result = verify_acl_descriptor(descriptor, acl, user.as_psid());
    // SAFETY: `descriptor` is the LocalAlloc-owned pointer returned by
    // GetNamedSecurityInfoW. The nested ACL pointer is not freed separately.
    let _ = unsafe { LocalFree(descriptor.cast()) };
    result
}

fn verify_acl_descriptor(
    descriptor: PSECURITY_DESCRIPTOR,
    acl: *mut ACL,
    user_sid: PSID,
) -> io::Result<()> {
    if descriptor.is_null() || acl.is_null() {
        return insecure_acl();
    }
    let mut control = 0_u16;
    let mut revision = 0_u32;
    // SAFETY: `descriptor` is a live descriptor returned by Windows and both
    // scalar out-pointers refer to initialized writable storage.
    if unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) } == 0
        || control & SE_DACL_PROTECTED == 0
    {
        return insecure_acl();
    }
    let mut info = ACL_SIZE_INFORMATION::default();
    // SAFETY: `acl` is nested in the live descriptor and `info` has exactly the
    // size required for AclSizeInformation.
    if unsafe {
        GetAclInformation(
            acl,
            (&mut info as *mut ACL_SIZE_INFORMATION).cast(),
            size_of::<ACL_SIZE_INFORMATION>() as u32,
            AclSizeInformation,
        )
    } == 0
        || info.AceCount != 2
    {
        return insecure_acl();
    }

    let mut saw_user = false;
    let mut saw_system = false;
    for index in 0..info.AceCount {
        let mut raw_ace: *mut c_void = null_mut();
        // SAFETY: `index` is below the ACE count reported for this live ACL and
        // `raw_ace` is a valid out-pointer.
        if unsafe { GetAce(acl, index, &mut raw_ace) } == 0 || raw_ace.is_null() {
            return insecure_acl();
        }
        // SAFETY: GetAce returned a live ACE pointer and every ACE begins with
        // an ACE_HEADER. No fields beyond that header are read here.
        let header = unsafe { &*(raw_ace.cast::<ACE_HEADER>()) };
        if header.AceType != ACCESS_ALLOWED_ACE_TYPE
            || header.AceFlags != 0
            || usize::from(header.AceSize) < size_of::<ACCESS_ALLOWED_ACE>()
        {
            return insecure_acl();
        }
        // SAFETY: the checked type and size establish that this ACE contains
        // the ACCESS_ALLOWED_ACE fixed fields through SidStart.
        let ace = unsafe { &*(raw_ace.cast::<ACCESS_ALLOWED_ACE>()) };
        if ace.Mask != GENERIC_ALL && ace.Mask & FILE_ALL_ACCESS != FILE_ALL_ACCESS {
            return insecure_acl();
        }
        let ace_sid = (&ace.SidStart as *const u32).cast_mut().cast();
        // SAFETY: `ace_sid` points into the size-checked live ACE. Windows
        // validates the variable-length SID representation without retaining it.
        if unsafe { IsValidSid(ace_sid) } == 0 {
            return insecure_acl();
        }
        // SAFETY: both pointers are valid SIDs: one embedded in the ACE and one
        // obtained from the current process token.
        if unsafe { EqualSid(ace_sid, user_sid) } != 0 {
            saw_user = true;
        // SAFETY: the SID embedded in the validated ACE can be passed to the
        // Windows well-known SID predicate.
        } else if unsafe {
            windows_sys::Win32::Security::IsWellKnownSid(ace_sid, WinLocalSystemSid)
        } != 0
        {
            saw_system = true;
        } else {
            return insecure_acl();
        }
    }
    if saw_user && saw_system {
        Ok(())
    } else {
        insecure_acl()
    }
}

fn explicit_access(sid: PSID, trustee_type: i32) -> EXPLICIT_ACCESS_W {
    EXPLICIT_ACCESS_W {
        grfAccessPermissions: GENERIC_ALL,
        grfAccessMode: GRANT_ACCESS,
        grfInheritance: NO_INHERITANCE,
        Trustee: TRUSTEE_W {
            pMultipleTrustee: null_mut(),
            MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: trustee_type,
            ptstrName: sid.cast(),
        },
    }
}

fn system_sid() -> io::Result<Vec<u8>> {
    let mut sid = vec![0_u8; SECURITY_MAX_SID_SIZE as usize];
    let mut len = SECURITY_MAX_SID_SIZE;
    // SAFETY: `sid` provides SECURITY_MAX_SID_SIZE writable bytes and `len`
    // accurately describes that capacity; a domain SID is not required.
    if unsafe {
        CreateWellKnownSid(
            WinLocalSystemSid,
            null_mut(),
            sid.as_mut_ptr().cast(),
            &mut len,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    sid.truncate(len as usize);
    Ok(sid)
}

struct CurrentUserSid {
    token: HANDLE,
    storage: Vec<usize>,
}

impl CurrentUserSid {
    fn load() -> io::Result<Self> {
        let mut token = null_mut();
        // SAFETY: GetCurrentProcess returns a pseudo-handle valid for
        // OpenProcessToken and `token` is a valid out-pointer.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut bytes = 0_u32;
        // SAFETY: a null buffer with length zero is the documented size-query
        // form; `bytes` is a valid out-pointer.
        let first = unsafe { GetTokenInformation(token, TokenUser, null_mut(), 0, &mut bytes) };
        // SAFETY: GetLastError is read immediately after the failed size query.
        let expected_size_failure =
            first == 0 && unsafe { GetLastError() } == ERROR_INSUFFICIENT_BUFFER;
        if !expected_size_failure || bytes == 0 {
            // SAFETY: `token` was opened successfully and is not used again.
            let _ = unsafe { CloseHandle(token) };
            return Err(io::Error::last_os_error());
        }
        let word = size_of::<usize>();
        let words = (bytes as usize).div_ceil(word);
        let mut storage = vec![0_usize; words];
        // SAFETY: `storage` is aligned and has at least `bytes` writable bytes;
        // `token` remains open throughout the call.
        if unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                storage.as_mut_ptr().cast(),
                bytes,
                &mut bytes,
            )
        } == 0
        {
            let error = io::Error::last_os_error();
            // SAFETY: `token` was opened successfully and is not used again.
            let _ = unsafe { CloseHandle(token) };
            return Err(error);
        }
        Ok(Self { token, storage })
    }

    fn as_psid(&self) -> PSID {
        // SAFETY: `storage` contains the TOKEN_USER structure written by
        // GetTokenInformation and remains alive for the returned SID pointer.
        unsafe { (*(self.storage.as_ptr().cast::<TOKEN_USER>())).User.Sid }
    }
}

impl Drop for CurrentUserSid {
    fn drop(&mut self) {
        // SAFETY: `token` is the unique live handle owned by this value.
        let _ = unsafe { CloseHandle(self.token) };
    }
}

fn wide_path(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

fn insecure_acl<T>() -> io::Result<T> {
    Err(io::Error::new(
        io::ErrorKind::PermissionDenied,
        "private file ACL is not restricted to the current user and SYSTEM",
    ))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::fs;

    use super::{apply_private_acl, prepare_private_directory, verify_private_acl};

    #[test]
    fn applied_acl_allows_only_current_user_and_system_without_inheritance() {
        let path = std::env::temp_dir().join(format!(
            "kaleido-transport-windows-acl-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir(&path).expect("test directory");
        apply_private_acl(&path).expect("apply ACL");
        verify_private_acl(&path).expect("verify ACL");
        fs::remove_dir(&path).expect("cleanup");
    }

    #[test]
    fn broad_inherited_acl_is_rejected_fail_loud() {
        let path = std::env::temp_dir().join(format!(
            "kaleido-transport-windows-broad-acl-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir(&path).expect("test directory");
        assert!(prepare_private_directory(&path).is_err());
        fs::remove_dir(&path).expect("cleanup");
    }
}
