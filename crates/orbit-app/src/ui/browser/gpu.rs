use iced::{Rectangle, widget::shader};
use iced_wgpu::wgpu::{self, *};
use orbit_core::browser::Frame;
#[derive(Debug)]
pub(super) struct Pixels(pub Frame);
pub struct Pipeline {
    pipeline: RenderPipeline,
    surface: Option<(Texture, BindGroup)>,
    uploaded: Option<(u64, u32)>,
}

impl shader::Pipeline for Pipeline {
    fn new(device: &Device, _: &Queue, format: TextureFormat) -> Self {
        let shader = device.create_shader_module(ShaderModuleDescriptor {
            label: Some("CEF sidebar"),
            source: ShaderSource::Wgsl(
                r#"
struct Vertex { @builtin(position) pos:vec4f, @location(0) uv:vec2f }
@vertex fn vs(@builtin(vertex_index) i:u32)->Vertex {
    let uv=vec2f(f32((i << 1u) & 2u),f32(i & 2u));
    return Vertex(vec4f(uv * vec2f(2,-2) + vec2f(-1,1),0,1),uv);
}
@group(0) @binding(0) var img:texture_2d<f32>;
@fragment fn fs(v:Vertex)->@location(0) vec4f {
    let size=vec2i(textureDimensions(img));
    return textureLoad(img, clamp(vec2i(v.uv*vec2f(size)), vec2i(0),size-1),0);
}"#
                .into(),
            ),
        });
        let pipeline = device.create_render_pipeline(&RenderPipelineDescriptor {
            label: Some("CEF sidebar"),
            layout: None,
            vertex: VertexState {
                module: &shader,
                entry_point: Some("vs"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                targets: &[Some(format.into())],
                compilation_options: Default::default(),
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview: None,
            cache: None,
        });
        Self {
            pipeline,
            surface: None,
            uploaded: None,
        }
    }
}

impl shader::Primitive for Pixels {
    type Pipeline = Pipeline;
    fn prepare(
        &self,
        p: &mut Pipeline,
        device: &Device,
        queue: &Queue,
        _: &Rectangle,
        _: &shader::Viewport,
    ) {
        if p.uploaded == Some(self.0.key) {
            return;
        }
        let size = Extent3d {
            width: self.0.width,
            height: self.0.height,
            depth_or_array_layers: 1,
        };
        if p.surface.as_ref().is_none_or(|(t, _)| t.size() != size) {
            let texture = device.create_texture(&TextureDescriptor {
                label: Some("CEF pixels"),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: TextureDimension::D2,
                format: TextureFormat::Bgra8UnormSrgb,
                usage: TextureUsages::COPY_DST | TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            let group = device.create_bind_group(&BindGroupDescriptor {
                label: None,
                layout: &p.pipeline.get_bind_group_layout(0),
                entries: &[BindGroupEntry {
                    binding: 0,
                    resource: BindingResource::TextureView(
                        &texture.create_view(&Default::default()),
                    ),
                }],
            });
            p.surface = Some((texture, group));
        }
        let texture = &p.surface.as_ref().unwrap().0;
        queue.write_texture(
            texture.as_image_copy(),
            &self.0.bgra,
            TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(self.0.width * 4),
                rows_per_image: None,
            },
            size,
        );
        p.uploaded = Some(self.0.key);
    }
    fn draw(&self, p: &Pipeline, pass: &mut wgpu::RenderPass<'_>) -> bool {
        pass.set_pipeline(&p.pipeline);
        pass.set_bind_group(0, &p.surface.as_ref().unwrap().1, &[]);
        pass.draw(0..3, 0..1);
        true
    }
}
