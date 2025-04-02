use std::sync::atomic::{AtomicIsize, Ordering};
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, RECT};
use windows::Win32::UI::WindowsAndMessaging::{GetWindowTextW, IsWindow, IsWindowVisible, GetWindowRect};
use log::{debug, info, trace, warn};

/// Defines how window titles should be matched
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowMatchType {
    /// Match any window whose title contains the given string (case-insensitive)
    Substring,
    /// Match only windows whose title exactly matches the given string (case-sensitive)
    ExactMatch,
}

impl Default for WindowMatchType {
    fn default() -> Self {
        WindowMatchType::Substring
    }
}

struct SearchContext {
    search_string: String,
    match_type: WindowMatchType,
    result: AtomicIsize,
}

/// Gets a window handle by searching for windows with titles matching the given string
/// Uses substring matching (case-insensitive)
pub fn get_window_by_string(search_string: &str) -> Option<HWND> {
    get_window_by_string_with_options(search_string, WindowMatchType::Substring)
}

/// Gets a window handle by searching for windows with titles exactly matching the given string
/// Uses exact matching (case-sensitive)
pub fn get_window_by_exact_string(search_string: &str) -> Option<HWND> {
    get_window_by_string_with_options(search_string, WindowMatchType::ExactMatch)
}

/// Gets a window handle by searching for windows with titles matching the given string and options
pub fn get_window_by_string_with_options(search_string: &str, match_type: WindowMatchType) -> Option<HWND> {
    let search_str = match match_type {
        WindowMatchType::Substring => search_string.to_lowercase(),
        WindowMatchType::ExactMatch => search_string.to_string(),
    };

    let context = SearchContext {
        search_string: search_str,
        match_type,
        result: AtomicIsize::new(0),
    };

    unsafe {
        windows::Win32::UI::WindowsAndMessaging::EnumWindows(
            Some(window_enumeration_callback),
            LPARAM(&context as *const _ as isize),
        );
    }

    let hwnd_value = context.result.load(Ordering::Relaxed);
    if hwnd_value == 0 {
        debug!("No window found matching '{}' with {:?}", search_string, match_type);
        None
    } else {
        debug!("Found window matching '{}' with {:?} at handle {:?}", 
               search_string, match_type, HWND(hwnd_value));
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

/// Gets the window position and size
pub fn get_window_rect(hwnd: HWND) -> Option<(i32, i32, u32, u32)> {
    unsafe {
        let mut rect = RECT::default();
        if GetWindowRect(hwnd, &mut rect).as_bool() {
            let width = (rect.right - rect.left) as u32;
            let height = (rect.bottom - rect.top) as u32;
            
            // Get window title for better logging
            let title = get_window_title(hwnd);
            let title_str = if title.is_empty() { "<Unnamed>" } else { &title };
            
            info!("Window '{}' rect: [{}, {}, {}, {}] - {}x{}", 
                title_str, rect.left, rect.top, rect.right, rect.bottom, width, height);
                
            Some((rect.left, rect.top, width, height))
        } else {
            let error_code = windows::Win32::Foundation::GetLastError();
            warn!("Failed to get window rect for hwnd: {:?}, error: {:?}", hwnd, error_code);
            None
        }
    }
}

unsafe extern "system" fn window_enumeration_callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let context = &*(lparam.0 as *const SearchContext);
    
    // Skip windows that aren't visible
    if !IsWindowVisible(hwnd).as_bool() {
        return BOOL(1); // Continue enumeration
    }
    
    let mut text: [u16; 512] = [0; 512];
    let length = GetWindowTextW(hwnd, &mut text);
    
    let window_text = match context.match_type {
        WindowMatchType::Substring => {
            String::from_utf16_lossy(&text[..length as usize]).to_lowercase()
        },
        WindowMatchType::ExactMatch => {
            String::from_utf16_lossy(&text[..length as usize])
        },
    };
    
    // Print both strings for debugging
    if context.match_type == WindowMatchType::ExactMatch {
        log::debug!("Exact matching: '{}' vs '{}'", window_text, context.search_string);
    }
    
    let is_match = match context.match_type {
        WindowMatchType::Substring => {
            window_text.contains(&context.search_string)
        },
        WindowMatchType::ExactMatch => {
            window_text == context.search_string
        },
    };
    
    if is_match {
        trace!("Found matching window: '{}'", window_text);
        context.result.store(hwnd.0, Ordering::Relaxed);
        BOOL(0) // Stop enumeration
    } else {
        BOOL(1) // Continue enumeration
    }
}
