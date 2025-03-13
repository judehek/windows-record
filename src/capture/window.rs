use std::sync::atomic::{AtomicBool, Ordering};
use std::collections::HashSet;
use std::borrow::Cow;
use windows::core::Result as WindowsResult;
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, WIN32_ERROR, GetLastError};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowTextW, IsWindow, IsWindowVisible
};
use log::{debug, trace, error};

/// A wrapper around the Windows HWND to provide more Rust-idiomatic functionality
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct WindowHandle(HWND);

impl std::hash::Hash for WindowHandle {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        // HWND is a wrapper around isize, so we can hash that directly
        self.0.0.hash(state);
    }
}

impl WindowHandle {
    /// Creates a new WindowHandle from a raw HWND
    pub fn new(hwnd: HWND) -> Self {
        Self(hwnd)
    }
    
    /// Gets the raw HWND value
    pub fn as_raw(&self) -> HWND {
        self.0
    }
    
    /// Checks if the window is valid and visible
    pub fn is_valid_and_visible(&self) -> bool {
        is_window_valid(self.0)
    }
    
    /// Gets the window's title text
    pub fn get_title(&self) -> WindowsResult<String> {
        get_window_title(self.0)
    }
}

/// Options for window search
pub struct WindowSearchOptions {
    /// Whether the search is case-sensitive
    pub case_sensitive: bool,
    /// Maximum number of results to return (None for unlimited)
    pub max_results: Option<usize>,
    /// Whether to match the exact string instead of substring
    pub exact_match: bool,
}

impl Default for WindowSearchOptions {
    fn default() -> Self {
        Self {
            case_sensitive: false,
            max_results: Some(1),
            exact_match: false,
        }
    }
}

struct SearchContext<'a> {
    search_text: Cow<'a, str>,
    results: Vec<HWND>,
    options: WindowSearchOptions,
    should_stop: AtomicBool,
}

/// Finds windows containing a given string in their title.
///
/// # Arguments
/// * `search_text` - The text to search for in window titles
/// * `options` - Search options (defaults to case-insensitive, first match only)
///
/// # Returns
/// A vector of window handles matching the search criteria
///
/// # Examples
/// ```
/// // Find the first window containing "notepad" (case-insensitive)
/// let window = find_windows_by_text("notepad", Default::default()).first().cloned();
///
/// // Find all windows containing "chrome" (case-insensitive)
/// let options = WindowSearchOptions {
///     max_results: None,
///     ..Default::default()
/// };
/// let chrome_windows = find_windows_by_text("chrome", options);
/// ```
pub fn find_windows_by_text(
    search_text: &str,
    options: WindowSearchOptions,
) -> Vec<WindowHandle> {
    let prepared_text = if options.case_sensitive {
        Cow::Borrowed(search_text)
    } else {
        Cow::Owned(search_text.to_lowercase())
    };
    
    let context = SearchContext {
        search_text: prepared_text,
        results: Vec::new(),
        options,
        should_stop: AtomicBool::new(false),
    };

    unsafe {
        let result = EnumWindows(
            Some(window_enumeration_callback),
            LPARAM(&context as *const _ as isize),
        );
        
        if !result.as_bool() {
            // EnumWindows failed for a reason other than our callback returning FALSE
            let error = unsafe { GetLastError() };
            if !context.should_stop.load(Ordering::Relaxed) {
                error!("EnumWindows failed with error code: {}", error.0);
            }
        }
    }

    // Convert raw HWNDs to our wrapper type
    context.results.into_iter()
        .map(WindowHandle::new)
        .collect()
}

/// Convenience function to get a single window by text (returns the first match)
pub fn get_window_by_text(search_text: &str) -> Option<WindowHandle> {
    find_windows_by_text(search_text, Default::default()).into_iter().next()
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

/// Gets the window title as a String
pub fn get_window_title(hwnd: HWND) -> WindowsResult<String> {
    unsafe {
        // First query with zero length to get the required buffer size
        let length = GetWindowTextW(hwnd, &mut []);
        
        if length == 0 {
            let error = unsafe { GetLastError() };
            if error.0 != 0 {
                return Err(windows::core::Error::from_win32());
            }
            // Window exists but has no title
            return Ok(String::new());
        }
        
        // Allocate buffer of appropriate size (plus 1 for null terminator)
        let mut buffer = vec![0u16; length as usize + 1];
        let actual_length = GetWindowTextW(hwnd, &mut buffer);
        
        if actual_length == 0 {
            let error = unsafe { GetLastError() };
            if error.0 != 0 {
                return Err(error.into());
            }
        }
        
        // Truncate buffer to actual length and convert to String
        buffer.truncate(actual_length as usize);
        Ok(String::from_utf16_lossy(&buffer))
    }
}

unsafe extern "system" fn window_enumeration_callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let context = &mut *(lparam.0 as *mut SearchContext);
    
    // Skip windows that aren't visible
    if !IsWindowVisible(hwnd).as_bool() {
        return BOOL(1); // Continue enumeration
    }
    
    // Get window title with dynamic allocation
    let window_text = match get_window_title(hwnd) {
        Ok(text) => text,
        Err(e) => {
            debug!("Failed to get window text for {:?}: {:?}", hwnd, e);
            return BOOL(1); // Continue enumeration
        }
    };
    
    // Prepare window text for comparison
    let comparable_text = if context.options.case_sensitive {
        Cow::Borrowed(&window_text)
    } else {
        Cow::Owned(window_text.to_lowercase())
    };
    
    let is_match = if context.options.exact_match {
        &*comparable_text == &*context.search_text
    } else {
        comparable_text.contains(&*context.search_text)
    };
    
    if is_match {
        trace!("Found matching window: '{}'", window_text);
        context.results.push(hwnd);
        
        // Check if we've reached the maximum number of results
        if let Some(max) = context.options.max_results {
            if context.results.len() >= max {
                context.should_stop.store(true, Ordering::Relaxed);
                return BOOL(0); // Stop enumeration
            }
        }
    }
    
    BOOL(1) // Continue enumeration
}

/// Get all visible window titles - useful for debugging
pub fn list_all_visible_windows() -> HashSet<String> {
    struct WindowCollector {
        windows: HashSet<String>,
    }
    
    let collector = WindowCollector {
        windows: HashSet::new(),
    };
    
    unsafe extern "system" fn collect_callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let collector = &mut *(lparam.0 as *mut WindowCollector);
        
        if IsWindowVisible(hwnd).as_bool() {
            if let Ok(title) = get_window_title(hwnd) {
                if !title.is_empty() {
                    collector.windows.insert(title);
                }
            }
        }
        
        BOOL(1) // Continue enumeration
    }
    
    unsafe {
        EnumWindows(
            Some(collect_callback),
            LPARAM(&collector as *const _ as isize),
        );
    }
    
    collector.windows
}