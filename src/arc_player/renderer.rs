//! Pixel renderer executing a single-pass HLSL shader for zero-copy NV12-to-RGB conversion,
//! aspect-ratio scaling (Cover, Contain, Fill), and brightness adjustments.

use windows::Win32::Foundation::E_FAIL;
use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::core::{Error, Result, s};

use super::ScreenFit;
use super::presenter::Presenter;

/// Embedded HLSL shader code for fullscreen triangle vertex shader and NV12-to-RGB pixel shader.
const SHADER_SOURCE: &[u8] = b"
cbuffer RenderParams : register(b0) {
    float2 uv_scale;
    float2 uv_offset;
    float brightness;
    uint slice_index;
    float2 _padding;
};

Texture2DArray<float> tex_y : register(t0);
Texture2DArray<float2> tex_uv : register(t1);
SamplerState sam_linear : register(s0);

struct VSOutput {
    float4 pos : SV_Position;
    float2 uv  : TEXCOORD0;
};

VSOutput vs_main(uint id : SV_VertexID) {
    VSOutput output;
    // Generate fullscreen triangle covering [-1, 1] clip space from SV_VertexID (0, 1, 2)
    output.uv = float2((id == 1) ? 2.0f : 0.0f, (id == 2) ? 2.0f : 0.0f);
    output.pos = float4(output.uv.x * 2.0f - 1.0f, 1.0f - output.uv.y * 2.0f, 0.0f, 1.0f);
    return output;
}

float4 ps_main(VSOutput input) : SV_Target {
    float2 uv = input.uv * uv_scale + uv_offset;

    // Solid black letterbox/pillarbox bars for Contain mode
    if (uv.x < 0.0f || uv.x > 1.0f || uv.y < 0.0f || uv.y > 1.0f) {
        return float4(0.0f, 0.0f, 0.0f, 1.0f);
    }

    float3 coord = float3(uv, (float)slice_index);
    float y = tex_y.Sample(sam_linear, coord);
    float2 uv_chroma = tex_uv.Sample(sam_linear, coord);

    // ITU-R BT.709 Limited Range standard color conversion matrix
    float y_adj = 1.164383f * (y - (16.0f / 255.0f));
    float cb = uv_chroma.x - (128.0f / 255.0f);
    float cr = uv_chroma.y - (128.0f / 255.0f);

    float r = y_adj + 1.792742f * cr;
    float g = y_adj - 0.213249f * cb - 0.532909f * cr;
    float b = y_adj + 2.112402f * cb;

    float3 rgb = saturate(float3(r, g, b)) * brightness;
    return float4(rgb, 1.0f);
}
\0";

/// 16-byte aligned parameters mapped into Constant Buffer register b0.
#[repr(C)]
#[derive(Clone, Copy)]
struct RenderParams {
    uv_scale: [f32; 2],
    uv_offset: [f32; 2],
    brightness: f32,
    slice_index: u32,
    _padding: [f32; 2],
}

/// Direct3D 11 video pixel renderer.
pub struct Renderer {
    vertex_shader: ID3D11VertexShader,
    pixel_shader: ID3D11PixelShader,
    sampler_state: ID3D11SamplerState,
    constant_buffer: ID3D11Buffer,
}

impl Renderer {
    /// Compiles shaders and instantiates GPU state resources for rendering.
    pub fn new(device: &ID3D11Device) -> Result<Self> {
        // 1. Compile Vertex Shader (vs_5_0)
        let mut vs_code = None;
        let mut vs_error = None;
        let vs_result = unsafe {
            D3DCompile(
                SHADER_SOURCE.as_ptr() as _,
                SHADER_SOURCE.len(),
                None,
                None,
                None,
                s!("vs_main"),
                s!("vs_5_0"),
                0,
                0,
                &mut vs_code,
                Some(&mut vs_error),
            )
        };

        vs_result?;

        let vs_code = vs_code.ok_or_else(|| Error::from_hresult(E_FAIL))?;
        let vs_bytecode = unsafe {
            std::slice::from_raw_parts(
                vs_code.GetBufferPointer() as *const u8,
                vs_code.GetBufferSize(),
            )
        };

        let mut vertex_shader = None;
        unsafe {
            device.CreateVertexShader(vs_bytecode, None, Some(&mut vertex_shader))?;
        }
        let vertex_shader = vertex_shader.ok_or_else(|| Error::from_hresult(E_FAIL))?;

        // 2. Compile Pixel Shader (ps_5_0)
        let mut ps_code = None;
        let mut ps_error = None;
        let ps_result = unsafe {
            D3DCompile(
                SHADER_SOURCE.as_ptr() as _,
                SHADER_SOURCE.len(),
                None,
                None,
                None,
                s!("ps_main"),
                s!("ps_5_0"),
                0,
                0,
                &mut ps_code,
                Some(&mut ps_error),
            )
        };

        ps_result?;

        let ps_code = ps_code.ok_or_else(|| Error::from_hresult(E_FAIL))?;
        let ps_bytecode = unsafe {
            std::slice::from_raw_parts(
                ps_code.GetBufferPointer() as *const u8,
                ps_code.GetBufferSize(),
            )
        };

        let mut pixel_shader = None;
        unsafe {
            device.CreatePixelShader(ps_bytecode, None, Some(&mut pixel_shader))?;
        }
        let pixel_shader = pixel_shader.ok_or_else(|| Error::from_hresult(E_FAIL))?;

        // 3. Create linear clamp texture sampler
        let sampler_desc = D3D11_SAMPLER_DESC {
            Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
            AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
            AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
            AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
            MipLODBias: 0.0,
            MaxAnisotropy: 1,
            ComparisonFunc: D3D11_COMPARISON_NEVER,
            BorderColor: [0.0; 4],
            MinLOD: 0.0,
            MaxLOD: D3D11_FLOAT32_MAX,
        };

        let mut sampler_state = None;
        unsafe {
            device.CreateSamplerState(&sampler_desc, Some(&mut sampler_state))?;
        }
        let sampler_state = sampler_state.ok_or_else(|| Error::from_hresult(E_FAIL))?;

        // 4. Create dynamic constant buffer for per-frame parameters
        let buffer_desc = D3D11_BUFFER_DESC {
            ByteWidth: std::mem::size_of::<RenderParams>() as u32,
            Usage: D3D11_USAGE_DYNAMIC,
            BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
            CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
            MiscFlags: 0,
            StructureByteStride: 0,
        };

        let mut constant_buffer = None;
        unsafe {
            device.CreateBuffer(&buffer_desc, None, Some(&mut constant_buffer))?;
        }
        let constant_buffer = constant_buffer.ok_or_else(|| Error::from_hresult(E_FAIL))?;

        Ok(Self {
            vertex_shader,
            pixel_shader,
            sampler_state,
            constant_buffer,
        })
    }

    /// Renders an NV12 texture frame directly to the presenter's render target.
    #[allow(clippy::too_many_arguments)]
    pub fn render_nv12_frame(
        &self,
        presenter: &Presenter,
        nv12_texture: &ID3D11Texture2D,
        subresource_index: u32,
        video_width: u32,
        video_height: u32,
        brightness: f32,
        fit_mode: ScreenFit,
    ) -> Result<()> {
        let rtv = presenter
            .render_target_view()
            .ok_or_else(|| Error::from_hresult(E_FAIL))?;

        if presenter.width == 0 || presenter.height == 0 || video_width == 0 || video_height == 0 {
            return Ok(());
        }

        // 1. Create Plane Y Shader Resource View (R8_UNORM) for subresource slice
        let srv_y_desc = D3D11_SHADER_RESOURCE_VIEW_DESC {
            Format: DXGI_FORMAT_R8_UNORM,
            ViewDimension: D3D_SRV_DIMENSION_TEXTURE2DARRAY,
            Anonymous: D3D11_SHADER_RESOURCE_VIEW_DESC_0 {
                Texture2DArray: D3D11_TEX2D_ARRAY_SRV {
                    MostDetailedMip: 0,
                    MipLevels: 1,
                    FirstArraySlice: subresource_index,
                    ArraySize: 1,
                },
            },
        };

        let mut srv_y = None;
        unsafe {
            presenter.device.CreateShaderResourceView(
                nv12_texture,
                Some(&srv_y_desc),
                Some(&mut srv_y),
            )?;
        }
        let srv_y = srv_y.ok_or_else(|| Error::from_hresult(E_FAIL))?;

        // 2. Create Plane UV Shader Resource View (R8G8_UNORM) for subresource slice
        let srv_uv_desc = D3D11_SHADER_RESOURCE_VIEW_DESC {
            Format: DXGI_FORMAT_R8G8_UNORM,
            ViewDimension: D3D_SRV_DIMENSION_TEXTURE2DARRAY,
            Anonymous: D3D11_SHADER_RESOURCE_VIEW_DESC_0 {
                Texture2DArray: D3D11_TEX2D_ARRAY_SRV {
                    MostDetailedMip: 0,
                    MipLevels: 1,
                    FirstArraySlice: subresource_index,
                    ArraySize: 1,
                },
            },
        };

        let mut srv_uv = None;
        unsafe {
            presenter.device.CreateShaderResourceView(
                nv12_texture,
                Some(&srv_uv_desc),
                Some(&mut srv_uv),
            )?;
        }
        let srv_uv = srv_uv.ok_or_else(|| Error::from_hresult(E_FAIL))?;

        // 3. Compute UV scaling and offsets for aspect-ratio preservation
        let (uv_scale, uv_offset) = match fit_mode {
            ScreenFit::Fill => ([1.0, 1.0], [0.0, 0.0]),
            ScreenFit::Cover => {
                let aspect_s = presenter.width as f32 / presenter.height as f32;
                let aspect_v = video_width as f32 / video_height as f32;
                if aspect_s > aspect_v {
                    let scale_y = aspect_v / aspect_s;
                    ([1.0, scale_y], [0.0, (1.0 - scale_y) * 0.5])
                } else {
                    let scale_x = aspect_s / aspect_v;
                    ([scale_x, 1.0], [(1.0 - scale_x) * 0.5, 0.0])
                }
            }
            ScreenFit::Contain => {
                let aspect_s = presenter.width as f32 / presenter.height as f32;
                let aspect_v = video_width as f32 / video_height as f32;
                if aspect_s > aspect_v {
                    let scale_x = aspect_s / aspect_v;
                    ([scale_x, 1.0], [(1.0 - scale_x) * 0.5, 0.0])
                } else {
                    let scale_y = aspect_v / aspect_s;
                    ([1.0, scale_y], [0.0, (1.0 - scale_y) * 0.5])
                }
            }
        };

        // 4. Update constant buffer (Dynamic Map/Unmap)
        let params = RenderParams {
            uv_scale,
            uv_offset,
            brightness,
            slice_index: 0,
            _padding: [0.0, 0.0],
        };

        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        unsafe {
            presenter.context.Map(
                &self.constant_buffer,
                0,
                D3D11_MAP_WRITE_DISCARD,
                0,
                Some(&mut mapped),
            )?;
            std::ptr::copy_nonoverlapping(
                &params as *const RenderParams as *const u8,
                mapped.pData as *mut u8,
                std::mem::size_of::<RenderParams>(),
            );
            presenter.context.Unmap(&self.constant_buffer, 0);
        }

        // 5. Setup Viewport and Pipeline bindings
        let viewport = D3D11_VIEWPORT {
            TopLeftX: 0.0,
            TopLeftY: 0.0,
            Width: presenter.width as f32,
            Height: presenter.height as f32,
            MinDepth: 0.0,
            MaxDepth: 1.0,
        };

        unsafe {
            presenter
                .context
                .OMSetRenderTargets(Some(&[Some(rtv.clone())]), None);
            presenter.context.RSSetViewports(Some(&[viewport]));
            presenter.context.VSSetShader(&self.vertex_shader, None);
            presenter.context.PSSetShader(&self.pixel_shader, None);
            presenter
                .context
                .PSSetConstantBuffers(0, Some(&[Some(self.constant_buffer.clone())]));
            presenter
                .context
                .PSSetSamplers(0, Some(&[Some(self.sampler_state.clone())]));
            presenter
                .context
                .PSSetShaderResources(0, Some(&[Some(srv_y), Some(srv_uv)]));

            // Draw fullscreen triangle
            presenter
                .context
                .IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            presenter.context.Draw(3, 0);

            // Unbind SRVs immediately so decoder surface reference is never held locked
            presenter
                .context
                .PSSetShaderResources(0, Some(&[None, None]));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shader_compilation() {
        let mut vs_code = None;
        let mut vs_error = None;
        let hr_vs = unsafe {
            D3DCompile(
                SHADER_SOURCE.as_ptr() as _,
                SHADER_SOURCE.len(),
                None,
                None,
                None,
                s!("vs_main"),
                s!("vs_5_0"),
                0,
                0,
                &mut vs_code,
                Some(&mut vs_error),
            )
        };
        assert!(hr_vs.is_ok(), "Vertex shader must compile cleanly");
        assert!(vs_code.is_some());

        let mut ps_code = None;
        let mut ps_error = None;
        let hr_ps = unsafe {
            D3DCompile(
                SHADER_SOURCE.as_ptr() as _,
                SHADER_SOURCE.len(),
                None,
                None,
                None,
                s!("ps_main"),
                s!("ps_5_0"),
                0,
                0,
                &mut ps_code,
                Some(&mut ps_error),
            )
        };
        assert!(hr_ps.is_ok(), "Pixel shader must compile cleanly");
        assert!(ps_code.is_some());
    }

    #[test]
    fn test_constant_buffer_alignment() {
        assert_eq!(
            std::mem::size_of::<RenderParams>() % 16,
            0,
            "Constant buffer struct must be 16-byte aligned"
        );
        assert_eq!(std::mem::size_of::<RenderParams>(), 32);
    }
}
