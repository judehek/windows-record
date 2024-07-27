use thiserror::Error;
use windows::{
    core::*,
    Win32::Media::MediaFoundation::*,
    Win32::Graphics::Dxgi::*,
    Win32::Graphics::Direct3D11::*,
    Win32::Graphics::Direct3D::*,
    Win32::System::Com::*,
    Win32::Foundation::*,
    Win32::UI::WindowsAndMessaging::*,
    Win32::Graphics::Dxgi::Common::*,
    Win32::Media::Audio::*,
    Win32::System::Com::StructuredStorage::*,
};
use std::cell::RefCell;
use std::mem::{size_of, ManuallyDrop };
use std::sync::atomic::{ AtomicBool, Ordering };
use std::time::Instant;
use std::{ptr, time::Duration};
use std::sync::atomic::AtomicIsize;
use std::sync::{Arc, Mutex, Condvar, Barrier};
use std::thread::JoinHandle;
use std::sync::mpsc::{Sender, Receiver, channel};

struct SendableSample(Arc<IMFSample>);
unsafe impl Send for SendableSample {}
unsafe impl Sync for SendableSample {}


#[derive(Clone)]
struct SendableWriter(Arc<IMFSinkWriter>);
unsafe impl Send for SendableWriter {}
unsafe impl Sync for SendableWriter {}


struct RecorderInner {
    recording: Arc<AtomicBool>,
    collect_video_handle: RefCell<Option<JoinHandle<Result<()>>>>,
    process_handle: RefCell<Option<JoinHandle<Result<()>>>>,
    collect_audio_handle: RefCell<Option<JoinHandle<Result<()>>>>,
}


unsafe fn create_sink_writer(filename: &str, fps_num: u32, fps_den: u32, s_width: u32, s_height: u32) -> Result<IMFSinkWriter> {
    let mut attributes: Option<IMFAttributes> = None;
    MFCreateAttributes(&mut attributes, 0)?;
    if let Some(attrs) = &attributes {
        attrs.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)?;
    }
    
    
    let sink_writer: IMFSinkWriter = MFCreateSinkWriterFromURL(
        &HSTRING::from(filename),
        None,
        attributes.as_ref(),
    )?;

    // Configuring the streams
    // Make output type!!!!!
    let video_output_type: IMFMediaType = MFCreateMediaType()?;
    video_output_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
    video_output_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
    video_output_type.SetUINT64(&MF_MT_FRAME_RATE, ((fps_num as u64) << 32) | (fps_den as u64))?;
    video_output_type.SetUINT64(&MF_MT_FRAME_SIZE, ((s_width as u64) << 32) | (s_height as u64))?;
    video_output_type.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1 << 32) | 1u64)?;
    video_output_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0.try_into().unwrap())?;

    // Video in Type
    let video_media_type: IMFMediaType = MFCreateMediaType()?;
    video_media_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
    video_media_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
    video_media_type.SetUINT64(&MF_MT_FRAME_RATE, ((fps_num as u64) << 32) | (fps_den as u64))?;
    video_media_type.SetUINT64(&MF_MT_FRAME_SIZE, ((s_width as u64) << 32) | (s_height as u64))?;
    video_media_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
    video_media_type.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1)?;
    video_media_type.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1u64 << 32) | 1u64)?;

    // Calculate and set the correct stride (should be multiple of 16 for optimal performance)
    let stride = s_width;
    video_media_type.SetUINT32(&MF_MT_DEFAULT_STRIDE, stride as u32)?;

    // Calculate and set the buffer size
    //let buffer_size = (stride * s_height * 3) / 2;
    //video_media_type.SetUINT32(&MF_MT_SAMPLE_SIZE, buffer_size as u32)?;

    // OUTPUT DOES ADD STREAM, THEN SET INPUT MEDIA TYPE ON STREAM !!!!
    let stream_val = sink_writer.AddStream(&video_output_type)?;

    let mut config_attrs: Option<IMFAttributes> = None;
    MFCreateAttributes(&mut config_attrs, 0)?;
    if let Some(attrs) = &config_attrs {
        attrs.SetUINT32(&CODECAPI_AVEncCommonRateControlMode, eAVEncCommonRateControlMode_GlobalVBR.0.try_into().unwrap())?;
        attrs.SetUINT32(&CODECAPI_AVEncCommonMeanBitRate, 5000000)?; // 1 Mbps
        //attrs.SetUINT32(&CODECAPI_AVEncCommonRealTime, 1)?;
    }

    sink_writer.SetInputMediaType(stream_val, &video_media_type, config_attrs.as_ref())?;
    
    // Audio Type
    let audio_output_type: IMFMediaType = MFCreateMediaType()?;
    audio_output_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
    audio_output_type.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_AAC)?;
    audio_output_type.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
    audio_output_type.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, 44100)?;
    audio_output_type.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 2)?;
    audio_output_type.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, 12000)?;


    let audio_media_type: IMFMediaType = MFCreateMediaType()?;
    let wav_format: WAVEFORMATEX = WAVEFORMATEX {
        wFormatTag: WAVE_FORMAT_PCM.try_into().unwrap(),
        nChannels: 2,
        nSamplesPerSec: 44100,
        nAvgBytesPerSec: 176400,
        nBlockAlign: 4,
        wBitsPerSample: 16,
        cbSize: 0
    };

    MFInitMediaTypeFromWaveFormatEx(&audio_media_type, &wav_format, size_of::<WAVEFORMATEX>().try_into().unwrap())?;
    let stream_val = sink_writer.AddStream(&audio_output_type)?;
    sink_writer.SetInputMediaType(stream_val, &audio_media_type, None)?;

    Ok(sink_writer)
} 

struct SearchContext {
    substring: String,
    result: AtomicIsize,
}

fn find_window_by_substring(substring: &str) -> Option<HWND> {
    let context = SearchContext {
        substring: substring.to_lowercase(),
        result: AtomicIsize::new(0),
    };

    unsafe {
        EnumWindows(Some(enum_window_proc), LPARAM(&context as *const _ as isize));
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

unsafe fn create_dxgi_sample(texture: &ID3D11Texture2D, fps_num: u32) -> Result<IMFSample> {
    let surface = texture.cast::<IDXGISurface>()?;

    let samp: IMFSample = MFCreateSample()?;
    let buff = MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, &surface, 0, TRUE)?;
    samp.AddBuffer(&buff)?;
    samp.SetSampleDuration(10_000_000 / fps_num as i64)?;
    Ok(samp)
}

unsafe fn create_blank_dxgi_texture(
    device: &ID3D11Device,
    width: u32,
    height: u32,
) -> Result<(ID3D11Texture2D, IDXGIResource)> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: D3D11_BIND_FLAG(40),
        CPUAccessFlags: D3D11_CPU_ACCESS_FLAG(0),
        MiscFlags: D3D11_RESOURCE_MISC_FLAG(10496),
    };

    let mut texture: Option<ID3D11Texture2D> = None;
    device.CreateTexture2D(
        &desc,
        None,
        Some(&mut texture),
    )?;

    let texture = texture.unwrap();
    let dxgi_resource: IDXGIResource = texture.cast()?;

    Ok((texture, dxgi_resource))
}

unsafe fn convert_bgra_to_nv12(
    device: &ID3D11Device,
    converter: &IMFTransform,
    in_sample: &IMFSample,
    width: u32,
    height: u32,
) -> Result<IMFSample> {
    let duration = in_sample.GetSampleDuration()?;
    let time = in_sample.GetSampleTime()?;
    //println!("Frame Count: {}", time / duration);

    // Create NV12 texture
    let mut nv12_desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_NV12,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_STAGING,
        BindFlags: D3D11_BIND_FLAG(0),
        CPUAccessFlags: D3D11_CPU_ACCESS_READ | D3D11_CPU_ACCESS_WRITE,
        MiscFlags: D3D11_RESOURCE_MISC_FLAG(0),
    };
    let mut nv12_texture = None;
    device.CreateTexture2D(&nv12_desc, None, Some(&mut nv12_texture))?;
    let nv12_texture = nv12_texture.unwrap();

    // Create output sample
    let output_sample: IMFSample = MFCreateSample()?;
    let nv12_surface: IDXGISurface = nv12_texture.cast()?;
    
    let output_buffer = MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, &nv12_surface, 0, FALSE)?;

    output_sample.AddBuffer(&output_buffer)?;
    //output_sample.SetSampleDuration(duration)?;
    //output_sample.SetSampleTime(time)?;

    // Process the frame
    converter.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0)?;

    converter.ProcessInput(0, in_sample, 0)?;
    converter.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)?;

    let mut output = MFT_OUTPUT_DATA_BUFFER {
        pSample: ManuallyDrop::new(Some(output_sample)),
        dwStatus: 0,
        pEvents: ManuallyDrop::new(None),
        dwStreamID: 0
    };

    let mut output_slice = std::slice::from_mut(&mut output);
    let mut test: u32 = 0;

    let x = converter.ProcessOutput(0,output_slice, &mut test);
    //println!("flags: {}", test);


    if let Err(e) = x {
        println!("{:?}", e);
        device.GetDeviceRemovedReason()?;
        return Err(e);
    }
    //drop(nv12_texture);
    ManuallyDrop::drop(&mut output_slice[0].pEvents);
    let samp = ManuallyDrop::take(&mut output_slice[0].pSample).ok_or(Error::from_win32())?;
    //samp.SetSampleDuration(duration)?;
    //samp.SetSampleTime(time)?;
    //println!("huh");
    Ok(samp)

}

unsafe fn collect_frames(
    send: Sender<SendableSample>,
    recording: Arc<AtomicBool>,
    hwnd: HWND,
    fps_num: u32,
    fps_den: u32,
    width: u32,
    height: u32,
    started: Arc<Barrier>,
    device: Arc<ID3D11Device>,
    context_mutex: Arc<Mutex<ID3D11DeviceContext>>
) -> Result<()> {
    // Get DXGI device
    let dxgi_device: IDXGIDevice = device.cast()?;

    // Get DXGI adapter
    let dxgi_adapter: IDXGIAdapter = dxgi_device.GetAdapter()?;

    // Get output
    let output = dxgi_adapter.EnumOutputs(0)?;

    // Get output1
    let output1: IDXGIOutput1 = output.cast()?;

    // Create desktop duplication
    let duplication = output1.DuplicateOutput(&*device)?;
    //let supported_formats = [DXGI_FORMAT_B8G8R8A8_UNORM];
    //let duplication = output1.DuplicateOutput1(&device, 0, &supported_formats)?;


    let frame_duration = Duration::from_nanos(1_000_000_000 * fps_den as u64 / fps_num as u64);
    let mut next_frame_time = Instant::now();
    let mut frame_count = 0;
    // let start = Instant::now();

    // let mut last_frame_overran = false;
    let mut accumulated_delay = Duration::ZERO;
    let mut num_duped = 0;

    /*let mut staging_texture: Option<ID3D11Texture2D> = None;
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    desc.Width = width;
    desc.Height = height;
    desc.MipLevels = 1;
    desc.ArraySize = 1;
    desc.Format = DXGI_FORMAT_B8G8R8A8_UNORM;
    desc.SampleDesc = DXGI_SAMPLE_DESC { Count: 1, Quality: 0 };
    desc.Usage = D3D11_USAGE_STAGING;
    desc.BindFlags = D3D11_BIND_FLAG(0);
    desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
    desc.MiscFlags = D3D11_RESOURCE_MISC_FLAG(0);
    device.CreateTexture2D(&desc, None, Some(&mut staging_texture))?;
    println!("test");
    let staging_texture = staging_texture.unwrap();*/

    let (blank_texture, _blank_resource) = create_blank_dxgi_texture(&device, width, height)?;

    println!("GOT TO WAIT");
    started.wait();
    while recording.load(Ordering::Relaxed) {
        // let mut frame_start = Instant::now();

        let mut resource: Option<IDXGIResource> = None;
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();

        let foreground_window = GetForegroundWindow();
        let is_target_window = foreground_window == hwnd;
        // println!("Init: {:?}", frame_start.elapsed());

        // Acquire next frame
        match duplication.AcquireNextFrame(0, &mut info, &mut resource) {
            Ok(_) => {
                if let Some(resource) = resource {
                    let texture: ID3D11Texture2D = resource.cast()?;
                    let mut staging_texture: Option<ID3D11Texture2D> = None;
                    let mut desc = D3D11_TEXTURE2D_DESC::default();
                    desc.Width = width;
                    desc.Height = height;
                    desc.MipLevels = 1;
                    desc.ArraySize = 1;
                    desc.Format = DXGI_FORMAT_B8G8R8A8_UNORM;
                    desc.SampleDesc = DXGI_SAMPLE_DESC { Count: 1, Quality: 0 };
                    desc.Usage = D3D11_USAGE_STAGING;
                    desc.BindFlags = D3D11_BIND_FLAG(0);
                    desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
                    desc.MiscFlags = D3D11_RESOURCE_MISC_FLAG(0);
                    device.CreateTexture2D(&desc, None, Some(&mut staging_texture))?;
                    let staging_texture = staging_texture.unwrap();

                    let context = context_mutex.lock().unwrap();
                    if is_target_window {
                        context.CopyResource(
                            &staging_texture,
                            &texture
                        );
                    } else {
                        context.CopyResource(
                            &staging_texture,
                            &blank_texture
                        );
                    }
                    drop(context);

                    while accumulated_delay >= frame_duration {
                        println!("Duping a frame to catch up");
                        println!("Accum: {:?}, duration: {:?}", accumulated_delay, frame_duration);

                        let samp = create_dxgi_sample(&staging_texture, fps_num)?;
                        samp.SetSampleTime((frame_count as i64 * 10_000_000i64 / fps_num as i64) as i64)?;
                        send.send(SendableSample(Arc::new(samp))).expect("Failed to send sample");
                        frame_count += 1;
                        next_frame_time += frame_duration;
                        accumulated_delay -= frame_duration;
                        num_duped += 1;
                    }
                    
                    let samp = create_dxgi_sample(&staging_texture, fps_num)?;
                    samp.SetSampleTime((frame_count as i64 * 10_000_000i64 / fps_num as i64) as i64)?;
                    send.send(SendableSample(Arc::new(samp))).expect("Failed to send sample");
                    
                    frame_count += 1;
                    next_frame_time += frame_duration;

                    let current_time = Instant::now();

                    if current_time > next_frame_time {
                        let overrun = current_time.duration_since(next_frame_time);
                        //println!("Frame {} overran by {:?}", frame_count, overrun);
                        // last_frame_overran = true;
                        accumulated_delay += overrun;
                    } else {
                        let sleep_time = next_frame_time.duration_since(current_time);
                        spin_sleep::sleep(sleep_time);
                    }


                    // Unmap staging texture
                    // context_mutex.lock().unwrap().Unmap(&staging_texture, 0);
                    // Release frame
                    duplication.ReleaseFrame()?;
                }
            }
            Err(error) if error.code() == DXGI_ERROR_WAIT_TIMEOUT => {
                // Timeout occurred, continue to next iteration
                continue;
            }
            Err(error) => return Err(error),
        }
    }


    println!("Num duped: {}", num_duped);
    Ok(())
}

unsafe fn process_samples(
    writer: SendableWriter, 
    rec_video: Receiver<SendableSample>,
    rec_audio: Receiver<SendableSample>, 
    recording: Arc<AtomicBool>,
    width: u32,
    height: u32,
    device: Arc<ID3D11Device>,
) -> Result<()> {
    let converter: IMFTransform = CoCreateInstance(&CLSID_VideoProcessorMFT, None, CLSCTX_INPROC_SERVER)?;

    // Set input media type (B8G8R8A8_UNORM)
    let input_type: IMFMediaType = MFCreateMediaType()?;
    input_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
    input_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_ARGB32)?;
    input_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0.try_into().unwrap())?;
    input_type.SetUINT64(&MF_MT_FRAME_SIZE, ((width as u64) << 32) | (height as u64))?;
    input_type.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1 << 32) | 1)?;
    input_type.SetUINT32(&MF_MT_DEFAULT_STRIDE, (width * 4) as u32)?; 
    converter.SetInputType(0, &input_type, 0)?;


    // Set output media type (NV12)
    let output_type: IMFMediaType = MFCreateMediaType()?;
    output_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
    output_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
    output_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0.try_into().unwrap())?;
    output_type.SetUINT64(&MF_MT_FRAME_SIZE, ((width as u64) << 32) | (height as u64))?;
    output_type.SetUINT32(&MF_MT_DEFAULT_STRIDE, width as u32)?;
    output_type.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1 << 32) | 1)?;
    converter.SetOutputType(0, &output_type, 0)?;




    while recording.load(Ordering::Relaxed) {
        // Video
        if let Ok(samp) = rec_video.try_recv() {
            let cvt = convert_bgra_to_nv12(&device, &converter, &*samp.0, width, height)?;
            writer.0.WriteSample(0, &cvt)?; 
            drop(samp);
            drop(cvt);
        }
        if let Ok(audio_samp) = rec_audio.try_recv() {
            writer.0.WriteSample(1, &*audio_samp.0)?;
            drop(audio_samp);
        }
    }
    writer.0.Finalize()?;
    Ok(())
}


// Define your completion handler struct
#[derive(Clone)]
#[implement(IActivateAudioInterfaceCompletionHandler)]
struct WASAPIActivateAudioInterfaceCompletionHandler {
    inner: Arc<(Mutex<InnerHandler>, Condvar)>,
}

struct InnerHandler {
    punk_audio_interface: Option<IUnknown>,
    done: bool,
}

// Implement the IActivateAudioInterfaceCompletionHandler interface
impl WASAPIActivateAudioInterfaceCompletionHandler {
    unsafe fn new() -> Self {
        Self {
            inner: Arc::new((Mutex::new(InnerHandler {
                punk_audio_interface: None,
                done: false
            }), 
            Condvar::new())),
        }
    }
}

impl IActivateAudioInterfaceCompletionHandler_Impl for WASAPIActivateAudioInterfaceCompletionHandler {
    fn ActivateCompleted(&self, activate_operation: Option<&IActivateAudioInterfaceAsyncOperation>) -> Result<()> {
        unsafe {
            let mut activate_result: HRESULT = E_UNEXPECTED;
            let mut inner = self.inner.0.lock().unwrap();
            activate_operation.unwrap().GetActivateResult(&mut activate_result, &mut inner.punk_audio_interface)?;
            inner.done = true;
            self.inner.1.notify_all();
        }
        Ok(())
    }
}

impl WASAPIActivateAudioInterfaceCompletionHandler {
    pub unsafe fn get_activate_result(&self) -> Result<IAudioClient> {
        let mut inner = self.inner.0.lock().unwrap();
        while !inner.done {
            inner = self.inner.1.wait(inner).unwrap();
        }
        inner.punk_audio_interface.as_ref().unwrap().cast()
    }
}

unsafe fn collect_audio(
    send: Sender<SendableSample>,
    recording: Arc<AtomicBool>,
    proc_id: u32,
    started: Arc<Barrier>
) -> Result<()> {
    // Create device enumerator
    let enumerator: IMMDeviceEnumerator = CoCreateInstance(
        &MMDeviceEnumerator,
        None,
        CLSCTX_ALL
    )?;

    // Get default audio endpoint
    let device: IMMDevice = enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?;


    // Set up audio client properties
    let stream_flags = AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_EVENTCALLBACK;

    let wave_format = WAVEFORMATEX {
        wFormatTag: WAVE_FORMAT_PCM.try_into().unwrap(),
        nChannels: 2,
        nSamplesPerSec: 44100,
        nAvgBytesPerSec: 176400,
        nBlockAlign: 4,
        wBitsPerSample: 16,
        cbSize: 0
    };
    


    // Set up activation params for process-specific capture
    let activation_params = AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 { 
            ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                TargetProcessId: proc_id,
                ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
            }
        },
    };

    let mut prop_variant = PROPVARIANT::default();

    (*prop_variant.Anonymous.Anonymous).vt = VT_BLOB;
    (*prop_variant.Anonymous.Anonymous).Anonymous.blob.cbSize = std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32;
    (*prop_variant.Anonymous.Anonymous).Anonymous.blob.pBlobData = &activation_params as *const _ as *mut _;


    // Initialize audio client
    let handler = WASAPIActivateAudioInterfaceCompletionHandler::new();
    let handler_interface: IActivateAudioInterfaceCompletionHandler = handler.clone().into();
    ActivateAudioInterfaceAsync(VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK, &IAudioClient::IID, Some(&mut prop_variant), &handler_interface)?;
    
    let audio_client = handler.get_activate_result()?;

    // Initialize the audio client
    audio_client.Initialize(
        AUDCLNT_SHAREMODE_SHARED,
        stream_flags,
        0,
        0,
        &wave_format,
        None,
    )?;
    // Get the capture client
    let capture_client: IAudioCaptureClient = audio_client.GetService()?;

    // Calculate the duration of each packet based on the format
    /*let packet_duration = Duration::from_nanos(
        (10_000_000.0 * (wave_format.nBlockAlign / 2) as f64 / wave_format.nSamplesPerSec as f64) as u64
    );*/

    let packet_duration = Duration::from_nanos((1000000000.0 / wave_format.nSamplesPerSec as f64) as u64);
    let packet_duration_hns = packet_duration.as_nanos() as i64 / 100;
    
    // Start audio capture
    audio_client.Start()?;
    let mut packet_count = 0;
    started.wait();
    while recording.load(Ordering::Relaxed) {
        let next_packet_size = capture_client.GetNextPacketSize()?;

        if next_packet_size > 0 {
            let mut buffer: *mut u8 = std::ptr::null_mut();
            let mut num_frames_available = 0;
            let mut flags = 0;
            let mut device_position = 0;
            let mut qpc_position = 0;

            capture_client.GetBuffer(
                &mut buffer,
                &mut num_frames_available,
                &mut flags,
                Some(&mut device_position),
                Some(&mut qpc_position)
            )?;

            if (flags & (AUDCLNT_BUFFERFLAGS_SILENT.0 as u32)) == 0 {
                // Process audio data
                let buffer_size = num_frames_available as usize * wave_format.nBlockAlign as usize;
                let audio_data = std::slice::from_raw_parts(buffer, buffer_size);

                // Create IMFSample
                let sample: IMFSample = MFCreateSample()?;
                let media_buffer: IMFMediaBuffer = MFCreateMemoryBuffer(buffer_size as u32)?;
                
                let mut buffer_ptr: *mut u8 = std::ptr::null_mut();
                let mut max_length = 0;
                let mut current_length = 0;
                media_buffer.Lock(&mut buffer_ptr, Some(&mut max_length), Some(&mut current_length))?;
                
                std::ptr::copy_nonoverlapping(audio_data.as_ptr(), buffer_ptr, buffer_size);
                
                media_buffer.SetCurrentLength(buffer_size as u32)?;
                media_buffer.Unlock()?;

                sample.AddBuffer(&media_buffer)?;
                sample.SetSampleTime(packet_count as i64 * packet_duration_hns)?;
                sample.SetSampleDuration(num_frames_available as i64 * packet_duration_hns)?;

                send.send(SendableSample(Arc::new(sample))).expect("Failed to send audio sample");
            }

            capture_client.ReleaseBuffer(num_frames_available)?;
            packet_count += num_frames_available;
        } else {
            // No data available, sleep for a short duration
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    // Stop audio capture
    audio_client.Stop()?;
    Ok(())
}


impl RecorderInner {
    fn init(
        filename: &str,
        fps_num: u32, 
        fps_den: u32, 
        screen_width: u32,
        screen_height: u32,
        process_name: &str,
    ) -> Result<Self> {
        // Init Libraries
        let recording = Arc::new(AtomicBool::new(true));
        let mut collect_video_handle: Option<JoinHandle<Result<()>>> = None;
        let mut process_handle: Option<JoinHandle<Result<()>>> = None;
        let mut collect_audio_handle: Option<JoinHandle<Result<()>>> = None;


        unsafe {
            CoInitializeEx(Some(ptr::null()), COINIT_MULTITHREADED)?;
            MFStartup(MF_VERSION, MFSTARTUP_FULL)?;

            let media_sink = create_sink_writer(filename, fps_num, fps_den, screen_width, screen_height)?;

            // Begin recording prob on new thread but check proc exists first
            let hwnd = find_window_by_substring(process_name).ok_or(Error::new(HRESULT(-1), HSTRING::from("No Window Found")))?;

            // Get the process ID
            let mut process_id: u32 = 0;
            GetWindowThreadProcessId(hwnd, Some(&mut process_id));
            println!("Process ID: {}", process_id);

            media_sink.BeginWriting()?;
            let sendable_sink = SendableWriter(Arc::new(media_sink));
            let (sender, receiver) = channel::<SendableSample>();
            let (sender_audio, receiver_audio) = channel::<SendableSample>();

            let barrier = Arc::new(Barrier::new(2));
            let barrier_clone = barrier.clone();



            let mut device: Option<ID3D11Device> = None;
            let mut context: Option<ID3D11DeviceContext> = None;
            let feature_levels = [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_10_1, D3D_FEATURE_LEVEL_10_0, D3D_FEATURE_LEVEL_9_3, D3D_FEATURE_LEVEL_9_2, D3D_FEATURE_LEVEL_9_1];
            
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                None,
                D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_DEBUG,
                Some(&feature_levels),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )?;
            let device = device.unwrap();
            let multithread: ID3D11Multithread = device.cast()?;
            multithread.SetMultithreadProtected(true);

            let context = context.unwrap();
            let context_mutex = Arc::new(Mutex::new(context));
            let device_ptr = Arc::new(device);
            let dev_clone = device_ptr.clone();

            let rec_clone = recording.clone();
            collect_video_handle = Some(std::thread::spawn(move || {
                collect_frames(sender, rec_clone, hwnd, fps_num, fps_den, screen_width, screen_height, barrier, dev_clone, context_mutex)
            }));

            let rec_clone = recording.clone();
            collect_audio_handle = Some(std::thread::spawn(move || {
                collect_audio(sender_audio, rec_clone, process_id, barrier_clone)
            }));

            let rec_clone = recording.clone();
            process_handle = Some(std::thread::spawn(move || {
                process_samples(sendable_sink, receiver, receiver_audio,  rec_clone, screen_width, screen_height, device_ptr)
            }));

        }   
        Ok(Self {
            recording,
            collect_video_handle: RefCell::new(collect_video_handle),
            process_handle: RefCell::new(process_handle),
            collect_audio_handle: RefCell::new(collect_audio_handle),
        })
    }

    fn stop(&self) -> std::result::Result<(), RecorderError> {
        println!("Stopping");
        if !self.recording.load(Ordering::Relaxed) {
            return Err(RecorderError::RecorderAlreadyStopped)
        }
        self.recording.store(false, Ordering::Relaxed);

        let frame_handle = self.collect_video_handle.borrow_mut().take();
        let audio_handle = self.collect_audio_handle.borrow_mut().take();
        let process_handle = self.process_handle.borrow_mut().take();

        println!("VIDEO: ");
        if let Some(handle) = frame_handle {
            if let Ok(res) = handle.join()
                .map_err(|_| RecorderError::Generic("Frame Handle Join Failed".to_string())) {
                    println!("{:?}", res)
                };
        }
        println!("AUDIO: ");
        if let Some(handle) = audio_handle {
            if let Ok(res) = handle.join()
                .map_err(|_| RecorderError::Generic("Audio Handle Join Failed".to_string())) {
                    println!("{:?}", res)
                };
        }
        println!("PROCESS: ");
        if let Some(handle) = process_handle {
            if let Ok(res) = handle.join()
                .map_err(|_| RecorderError::Generic("Process Handle Join Failed".to_string())) {
                    println!("{:?}", res)
                };
        }
        Ok(())
    }
}

impl Drop for RecorderInner {
    fn drop(&mut self) {
        unsafe {
            let _ = MFShutdown();
            CoUninitialize();
        }
    }
}


#[derive(Debug, Error)]
enum RecorderError {
    #[error("Generic Error: {0}")]
    Generic(String),
    #[error("Failed to Start the Recording Process, reason: {0}")]
    FailedToStart(String),
    #[error("Failed to Stop the Recording Process")]
    FailedToStop,
    #[error("Called to Stop when there is no Recorder Configured")]
    NoRecorderBound,
    #[error("Called to Stop when the Recorder is Already Stopped")]
    RecorderAlreadyStopped,
    #[error("No Process Specified for the Recorder")]
    NoProcessSpecified
}

struct RecorderConfigs {
    fps_num: u32,
    fps_den: u32,
    screen_width: u32,
    screen_height: u32,
}


struct Recorder {
    rec_inner: RefCell<Option<RecorderInner>>,
    rec_configs: RefCell<RecorderConfigs>,
    rec_proc_name: RefCell<Option<String>>
}

impl Recorder {
    pub fn new(fps_num: u32, fps_den: u32, screen_width: u32, screen_height: u32) -> Self {
        Self {
            rec_inner: RefCell::new(None),
            rec_configs: RefCell::new(RecorderConfigs {
                fps_den,
                fps_num,
                screen_width,
                screen_height,
            }),
            rec_proc_name: RefCell::new(None)
        }
    }

    pub fn set_configs(&self, fps_den: Option<u32>, fps_num: Option<u32>, screen_width: Option<u32>, screen_height: Option<u32>) {
        let mut ref_configs = self.rec_configs.borrow_mut();

        if let Some(den) = fps_den {
            ref_configs.fps_den = den
        }
        if let Some(num) = fps_num {
            ref_configs.fps_num = num
        }
        if let Some(width) = screen_width {
            ref_configs.screen_width = width
        }
        if let Some(height) = screen_height {
            ref_configs.screen_height = height
        }
    }

    pub fn set_process_name(&self, proc_name: &str) {
        let mut ref_proc_name = self.rec_proc_name.borrow_mut();

        *ref_proc_name = Some(proc_name.to_string());
    }

    pub fn start_recording(&self, filename: &str) -> std::result::Result<(), RecorderError> {
        let rec_configs = self.rec_configs.borrow();
        let rec_proc_name = self.rec_proc_name.borrow();
        let mut ref_rec_mut = self.rec_inner.borrow_mut();
        let Some(ref proc_name) = *rec_proc_name 
        else {
            return Err(RecorderError::NoProcessSpecified)
        };

        *ref_rec_mut = Some(RecorderInner::init(filename, rec_configs.fps_num, rec_configs.fps_den, rec_configs.screen_width, rec_configs.screen_height, proc_name).map_err(|e| RecorderError::FailedToStart(e.to_string()))?);
        Ok(())
    }

    pub fn stop_recording(&self) -> std::result::Result<(), RecorderError> {
        let ref_inner = self.rec_inner.borrow();

        let Some(ref rec_inner) = *ref_inner
        else {
            return Err(RecorderError::NoRecorderBound);
        };

        rec_inner.stop()
    }
}

fn main() -> windows::core::Result<()> {
    let rec = Recorder::new(60, 1, 1920, 1080);

    rec.set_process_name("League of Legends (TM)");
    // Potentially add codecs here
    //println!("Starting in 10");
    std::thread::sleep(Duration::from_secs(3));
    
    let res = rec.start_recording("output.mp4");
    println!("{:?}", res);
    std::thread::sleep(Duration::from_secs(60));
    let res2 = rec.stop_recording();
    println!("{:?}", res2);
    Ok(())
}