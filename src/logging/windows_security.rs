//! 使用 Win32 句柄为 LunaMate 诊断目录和文件设置并验证保护 DACL。
//!
//! DACL 只包含当前进程用户的 `GENERIC_ALL` ACE。目录 ACE 向子目录和文件继承，确保
//! flexi_logger 在轮转时新建的文件从创建起就不会继承 `logs` 根目录的宽权限。

use std::{
    ffi::c_void,
    fs::File,
    io,
    mem::{offset_of, size_of},
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::Path,
    ptr,
};

use windows::{
    Win32::{
        Foundation::{GENERIC_ALL, HANDLE, HLOCAL, LocalFree},
        Security::{
            ACCESS_ALLOWED_ACE, ACL, ACL_REVISION, ACL_SIZE_INFORMATION, AclSizeInformation,
            AddAccessAllowedAceEx,
            Authorization::{GetSecurityInfo, SE_FILE_OBJECT, SetSecurityInfo},
            CONTAINER_INHERIT_ACE, DACL_SECURITY_INFORMATION, EqualSid, GetAce, GetAclInformation,
            GetLengthSid, GetSecurityDescriptorControl, GetTokenInformation, InitializeAcl,
            IsValidAcl, IsValidSid, IsWellKnownSid, OBJECT_INHERIT_ACE, OWNER_SECURITY_INFORMATION,
            PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SE_DACL_PROTECTED,
            TOKEN_QUERY, TOKEN_USER, TokenUser, WinBuiltinAdministratorsSid, WinLocalSystemSid,
        },
        Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, CreateFileW, FILE_APPEND_DATA, FILE_ATTRIBUTE_DIRECTORY,
            FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS,
            FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ,
            FILE_SHARE_WRITE, FILE_WRITE_DATA, GetFileInformationByHandle, OPEN_ALWAYS,
            OPEN_EXISTING, READ_CONTROL, WRITE_DAC,
        },
        System::{
            SystemServices::ACCESS_ALLOWED_ACE_TYPE,
            Threading::{GetCurrentProcess, OpenProcessToken},
        },
    },
    core::PCWSTR,
};

struct UserSid {
    _storage: Vec<usize>,
    sid: PSID,
}

struct PrivateAcl {
    storage: Vec<u32>,
}

impl PrivateAcl {
    fn as_ptr(&self) -> *const ACL {
        self.storage.as_ptr().cast()
    }

    fn as_mut_ptr(&mut self) -> *mut ACL {
        self.storage.as_mut_ptr().cast()
    }
}

struct SecurityHandle {
    owned: OwnedHandle,
}

impl SecurityHandle {
    fn raw(&self) -> HANDLE {
        HANDLE(self.owned.as_raw_handle())
    }

    fn into_file(self) -> File {
        File::from(self.owned)
    }
}

struct LocalSecurityDescriptor(PSECURITY_DESCRIPTOR);

impl Drop for LocalSecurityDescriptor {
    fn drop(&mut self) {
        // SAFETY: 该地址只在 `GetSecurityInfo` 成功后保存，文档要求用 `LocalFree` 释放一次。
        let _ = unsafe { LocalFree(Some(HLOCAL(self.0.0))) };
    }
}

pub(super) fn protect_directory(path: &Path) -> io::Result<()> {
    let handle = open_security_handle(path, true, false)?;
    protect_handle(&handle, true)
}

pub(super) fn protect_file(path: &Path) -> io::Result<()> {
    let handle = open_security_handle(path, false, false)?;
    protect_handle(&handle, false)
}

pub(super) fn open_private_append_file(path: &Path) -> io::Result<File> {
    let handle = open_security_handle(path, false, true)?;
    protect_handle(&handle, false)?;
    Ok(handle.into_file())
}

fn open_security_handle(path: &Path, directory: bool, append: bool) -> io::Result<SecurityHandle> {
    let wide = wide_path(path)?;
    let access = READ_CONTROL.0
        | WRITE_DAC.0
        | FILE_READ_ATTRIBUTES.0
        | if append {
            FILE_APPEND_DATA.0 | FILE_WRITE_DATA.0
        } else {
            0
        };
    let share = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
    let creation = if append { OPEN_ALWAYS } else { OPEN_EXISTING };
    let flags = FILE_FLAG_OPEN_REPARSE_POINT
        | if directory {
            FILE_FLAG_BACKUP_SEMANTICS
        } else {
            FILE_ATTRIBUTE_NORMAL
        };
    // SAFETY: `wide` 以 NUL 结尾并在调用期间保持有效；返回句柄由 `OwnedHandle` 独占。
    let raw = unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            access,
            share,
            None,
            creation,
            flags,
            None,
        )
    }
    .map_err(io_error)?;
    // SAFETY: `CreateFileW` 成功时返回新的有效句柄，此处立即转交唯一所有权。
    let owned = unsafe { OwnedHandle::from_raw_handle(raw.0) };
    let handle = SecurityHandle { owned };

    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: `information` 是有效可写结构，句柄在调用期间保持打开。
    unsafe { GetFileInformationByHandle(handle.raw(), &mut information) }.map_err(io_error)?;
    let attributes = information.dwFileAttributes;
    let is_directory = attributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0;
    if attributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 || is_directory != directory {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "diagnostic handle type or reparse-point verification failed",
        ));
    }
    if !directory && information.nNumberOfLinks != 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "diagnostic file must have exactly one hard link",
        ));
    }
    Ok(handle)
}

fn protect_handle(handle: &SecurityHandle, directory: bool) -> io::Result<()> {
    let user = current_user_sid()?;
    verify_handle_owner(handle.raw(), user.sid)?;
    let acl = build_private_acl(user.sid, directory)?;
    // SAFETY: 句柄带有 `WRITE_DAC`；ACL 和 SID 在调用期间有效，其他安全描述符字段不修改。
    let status = unsafe {
        SetSecurityInfo(
            handle.raw(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            None,
            None,
            Some(acl.as_ptr()),
            None,
        )
    };
    check_win32(status)?;
    verify_handle_acl(handle.raw(), user.sid, directory)
}

fn verify_handle_owner(handle: HANDLE, sid: PSID) -> io::Result<()> {
    let mut owner = PSID::default();
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    // SAFETY: 输出槽位有效；成功时描述符由系统分配，随后交给 RAII 包装释放。
    let status = unsafe {
        GetSecurityInfo(
            handle,
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            Some(&mut owner),
            None,
            None,
            None,
            Some(&mut descriptor),
        )
    };
    check_win32(status)?;
    if descriptor.is_invalid() || owner.is_invalid() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "diagnostic owner query returned no SID",
        ));
    }
    let _descriptor = LocalSecurityDescriptor(descriptor);
    // SAFETY: SID 由当前令牌和仍存活的安全描述符提供；Win32 只读取这些有效缓冲区。
    let owner_is_allowed = unsafe {
        IsValidSid(owner).as_bool()
            && (EqualSid(owner, sid).is_ok()
                || IsWellKnownSid(owner, WinBuiltinAdministratorsSid).as_bool()
                || IsWellKnownSid(owner, WinLocalSystemSid).as_bool())
    };
    if !owner_is_allowed {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "diagnostic path has an untrusted owner",
        ));
    }
    Ok(())
}

fn current_user_sid() -> io::Result<UserSid> {
    let mut token = HANDLE::default();
    // SAFETY: 伪进程句柄由系统提供，输出地址指向已初始化的 `HANDLE` 槽位。
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }.map_err(io_error)?;
    // SAFETY: `OpenProcessToken` 成功后返回新的令牌句柄，此处立即转交唯一所有权。
    let token_handle = unsafe { OwnedHandle::from_raw_handle(token.0) };
    let token = HANDLE(token_handle.as_raw_handle());

    let mut required = 0_u32;
    // SAFETY: 零长度探测按 API 约定传空缓冲区，只写入 `required`。
    let probe = unsafe { GetTokenInformation(token, TokenUser, None, 0, &mut required) };
    if required < u32::try_from(size_of::<TOKEN_USER>()).unwrap_or(u32::MAX) {
        return Err(probe.map_or_else(io_error, |_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "current-user token returned an undersized SID buffer",
            )
        }));
    }
    let mut storage = aligned_words(required)?;
    let capacity = storage
        .len()
        .checked_mul(size_of::<usize>())
        .and_then(|bytes| u32::try_from(bytes).ok())
        .ok_or_else(|| io::Error::other("token SID buffer size overflow"))?;
    // SAFETY: 缓冲区按 `usize` 对齐且至少为 `capacity` 字节，API 只在该范围内写入。
    unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            Some(storage.as_mut_ptr().cast()),
            capacity,
            &mut required,
        )
    }
    .map_err(io_error)?;

    let base = storage.as_ptr() as usize;
    let end = base
        .checked_add(usize::try_from(required).map_err(|_| io::Error::other("SID size overflow"))?)
        .ok_or_else(|| io::Error::other("SID address overflow"))?;
    // SAFETY: 成功的 `GetTokenInformation(TokenUser)` 已在对齐缓冲区中写入 `TOKEN_USER`。
    let sid = unsafe { (*(storage.as_ptr().cast::<TOKEN_USER>())).User.Sid };
    let sid_address = sid.0 as usize;
    if sid.is_invalid() || sid_address < base || sid_address >= end {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "current-user SID points outside its token buffer",
        ));
    }
    // SAFETY: SID 来源于已成功返回且仍存活的 `TOKEN_USER` 缓冲区。
    let sid_length = unsafe {
        if !IsValidSid(sid).as_bool() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "current-user SID is invalid",
            ));
        }
        GetLengthSid(sid)
    };
    let sid_end = sid_address
        .checked_add(usize::try_from(sid_length).map_err(|_| io::Error::other("SID overflow"))?)
        .ok_or_else(|| io::Error::other("SID address overflow"))?;
    if sid_end > end {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "current-user SID exceeds its token buffer",
        ));
    }
    Ok(UserSid {
        _storage: storage,
        sid,
    })
}

fn build_private_acl(sid: PSID, directory: bool) -> io::Result<PrivateAcl> {
    // SAFETY: 调用方提供的 SID 在构造期间有效；这里只读取长度并由 Win32 复制到 ACL。
    let sid_length = unsafe {
        if sid.is_invalid() || !IsValidSid(sid).as_bool() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "SID is invalid"));
        }
        GetLengthSid(sid)
    };
    let acl_bytes = private_acl_size(sid_length)?;
    let word_count = usize::try_from(acl_bytes)
        .map_err(|_| io::Error::other("ACL size overflow"))?
        .div_ceil(size_of::<u32>());
    let mut acl = PrivateAcl {
        storage: vec![0_u32; word_count],
    };
    let flags = expected_ace_flags(directory);
    // SAFETY: `acl` 是按 `u32` 对齐的可写缓冲区，长度由 ACL、ACE 和已验证 SID 精确计算。
    unsafe {
        InitializeAcl(acl.as_mut_ptr(), acl_bytes, ACL_REVISION)?;
        AddAccessAllowedAceEx(acl.as_mut_ptr(), ACL_REVISION, flags, GENERIC_ALL.0, sid)?;
    }
    verify_acl(acl.as_ptr(), sid, directory)?;
    Ok(acl)
}

fn verify_handle_acl(handle: HANDLE, sid: PSID, directory: bool) -> io::Result<()> {
    let mut dacl = ptr::null_mut();
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    // SAFETY: 输出槽位有效；成功时描述符由系统分配，随后交给 RAII 包装释放。
    let status = unsafe {
        GetSecurityInfo(
            handle,
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            None,
            None,
            Some(&mut dacl),
            None,
            Some(&mut descriptor),
        )
    };
    check_win32(status)?;
    if descriptor.is_invalid() || dacl.is_null() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "protected DACL query returned no descriptor",
        ));
    }
    let descriptor = LocalSecurityDescriptor(descriptor);
    let mut control = 0_u16;
    let mut revision = 0_u32;
    // SAFETY: 描述符由成功的 `GetSecurityInfo` 返回，并由 `descriptor` 保持存活。
    unsafe { GetSecurityDescriptorControl(descriptor.0, &mut control, &mut revision) }
        .map_err(io_error)?;
    if control & SE_DACL_PROTECTED.0 == 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "diagnostic DACL is not protected",
        ));
    }
    verify_acl(dacl, sid, directory)
}

fn verify_acl(acl: *const ACL, sid: PSID, directory: bool) -> io::Result<()> {
    if acl.is_null() {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "DACL is null"));
    }
    let mut size_information = ACL_SIZE_INFORMATION::default();
    let mut ace_pointer: *mut c_void = ptr::null_mut();
    // SAFETY: `acl` 由 Win32 构造或安全描述符查询返回；API 先验证 ACL 再返回首个 ACE。
    unsafe {
        if !IsValidAcl(acl).as_bool() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "DACL is invalid",
            ));
        }
        GetAclInformation(
            acl,
            (&mut size_information as *mut ACL_SIZE_INFORMATION).cast(),
            u32::try_from(size_of::<ACL_SIZE_INFORMATION>()).unwrap_or(u32::MAX),
            AclSizeInformation,
        )?;
        if size_information.AceCount != 1 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "diagnostic DACL contains an unexpected ACE count",
            ));
        }
        GetAce(acl, 0, &mut ace_pointer)?;
        if ace_pointer.is_null() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "DACL ACE is null",
            ));
        }
        let ace = &*ace_pointer.cast::<ACCESS_ALLOWED_ACE>();
        let sid_length = GetLengthSid(sid);
        let expected_ace_size = u16::try_from(
            offset_of!(ACCESS_ALLOWED_ACE, SidStart)
                + usize::try_from(sid_length).map_err(|_| io::Error::other("SID size overflow"))?,
        )
        .map_err(|_| io::Error::other("ACE size overflow"))?;
        if u32::from(ace.Header.AceType) != ACCESS_ALLOWED_ACE_TYPE
            || ace.Header.AceFlags != expected_ace_flags(directory).0 as u8
            || ace.Header.AceSize != expected_ace_size
            || ace.Mask != GENERIC_ALL.0
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "diagnostic DACL ACE does not match the private policy",
            ));
        }
        let ace_sid = PSID(ptr::addr_of!(ace.SidStart).cast_mut().cast());
        EqualSid(ace_sid, sid).map_err(io_error)?;
    }
    Ok(())
}

fn private_acl_size(sid_length: u32) -> io::Result<u32> {
    let bytes = size_of::<ACL>()
        .checked_add(offset_of!(ACCESS_ALLOWED_ACE, SidStart))
        .and_then(|base| base.checked_add(usize::try_from(sid_length).ok()?))
        .ok_or_else(|| io::Error::other("ACL size overflow"))?;
    u32::try_from(bytes).map_err(|_| io::Error::other("ACL size overflow"))
}

fn expected_ace_flags(directory: bool) -> windows::Win32::Security::ACE_FLAGS {
    if directory {
        OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE
    } else {
        windows::Win32::Security::ACE_FLAGS(0)
    }
}

fn aligned_words(bytes: u32) -> io::Result<Vec<usize>> {
    let bytes = usize::try_from(bytes).map_err(|_| io::Error::other("buffer size overflow"))?;
    let words = bytes.div_ceil(size_of::<usize>());
    if words == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "buffer size must not be zero",
        ));
    }
    Ok(vec![0_usize; words])
}

fn wide_path(path: &Path) -> io::Result<Vec<u16>> {
    let mut wide = path.as_os_str().encode_wide().collect::<Vec<_>>();
    if wide.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "diagnostic path contains an interior NUL",
        ));
    }
    wide.push(0);
    Ok(wide)
}

fn check_win32(status: windows::Win32::Foundation::WIN32_ERROR) -> io::Result<()> {
    if status.0 == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(
            i32::try_from(status.0).unwrap_or(i32::MAX),
        ))
    }
}

fn io_error(error: windows::core::Error) -> io::Error {
    io::Error::other(error.to_string())
}

#[cfg(test)]
pub(super) fn construct_private_acl_for_test(sid: PSID, directory: bool) -> io::Result<()> {
    let acl = build_private_acl(sid, directory)?;
    verify_acl(acl.as_ptr(), sid, directory)
}
