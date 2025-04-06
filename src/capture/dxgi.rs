use log::{debug, trace, info, warn, error};
use windows::core::{ComInterface, Result};
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D};
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::Foundation::{HWND, POINT, RECT};

/// Gets the adapter that the window is on
pub unsafe fn get_adapter_for_window(device: &ID3D11Device, hwnd: HWND) -> Result<IDXGIAdapter> {
    // Get DXGI device
    let dxgi_device: IDXGIDevice = device.cast()?;

    // Get adapter factory
    let dxgi_adapter = dxgi_device.GetAdapter()?;
    let factory: IDXGIFactory1 = dxgi_adapter.GetParent()?;

    // Get window position
    let mut window_rect = RECT::default();
    if !windows::Win32::UI::WindowsAndMessaging::GetWindowRect(hwnd, &mut window_rect).as_bool() {
        // If we can't get the window rect, just return the default adapter
        info!("Failed to get window rect, using default adapter");
        return dxgi_device.GetAdapter();
    }

    // Calculate window center point
    let window_center = POINT {
        x: (window_rect.left + window_rect.right) / 2,
        y: (window_rect.top + window_rect.bottom) / 2,
    };

    // Find the output that contains this point
    let mut adapter_index = 0;
    let mut best_adapter: Option<IDXGIAdapter> = None;

    // Enumerate all adapters
    loop {
        let adapter = match factory.EnumAdapters(adapter_index) {
            Ok(adapter) => adapter,
            Err(_) => break, // No more adapters
        };

        // Try to find an output on this adapter that contains our window
        let mut output_index = 0;
        loop {
            let output = match adapter.EnumOutputs(output_index) {
                Ok(output) => output,
                Err(_) => break, // No more outputs on this adapter
            };

            // Get the desktop coordinates of this output
            let mut monitor_info = DXGI_OUTPUT_DESC::default();
            if output.GetDesc(&mut monitor_info).is_ok() {
                let monitor_rect = monitor_info.DesktopCoordinates;

                // Check if window center is on this monitor
                if window_center.x >= monitor_rect.left
                    && window_center.x < monitor_rect.right
                    && window_center.y >= monitor_rect.top
                    && window_center.y < monitor_rect.bottom
                {
                    // Found the monitor containing the window
                    info!("Window is on adapter {} output {}", adapter_index, output_index);
                    return Ok(adapter);
                }
            }

            output_index += 1;
        }

        // If we haven't found a matching output but this is the first adapter,
        // save it as a fallback option
        if best_adapter.is_none() {
            best_adapter = Some(adapter.clone());
        }

        adapter_index += 1;
    }

    // If we got here, we didn't find the specific adapter/output
    // Use the first adapter as fallback
    match best_adapter {
        Some(adapter) => {
            info!("Window monitor not found, using first adapter as fallback");
            Ok(adapter)
        }
        None => {
            // Shouldn't happen unless there are no adapters at all
            error!("No adapters found, using device's default adapter");
            dxgi_device.GetAdapter()
        }
    }
}

pub unsafe fn setup_dxgi_duplication(device: &ID3D11Device) -> Result<IDXGIOutputDuplication> {
    // Get DXGI device
    let dxgi_device: IDXGIDevice = device.cast()?;

    // Get adapter
    let dxgi_adapter: IDXGIAdapter = dxgi_device.GetAdapter()?;

    // Get output
    let output = dxgi_adapter.EnumOutputs(0)?;
    let output1: IDXGIOutput1 = output.cast()?;

    // Create duplication with flag to include cursor
    let duplication = output1.DuplicateOutput(device)?;

    // Log cursor capabilities
    let mut desc = DXGI_OUTDUPL_DESC::default();
    duplication.GetDesc(&mut desc);
    debug!("DXGI Output Duplication Description:");
    debug!(
        "  - Desktop image capture supported: {}",
        !desc.DesktopImageInSystemMemory.as_bool()
    );
    debug!(
        "  - Cursor capture supported: {}",
        desc.DesktopImageInSystemMemory.as_bool()
    );

    Ok(duplication)
}

/// Sets up a DXGI duplication for a specific window by finding the adapter/output the window is on
pub unsafe fn setup_dxgi_duplication_for_window(
    device: &ID3D11Device, 
    hwnd: HWND
) -> Result<IDXGIOutputDuplication> {
    // Get DXGI device
    let dxgi_device: IDXGIDevice = device.cast()?;

    // Get adapter that the window is on
    let dxgi_adapter = get_adapter_for_window(device, hwnd)?;

    // Get window position
    let mut window_rect = RECT::default();
    if !windows::Win32::UI::WindowsAndMessaging::GetWindowRect(hwnd, &mut window_rect).as_bool() {
        warn!("Failed to get window rect, using default output");
        // Fall back to first output if we can't get window rect
        let output = dxgi_adapter.EnumOutputs(0)?;
        let output1: IDXGIOutput1 = output.cast()?;
        
        info!("Using adapter's first output");
        let duplication = output1.DuplicateOutput(device)?;
        
        // Log cursor capabilities
        let mut desc = DXGI_OUTDUPL_DESC::default();
        duplication.GetDesc(&mut desc);
        debug!("DXGI Output Duplication Description:");
        debug!(
            "  - Desktop image capture supported: {}",
            !desc.DesktopImageInSystemMemory.as_bool()
        );
        debug!(
            "  - Cursor capture supported: {}",
            desc.DesktopImageInSystemMemory.as_bool()
        );
        
        return Ok(duplication);
    }

    // Calculate window center point
    let window_center = POINT {
        x: (window_rect.left + window_rect.right) / 2,
        y: (window_rect.top + window_rect.bottom) / 2,
    };

    // Find the output that contains this point
    let mut output_index = 0;
    loop {
        let output = match dxgi_adapter.EnumOutputs(output_index) {
            Ok(output) => output,
            Err(_) => {
                if output_index == 0 {
                    // No outputs found, shouldn't happen
                    error!("No outputs found on adapter");
                    return Err(windows::core::Error::from_win32());
                }
                // No more outputs, fall back to the first one
                info!("Window not found on any output, falling back to output 0");
                let output = dxgi_adapter.EnumOutputs(0)?;
                let output1: IDXGIOutput1 = output.cast()?;
                return output1.DuplicateOutput(device);
            }
        };

        // Get the desktop coordinates of this output
        let mut monitor_info = DXGI_OUTPUT_DESC::default();
        if output.GetDesc(&mut monitor_info).is_ok() {
            let monitor_rect = monitor_info.DesktopCoordinates;

            // Check if window center is on this monitor
            if window_center.x >= monitor_rect.left
                && window_center.x < monitor_rect.right
                && window_center.y >= monitor_rect.top
                && window_center.y < monitor_rect.bottom
            {
                // Found the monitor containing the window
                info!("Window is on output {}", output_index);
                let output1: IDXGIOutput1 = output.cast()?;
                let duplication = output1.DuplicateOutput(device)?;
                
                // Log cursor capabilities
                let mut desc = DXGI_OUTDUPL_DESC::default();
                duplication.GetDesc(&mut desc);
                debug!("DXGI Output Duplication Description:");
                debug!(
                    "  - Desktop image capture supported: {}",
                    !desc.DesktopImageInSystemMemory.as_bool()
                );
                debug!(
                    "  - Cursor capture supported: {}",
                    desc.DesktopImageInSystemMemory.as_bool()
                );
                
                return Ok(duplication);
            }
        }

        output_index += 1;
    }
}
