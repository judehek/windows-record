use std::sync::atomic::{AtomicIsize, Ordering};
use windows::Win32::Foundation::{BOOL, HWND, LPARAM};
use windows::Win32::UI::WindowsAndMessaging::GetWindowTextW;

struct SearchContext {
    substring: String,
    result: AtomicIsize,
}

pub fn find_window_by_substring(substring: &str) -> Option<HWND> {
    let context = SearchContext {
        substring: substring.to_lowercase(),
        result: AtomicIsize::new(0),
    };

    unsafe {
        windows::Win32::UI::WindowsAndMessaging::EnumWindows(
            Some(enum_window_proc),
            LPARAM(&context as *const _ as isize),
        );
    }

    let hwnd_value = context.result.load(Ordering::Relaxed);
    if hwnd_value == 0 {
        None
    } else {
        Some(HWND(hwnd_value))
    }
}

unsafe extern "system" fn enum_window_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let context = &*(lparam.0 as *const SearchContext);
    let mut text: [u16; 512] = [0; 512];
    let length = GetWindowTextW(hwnd, &mut text);
    let window_text = String::from_utf16_lossy(&text[..length as usize]).to_lowercase();

    if window_text.contains(&context.substring) {
        context.result.store(hwnd.0, Ordering::Relaxed);
        BOOL(0) // Stop enumeration
    } else {
        BOOL(1) // Continue enumeration
    }
}
