use std::ffi::{OsStr, OsString};
use std::iter::once;
use std::marker::PhantomPinned;
use std::mem::transmute;
use std::os::windows::ffi::{OsStringExt, OsStrExt};
use std::pin::{Pin, pin};
use std::ptr::{null, null_mut, slice_from_raw_parts_mut};

use anyhow::{anyhow, Result};
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::ClientToScreen;
use windows_sys::Win32::Security::*;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;
use windows_sys::Win32::System::SystemServices::*;
use windows_sys::Win32::System::LibraryLoader::*;
use windows_sys::Win32::System::Threading::*;
use crate::positioning::Rect;

pub fn encode_lpcstr(s: &str) -> Vec<u8> {
    let mut arr: Vec<u8> = s.bytes().map(|x| x as u8).collect();
    arr.push(0);
    arr
}

fn encode_wide_with_null(s: impl AsRef<str>) -> Vec<u16> {
    let wide: Vec<u16> = OsStr::new(s.as_ref())
        .encode_wide()
        .chain(once(0))
        .collect();
    wide
}

pub fn find_window_local(title: impl AsRef<str>) -> Result<HWND> {
    let title = encode_wide_with_null(title);
    let class = encode_wide_with_null("UnityWndClass");
    let result: HWND = unsafe { FindWindowW(class.as_ptr(), title.as_ptr()) };
    if result.is_null() {
        Err(anyhow!("找不到游戏窗口 / Cannot find window"))
    } else {
        Ok(result)
    }
}

pub fn find_window_cloud() -> Result<HWND> {
    let title = encode_wide_with_null(String::from("云·原神"));
    //let class = encode_wide(String::from("Qt5152QWindowIcon"));
    unsafe {
        let mut result: HWND = null_mut();
        for _ in 0..3 {
            result = FindWindowExW(null_mut(), result, null_mut(), title.as_ptr());
            let exstyle = GetWindowLongPtrW(result, GWL_EXSTYLE);
            let style = GetWindowLongPtrW(result, GWL_STYLE);
            if exstyle == 0x0 && style == 0x96080000 {
                return Ok(result); //全屏
            } else if exstyle == 0x100 && style == 0x96CE0000 {
                return Ok(result); //窗口
            }
        }
    }
    Err(anyhow!("找不到游戏窗口 / Cannot find window"))
}

unsafe fn get_client_rect_unsafe(hwnd: HWND) -> Result<Rect<i32>> {
    // Verify the window handle is still valid — the game may have closed
    // between window enumeration and this call.
    if IsWindow(hwnd) == 0 {
        return Err(anyhow!(
            "游戏窗口已关闭或无效，请确认游戏仍在运行 / \
             Game window is no longer valid. Please make sure the game is still running."
        ));
    }

    let mut rect: RECT = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    if GetClientRect(hwnd, &mut rect) == 0 {
        return Err(anyhow!(
            "无法获取游戏窗口大小（GetClientRect 失败） / \
             Cannot get game window size (GetClientRect failed)"
        ));
    }
    let width: i32 = rect.right;
    let height: i32 = rect.bottom;

    let mut point: POINT = POINT { x: 0, y: 0 };
    if ClientToScreen(hwnd, &mut point as *mut POINT) == 0 {
        return Err(anyhow!(
            "无法获取游戏窗口位置（ClientToScreen 失败） / \
             Cannot get game window position (ClientToScreen failed)"
        ));
    }
    let left: i32 = point.x;
    let top: i32 = point.y;

    Ok(Rect {
        left,
        top,
        width,
        height
    })
}

pub fn get_client_rect(hwnd: HWND) -> Result<Rect<i32>> {
    unsafe { get_client_rect_unsafe(hwnd) }
}

fn admin_check_os_error(context_zh: &str, context_en: &str) -> anyhow::Error {
    let os_error = std::io::Error::last_os_error();
    anyhow!("{}: {} / {}: {}", context_zh, os_error, context_en, os_error)
}

unsafe fn is_admin_unsafe() -> Result<bool> {
    let mut authority: SID_IDENTIFIER_AUTHORITY = SID_IDENTIFIER_AUTHORITY {
        Value: [0, 0, 0, 0, 0, 5],
    };
    let mut group: PSID = null_mut();
    let mut b = AllocateAndInitializeSid(
        &mut authority as *mut SID_IDENTIFIER_AUTHORITY,
        2,
        SECURITY_BUILTIN_DOMAIN_RID as u32,
        DOMAIN_ALIAS_RID_ADMINS as u32,
        0,
        0,
        0,
        0,
        0,
        0,
        &mut group as *mut PSID,
    );
    if b == 0 {
        return Err(admin_check_os_error(
            "创建管理员组标识失败",
            "AllocateAndInitializeSid failed",
        ));
    }

    let r = CheckTokenMembership(null_mut(), group, &mut b as *mut BOOL);
    let check_result = if r == 0 {
        Err(admin_check_os_error(
            "检查管理员组成员身份失败",
            "CheckTokenMembership failed",
        ))
    } else {
        Ok(b != 0)
    };
    FreeSid(group);
    check_result
}

unsafe fn process_token_is_elevated_unsafe() -> Result<bool> {
    let mut token: HANDLE = null_mut();
    if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token as *mut HANDLE) == 0 {
        return Err(admin_check_os_error(
            "打开进程令牌失败",
            "OpenProcessToken failed",
        ));
    }

    let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
    let mut returned_len: u32 = 0;
    let ok = GetTokenInformation(
        token,
        TokenElevation,
        &mut elevation as *mut TOKEN_ELEVATION as *mut _,
        std::mem::size_of::<TOKEN_ELEVATION>() as u32,
        &mut returned_len as *mut u32,
    );
    let get_result = if ok == 0 {
        Err(admin_check_os_error(
            "读取进程提权状态失败",
            "GetTokenInformation(TokenElevation) failed",
        ))
    } else {
        Ok(elevation.TokenIsElevated != 0)
    };
    CloseHandle(token);
    get_result
}

pub fn admin_status() -> Result<bool> {
    unsafe {
        let is_effective_admin = is_admin_unsafe()?;
        if !is_effective_admin {
            return Ok(false);
        }

        // CheckTokenMembership verifies the effective token has an enabled
        // Administrators SID. TokenElevation is an independent UAC sanity
        // check; any API failure becomes a hard denial.
        process_token_is_elevated_unsafe()
    }
}

pub fn is_admin() -> bool {
    admin_status().unwrap_or(false)
}

pub fn ensure_admin() -> Result<()> {
    match admin_status() {
        Ok(true) => Ok(()),
        Ok(false) => Err(anyhow!("需要管理员权限，请右键点击程序选择「以管理员身份运行」/ Administrator privileges required. Right-click the program and select 'Run as administrator'.")),
        Err(e) => Err(anyhow!("无法确认管理员权限，已阻止操作: {} / Cannot verify administrator privileges; action blocked: {}", e, e)),
    }
}

pub fn set_dpi_awareness() {
    let h_lib = unsafe {
        let utf16 = encode_lpcstr("Shcore.dll");
        LoadLibraryA(utf16.as_ptr())
    };
    if h_lib.is_null() {
        unsafe {
            SetProcessDPIAware();
        }
    } else {
        unsafe {
            let addr = GetProcAddress(h_lib, encode_lpcstr("SetProcessDpiAwareness").as_ptr());
            if addr.is_none() {
                log_warn!("找不到函数SetProcessDpiAwareness，但Shcore.dll存在", "cannot find process SetProcessDpiAwareness, but Shcore.dll exists");
                SetProcessDPIAware();
            } else {
                let proc = addr.unwrap();
                let func = transmute::<unsafe extern "system" fn() -> isize, unsafe extern "system" fn(usize) -> isize>(proc);
                func(2);
            }

            FreeLibrary(h_lib);
        }
    }
}

/// Returns available physical memory in bytes, or None if detection fails.
pub fn available_memory_bytes() -> Option<u64> {
    use std::mem;
    let mut status = unsafe { mem::zeroed::<windows_sys::Win32::System::SystemInformation::MEMORYSTATUSEX>() };
    status.dwLength = mem::size_of::<windows_sys::Win32::System::SystemInformation::MEMORYSTATUSEX>() as u32;
    let ret = unsafe { windows_sys::Win32::System::SystemInformation::GlobalMemoryStatusEx(&mut status) };
    if ret != 0 {
        Some(status.ullAvailPhys)
    } else {
        None
    }
}

pub fn show_window_and_set_foreground(hwnd: HWND) {
    unsafe {
        ShowWindow(hwnd, SW_RESTORE);
        SetForegroundWindow(hwnd);
    }
}

#[allow(static_mut_refs)]
unsafe fn iterate_window_unsafe() -> Vec<HWND> {
    static mut ALL_HANDLES: Vec<HWND> = Vec::new();

    extern "system" fn callback(hwnd: HWND, _vec_ptr: LPARAM) -> BOOL {
        unsafe {
            ALL_HANDLES.push(hwnd);
        }
        1
    }

    ALL_HANDLES.clear();
    EnumWindows(Some(callback), 0);

    ALL_HANDLES.clone()
}

pub fn iterate_window() -> Vec<HWND> {
    unsafe {
        iterate_window_unsafe()
    }
}

unsafe fn get_window_title_unsafe(hwnd: HWND) -> Option<String> {
    let mut buffer: Vec<u16> = vec![0; 100];
    GetWindowTextW(hwnd, buffer.as_mut_ptr(), 100);

    let s = OsString::from_wide(&buffer);

    if let Some(ss) = s.into_string().ok() {
        let ss = ss.trim_matches(char::from(0));
        Some(String::from(ss))
    } else {
        None
    }
}

pub fn get_window_title(hwnd: HWND) -> Option<String> {
    unsafe {
        get_window_title_unsafe(hwnd)
    }
}
