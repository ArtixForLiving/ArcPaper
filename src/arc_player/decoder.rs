//! Hardware-accelerated Direct3D 11 video decoder leveraging Windows Media Foundation Transform (MFT).
//!
//! Feeds Annex-B NAL unit bitstreams into D3D11-aware hardware decoders (H.264 / HEVC)
//! and outputs zero-copy NV12 ID3D11Texture2D surfaces residing directly in GPU VRAM.

use std::mem::ManuallyDrop;
use windows::Win32::Foundation::E_FAIL;
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::core::{Error, Interface, Result};

use super::demux::VideoCodec;

/// Decoded hardware video frame referencing an NV12 texture directly in VRAM.
pub struct DecodedFrame {
    pub texture: ID3D11Texture2D,
    pub subresource_index: u32,
    pub pts_100ns: i64,
}

/// GPU hardware video decoder wrapper (H.264 / HEVC) bound to Direct3D 11.
pub struct Decoder {
    transform: IMFTransform,
    _device_manager: IMFDXGIDeviceManager,
    width: u32,
    height: u32,
}

impl Decoder {
    /// Initializes Windows Media Foundation, binds the D3D11 device, and configures the hardware MFT.
    pub fn new(device: &ID3D11Device, codec: VideoCodec, width: u32, height: u32) -> Result<Self> {
        // 1. Startup Media Foundation
        unsafe {
            MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET)?;
        }

        // 2. Create DXGI Device Manager and associate our D3D11 device
        let mut reset_token = 0u32;
        let mut device_manager: Option<IMFDXGIDeviceManager> = None;
        unsafe {
            MFCreateDXGIDeviceManager(&mut reset_token, &mut device_manager)?;
        }
        let device_manager = device_manager.ok_or_else(|| Error::from_hresult(E_FAIL))?;
        unsafe {
            device_manager.ResetDevice(device, reset_token)?;
        }

        // 3. Locate and instantiate hardware video decoder MFT
        let input_subtype = match codec {
            VideoCodec::H264 => MFVideoFormat_H264,
            VideoCodec::Hevc => MFVideoFormat_HEVC,
        };

        let input_type_info = MFT_REGISTER_TYPE_INFO {
            guidMajorType: MFMediaType_Video,
            guidSubtype: input_subtype,
        };

        let mut activate_list: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut num_activates = 0u32;

        // Try hardware-accelerated MFTs first
        let hr_hw = unsafe {
            MFTEnumEx(
                MFT_CATEGORY_VIDEO_DECODER,
                MFT_ENUM_FLAG_SYNCMFT
                    | MFT_ENUM_FLAG_LOCALMFT
                    | MFT_ENUM_FLAG_SORTANDFILTER
                    | MFT_ENUM_FLAG_HARDWARE,
                Some(&input_type_info),
                None,
                &mut activate_list,
                &mut num_activates,
            )
        };

        // Fallback to standard synchronous MFT (which dynamically unlocks D3D11VA via SET_D3D_MANAGER)
        if hr_hw.is_err() || num_activates == 0 || activate_list.is_null() {
            if !activate_list.is_null() {
                unsafe {
                    CoTaskMemFree(Some(activate_list as _));
                }
                activate_list = std::ptr::null_mut();
                num_activates = 0;
            }

            unsafe {
                MFTEnumEx(
                    MFT_CATEGORY_VIDEO_DECODER,
                    MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_SORTANDFILTER,
                    Some(&input_type_info),
                    None,
                    &mut activate_list,
                    &mut num_activates,
                )?;
            }
        }

        if num_activates == 0 || activate_list.is_null() {
            return Err(Error::from_hresult(E_FAIL));
        }

        let transform: IMFTransform = unsafe {
            let act = (*activate_list)
                .as_ref()
                .ok_or_else(|| Error::from_hresult(E_FAIL))?;
            act.ActivateObject()?
        };

        // Safely free the activate list
        unsafe {
            for i in 0..num_activates as usize {
                let _ = (*activate_list.add(i)).take();
            }
            CoTaskMemFree(Some(activate_list as _));
        }

        // 4. Enable D3D11 awareness, shader resource bind flags, and low latency attributes
        use windows::Win32::Graphics::Direct3D11::{D3D11_BIND_DECODER, D3D11_BIND_SHADER_RESOURCE};
        let bind_flags = (D3D11_BIND_DECODER.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32;

        if let Ok(attrs) = unsafe { transform.GetAttributes() } {
            let _ = unsafe { attrs.SetUINT32(&MF_SA_D3D11_AWARE, 1) };
            let _ = unsafe { attrs.SetUINT32(&MF_SA_D3D11_BINDFLAGS, bind_flags) };
            let _ = unsafe { attrs.SetUINT32(&MF_LOW_LATENCY, 1) };
            let _ = unsafe { attrs.SetUINT32(&CODECAPI_AVLowLatencyMode, 1) };
        }

        if let Ok(out_attrs) = unsafe { transform.GetOutputStreamAttributes(0) } {
            let _ = unsafe { out_attrs.SetUINT32(&MF_SA_D3D11_BINDFLAGS, bind_flags) };
        }

        // 5. Send D3D11 device manager to MFT
        let raw_mgr = device_manager.as_raw();
        unsafe {
            transform.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, raw_mgr as usize)?;
        }

        // 6. Configure Input Media Type
        let in_type = unsafe { MFCreateMediaType()? };
        unsafe {
            in_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            in_type.SetGUID(&MF_MT_SUBTYPE, &input_subtype)?;
            let frame_size = ((width as u64) << 32) | (height as u64);
            in_type.SetUINT64(&MF_MT_FRAME_SIZE, frame_size)?;
            in_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            transform.SetInputType(0, &in_type, 0)?;
        }

        let mut decoder = Self {
            transform,
            _device_manager: device_manager,
            width,
            height,
        };

        // 7. Configure Output Media Type to NV12
        decoder.reconfigure_nv12_output()?;

        // 8. Start streaming
        unsafe {
            decoder
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            decoder
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
        }

        Ok(decoder)
    }

    /// Reconfigures or renegotiates the output media type to NV12 format.
    pub fn reconfigure_nv12_output(&mut self) -> Result<()> {
        let mut type_index = 0;
        let mut found = false;

        while let Ok(out_type) = unsafe { self.transform.GetOutputAvailableType(0, type_index) } {
            if unsafe { out_type.GetGUID(&MF_MT_SUBTYPE) } == Ok(MFVideoFormat_NV12) {
                if let Ok(frame_size) = unsafe { out_type.GetUINT64(&MF_MT_FRAME_SIZE) } {
                    self.width = (frame_size >> 32) as u32;
                    self.height = (frame_size & 0xFFFFFFFF) as u32;
                }
                unsafe {
                    self.transform.SetOutputType(0, &out_type, 0)?;
                }
                found = true;
                break;
            }
            type_index += 1;
        }

        if !found {
            let out_type = unsafe { MFCreateMediaType()? };
            unsafe {
                out_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
                out_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
                let frame_size = ((self.width as u64) << 32) | (self.height as u64);
                out_type.SetUINT64(&MF_MT_FRAME_SIZE, frame_size)?;
                out_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
                self.transform.SetOutputType(0, &out_type, 0)?;
            }
        }

        Ok(())
    }

    /// Feeds an Annex-B video slice and drains decoded D3D11 NV12 frames with strict COM reference hygiene.
    pub fn feed_and_decode(
        &mut self,
        annexb_packet: &[u8],
        pts_100ns: i64,
        duration_100ns: i64,
    ) -> Result<Option<DecodedFrame>> {
        // 1. Process Input bitstream sample
        if !annexb_packet.is_empty() {
            let buffer = unsafe { MFCreateMemoryBuffer(annexb_packet.len() as u32)? };
            let mut dest_ptr = std::ptr::null_mut();
            let mut max_len = 0u32;
            let mut cur_len = 0u32;

            unsafe {
                buffer.Lock(&mut dest_ptr, Some(&mut max_len), Some(&mut cur_len))?;
                std::ptr::copy_nonoverlapping(
                    annexb_packet.as_ptr(),
                    dest_ptr,
                    annexb_packet.len(),
                );
                buffer.Unlock()?;
                buffer.SetCurrentLength(annexb_packet.len() as u32)?;

                let sample = MFCreateSample()?;
                sample.AddBuffer(&buffer)?;
                sample.SetSampleTime(pts_100ns)?;
                sample.SetSampleDuration(duration_100ns)?;

                self.transform.ProcessInput(0, &sample, 0)?;
            }
            // `buffer` and `sample` drop out of scope here immediately
        }

        // 2. Drain Output frames from MFT
        loop {
            let stream_info = unsafe { self.transform.GetOutputStreamInfo(0)? };
            let provides_samples =
                (stream_info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32) != 0;

            let mut output_data = MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: 0,
                pSample: if provides_samples {
                    ManuallyDrop::new(None)
                } else {
                    unsafe { ManuallyDrop::new(Some(MFCreateSample()?)) }
                },
                dwStatus: 0,
                pEvents: ManuallyDrop::new(None),
            };

            let mut status = 0u32;
            let res = unsafe {
                self.transform
                    .ProcessOutput(0, std::slice::from_mut(&mut output_data), &mut status)
            };

            let sample_opt = ManuallyDrop::into_inner(output_data.pSample);
            let _events = ManuallyDrop::into_inner(output_data.pEvents);

            if let Err(e) = res {
                if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT {
                    return Ok(None);
                }
                if e.code() == MF_E_TRANSFORM_STREAM_CHANGE {
                    self.reconfigure_nv12_output()?;
                    continue;
                }
                return Err(e);
            }

            if let Some(sample) = sample_opt {
                let pts = unsafe { sample.GetSampleTime().unwrap_or(pts_100ns) };
                let media_buf = unsafe { sample.GetBufferByIndex(0)? };
                let dxgi_buf: IMFDXGIBuffer = media_buf.cast()?;
                let mut texture: Option<ID3D11Texture2D> = None;
                unsafe {
                    dxgi_buf.GetResource(
                        &ID3D11Texture2D::IID,
                        &mut texture as *mut _ as *mut *mut core::ffi::c_void,
                    )?;
                }
                let texture = texture.ok_or_else(|| Error::from_hresult(E_FAIL))?;
                let subresource_index = unsafe { dxgi_buf.GetSubresourceIndex()? };

                // `media_buf` and `sample` are dropped here, releasing COM refcounts
                return Ok(Some(DecodedFrame {
                    texture,
                    subresource_index,
                    pts_100ns: pts,
                }));
            }
        }
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        unsafe {
            let _ = self
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arc_player::demux::Demuxer;
    use crate::arc_player::loop_ctrl::LoopController;
    use std::path::Path;
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::Graphics::Direct3D::*;
    use windows::Win32::Graphics::Direct3D11::*;

    #[test]
    fn test_hardware_decode_real_video() {
        let test_path = Path::new(r"D:\Arc Paper\SavedWallpapers\cyanex-in-warframe.1920x1080.mp4");
        if !test_path.exists() {
            return;
        }
        let mut device = None;
        let mut context = None;
        let flags = D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT;
        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                flags,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
            .expect("D3D11 device creation failed");
        }
        let device = device.unwrap();
        println!("D3D11 device created successfully!");

        let mut demuxer = Demuxer::new();
        demuxer.open(test_path).expect("Failed to open test MP4");
        let codec = demuxer.codec().expect("Failed to detect codec");
        use std::io::Write;
        println!(
            "Demuxer opened! Codec: {:?}, {}x{}",
            codec,
            demuxer.width(),
            demuxer.height()
        );
        let _ = std::io::stdout().flush();

        let mut decoder = Decoder::new(&device, codec, demuxer.width(), demuxer.height())
            .expect("Hardware Decoder initialization failed");

        let mut loop_ctrl = LoopController::new(demuxer.frame_count());
        let mut decoded_frames = 0;

        for _ in 0..60 {
            let (packet, pts, dur, _) = demuxer
                .read_next_annexb_packet(&mut loop_ctrl)
                .expect("Failed reading packet");

            if let Some(frame) = decoder
                .feed_and_decode(packet, pts, dur)
                .expect("Hardware decode error")
            {
                assert!(frame.pts_100ns >= 0);
                decoded_frames += 1;
            }
        }

        assert!(
            decoded_frames > 0,
            "Hardware decoder must decode frames into D3D11 textures!"
        );
    }
}
