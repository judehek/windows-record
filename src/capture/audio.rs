use log::{debug, info};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::sync::Barrier;
use windows::core::{implement, IUnknown};
use windows::core::{ComInterface, Result};
use windows::Win32::Foundation::*;
use windows::Win32::Media::Audio::*;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::Threading::*;
use StructuredStorage::PROPVARIANT;

use crate::types::SendableSample;

#[derive(Clone)]
#[implement(IActivateAudioInterfaceCompletionHandler)]
struct WASAPIActivateAudioInterfaceCompletionHandler {
    inner: Arc<(std::sync::Mutex<InnerHandler>, std::sync::Condvar)>,
}

struct InnerHandler {
    punk_audio_interface: Option<IUnknown>,
    done: bool,
}

impl WASAPIActivateAudioInterfaceCompletionHandler {
    unsafe fn new() -> Self {
        Self {
            inner: Arc::new((
                std::sync::Mutex::new(InnerHandler {
                    punk_audio_interface: None,
                    done: false,
                }),
                std::sync::Condvar::new(),
            )),
        }
    }

    pub unsafe fn get_activate_result(&self) -> Result<IAudioClient> {
        let mut inner = self.inner.0.lock().unwrap();
        while !inner.done {
            inner = self.inner.1.wait(inner).unwrap();
        }
        inner.punk_audio_interface.as_ref().unwrap().cast()
    }
}

impl IActivateAudioInterfaceCompletionHandler_Impl
    for WASAPIActivateAudioInterfaceCompletionHandler
{
    fn ActivateCompleted(
        &self,
        activate_operation: Option<&IActivateAudioInterfaceAsyncOperation>,
    ) -> Result<()> {
        unsafe {
            let mut activate_result = E_UNEXPECTED;
            let mut inner = self.inner.0.lock().unwrap();
            activate_operation
                .unwrap()
                .GetActivateResult(&mut activate_result, &mut inner.punk_audio_interface)?;
            inner.done = true;
            self.inner.1.notify_all();
        }
        Ok(())
    }
}

pub unsafe fn collect_audio(
    send: Sender<SendableSample>,
    recording: Arc<AtomicBool>,
    proc_id: u32,
    started: Arc<Barrier>,
) -> Result<()> {
    SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_ABOVE_NORMAL);

    // Get QPC frequency at the start
    let mut qpc_freq = 0;
    QueryPerformanceFrequency(&mut qpc_freq);
    let ticks_to_hns = 10000000.0 / qpc_freq as f64; // Convert QPC ticks to 100ns units

    let wave_format = WAVEFORMATEX {
        wFormatTag: WAVE_FORMAT_PCM.try_into().unwrap(),
        nChannels: 2,
        nSamplesPerSec: 44100,
        nAvgBytesPerSec: 176400,
        nBlockAlign: 4,
        wBitsPerSample: 16,
        cbSize: 0,
    };

    let audio_client = setup_audio_client(proc_id, &wave_format)?;
    let capture_client: IAudioCaptureClient = audio_client.GetService()?;

    let packet_duration =
        std::time::Duration::from_nanos((1000000000.0 / wave_format.nSamplesPerSec as f64) as u64);
    let packet_duration_hns = packet_duration.as_nanos() as i64 / 100;

    // Get initial QPC value for relative timing
    let mut start_qpc_i64: i64 = 0;
    QueryPerformanceCounter(&mut start_qpc_i64);
    let start_qpc = start_qpc_i64 as u64;

    audio_client.Start()?;
    started.wait();

    while recording.load(Ordering::Relaxed) {
        let next_packet_size = capture_client.GetNextPacketSize()?;

        if next_packet_size > 0 {
            let mut buffer = ptr::null_mut();
            let mut num_frames_available = 0;
            let mut flags = 0;
            let mut device_position = 0;
            let mut qpc_position: u64 = 0;

            capture_client.GetBuffer(
                &mut buffer,
                &mut num_frames_available,
                &mut flags,
                Some(&mut device_position),
                Some(&mut qpc_position as *mut u64), // Pass as raw pointer
            )?;

            if (flags & (AUDCLNT_BUFFERFLAGS_SILENT.0 as u32)) == 0 {
                // Convert QPC time to 100ns units relative to start
                let relative_qpc = qpc_position - start_qpc;
                let time_hns = (relative_qpc as f64 * ticks_to_hns) as i64;

                let sample = create_audio_sample(
                    buffer,
                    num_frames_available,
                    &wave_format,
                    time_hns, // Now passing converted time in 100ns units
                    packet_duration_hns,
                )?;

                send.send(SendableSample(Arc::new(sample)))
                    .expect("Failed to send audio sample");
            }

            capture_client.ReleaseBuffer(num_frames_available)?;
        } else {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    audio_client.Stop()?;
    Ok(())
}

unsafe fn setup_audio_client(proc_id: u32, wave_format: &WAVEFORMATEX) -> Result<IAudioClient> {
    let activation_params = AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                TargetProcessId: proc_id,
                ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
            },
        },
    };

    let mut prop_variant = PROPVARIANT::default();
    (*prop_variant.Anonymous.Anonymous).vt = VT_BLOB;
    (*prop_variant.Anonymous.Anonymous).Anonymous.blob.cbSize =
        std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32;
    (*prop_variant.Anonymous.Anonymous).Anonymous.blob.pBlobData =
        &activation_params as *const _ as *mut _;

    let handler = WASAPIActivateAudioInterfaceCompletionHandler::new();
    let handler_interface: IActivateAudioInterfaceCompletionHandler = handler.clone().into();

    ActivateAudioInterfaceAsync(
        VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
        &IAudioClient::IID,
        Some(&mut prop_variant),
        &handler_interface,
    )?;

    let audio_client = handler.get_activate_result()?;

    audio_client.Initialize(
        AUDCLNT_SHAREMODE_SHARED,
        AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
        0,
        0,
        wave_format,
        None,
    )?;

    Ok(audio_client)
}

unsafe fn get_audio_buffer(capture_client: &IAudioCaptureClient) -> Result<(*mut u8, u32, u32)> {
    let mut buffer = ptr::null_mut();
    let mut num_frames_available = 0;
    let mut flags = 0;
    let mut device_position = 0;
    let mut qpc_position: u64 = 0;

    capture_client.GetBuffer(
        &mut buffer,
        &mut num_frames_available,
        &mut flags,
        Some(&mut device_position),
        Some(&mut qpc_position as *mut u64),
    )?;

    Ok((buffer, num_frames_available, flags))
}

unsafe fn create_audio_sample(
    buffer: *mut u8,
    num_frames: u32,
    wave_format: &WAVEFORMATEX,
    time_hns: i64, // Changed to i64 since we're now passing 100ns units directly
    packet_duration_hns: i64,
) -> Result<IMFSample> {
    let buffer_size = num_frames as usize * wave_format.nBlockAlign as usize;
    let audio_data = std::slice::from_raw_parts(buffer, buffer_size);

    let sample: IMFSample = MFCreateSample()?;
    let media_buffer: IMFMediaBuffer = MFCreateMemoryBuffer(buffer_size as u32)?;

    let mut buffer_ptr: *mut u8 = ptr::null_mut();
    let mut max_length = 0;
    let mut current_length = 0;

    media_buffer.Lock(
        &mut buffer_ptr,
        Some(&mut max_length),
        Some(&mut current_length),
    )?;
    ptr::copy_nonoverlapping(audio_data.as_ptr(), buffer_ptr, buffer_size);
    media_buffer.SetCurrentLength(buffer_size as u32)?;
    media_buffer.Unlock()?;

    sample.AddBuffer(&media_buffer)?;

    // Now using proper 100ns units directly
    sample.SetSampleTime(time_hns)?;
    sample.SetSampleDuration(num_frames as i64 * packet_duration_hns)?;

    Ok(sample)
}
