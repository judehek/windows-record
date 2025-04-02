use log::info;
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromWindow, MONITORINFOEXW, MONITOR_DEFAULTTOPRIMARY};
use windows::Win32::UI::WindowsAndMessaging::{GetDesktopWindow, MONITORINFOF_PRIMARY};

/// Gets the resolution of the primary monitor
/// Returns a tuple of (width, height)
pub fn get_primary_monitor_resolution() -> (u32, u32) {
    unsafe {
        // Get desktop window handle
        let desktop_hwnd = GetDesktopWindow();
        info!("Got desktop window handle");
        
        // Get the monitor handle from the window
        let monitor = MonitorFromWindow(desktop_hwnd, MONITOR_DEFAULTTOPRIMARY);
        info!("Got monitor handle from desktop window");
        
        // Get monitor info
        let mut monitor_info: MONITORINFOEXW = Default::default();
        monitor_info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        
        if GetMonitorInfoW(monitor, &mut monitor_info as *mut MONITORINFOEXW as *mut _).as_bool() {
            let rc_monitor = monitor_info.monitorInfo.rcMonitor;
            let width = (rc_monitor.right - rc_monitor.left) as u32;
            let height = (rc_monitor.bottom - rc_monitor.top) as u32;
            
            info!("Primary monitor resolution: {}x{}", width, height);
            (width, height)
        } else {
            // Fallback to a common resolution if we can't get the monitor info
            info!("Failed to get monitor info, using default 1920x1080");
            (1920, 1080)
        }
    }
}

/// Gets the resolution of the monitor that contains the specified window
/// Returns a tuple of (width, height)
pub fn get_window_monitor_resolution(hwnd: HWND) -> (u32, u32) {
    unsafe {
        // Get the monitor handle from the window
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTOPRIMARY);
        
        // Get monitor info
        let mut monitor_info: MONITORINFOEXW = Default::default();
        monitor_info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        
        if GetMonitorInfoW(monitor, &mut monitor_info as *mut MONITORINFOEXW as *mut _).as_bool() {
            let rc_monitor = monitor_info.monitorInfo.rcMonitor;
            let width = (rc_monitor.right - rc_monitor.left) as u32;
            let height = (rc_monitor.bottom - rc_monitor.top) as u32;
            
            info!("Monitor resolution for window: {}x{}", width, height);
            (width, height)
        } else {
            // Fallback to primary monitor resolution
            info!("Failed to get window's monitor info, falling back to primary monitor");
            get_primary_monitor_resolution()
        }
    }
}