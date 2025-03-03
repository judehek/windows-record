use std::sync::atomic::{AtomicIsize, Ordering};
use windows::Win32::Foundation::{BOOL, HWND, LPARAM};
use windows::Win32::UI::WindowsAndMessaging::{GetWindowTextW, IsWindow, IsWindowVisible};
use log::{debug, trace};

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
        debug!("No window found with substring: {}", substring);
        None
    } else {
        debug!("Found window with substring '{}' at handle {:?}", substring, HWND(hwnd_value));
        Some(HWND(hwnd_value))
    }
}

/// Verifies if a window handle is still valid and visible
pub fn is_window_valid(hwnd: HWND) -> bool {
    unsafe {
        // Check if the window handle is still valid
        if IsWindow(hwnd).as_bool() {
            // Check if the window is visible
            if IsWindowVisible(hwnd).as_bool() {
                trace!("Window handle {:?} is valid and visible", hwnd);
                return true;
            } else {
                trace!("Window handle {:?} is valid but not visible", hwnd);
            }
        } else {
            debug!("Window handle {:?} is no longer valid", hwnd);
        }
        false
    }
}

/// Tries to get the window title for debugging purposes
pub fn get_window_title(hwnd: HWND) -> String {
    unsafe {
        let mut text: [u16; 512] = [0; 512];
        let length = GetWindowTextW(hwnd, &mut text);
        String::from_utf16_lossy(&text[..length as usize])
    }
}

unsafe extern "system" fn enum_window_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let context = &*(lparam.0 as *const SearchContext);
    
    // Skip windows that aren't visible
    if !IsWindowVisible(hwnd).as_bool() {
        return BOOL(1); // Continue enumeration
    }
    
    let mut text: [u16; 512] = [0; 512];
    let length = GetWindowTextW(hwnd, &mut text);
    let window_text = String::from_utf16_lossy(&text[..length as usize]).to_lowercase();

    if window_text.contains(&context.substring) {
        trace!("Found matching window: '{}'", window_text);
        context.result.store(hwnd.0, Ordering::Relaxed);
        BOOL(0) // Stop enumeration
    } else {
        BOOL(1) // Continue enumeration
    }
}
