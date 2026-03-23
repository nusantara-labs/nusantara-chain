use bytemuck::{Pod, Zeroable};
use nusantara_crypto::Hash;
use tracing::instrument;
use wgpu::util::DeviceExt;

use crate::error::ConsensusError;

/// Each GPU entry: initial_hash(64) + num_hashes(8) + expected_hash(64) = 136 bytes
/// Represented as 16 u32s + 2 u32s + 16 u32s = 34 u32s
#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
struct GpuPohEntry {
    initial_hash: [u32; 16],
    num_hashes_lo: u32,
    num_hashes_hi: u32,
    expected_hash: [u32; 16],
}

pub struct GpuPohVerifier {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::ComputePipeline,
}

impl GpuPohVerifier {
    /// Initialize GPU verifier. Returns None if no GPU is available.
    #[instrument(level = "info")]
    pub fn new() -> Result<Option<Self>, ConsensusError> {
        let instance = wgpu::Instance::default();

        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        }));

        let Some(adapter) = adapter else {
            tracing::info!("No GPU adapter found, falling back to CPU verification");
            return Ok(None);
        };

        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("PoH Verifier"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::Performance,
            },
            None,
        ))
        .map_err(|e| ConsensusError::Gpu(e.to_string()))?;

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("PoH Verify Shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("poh_verify.wgsl").into()),
        });

        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("PoH Verify Pipeline"),
            layout: None,
            module: &shader,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });

        tracing::info!(
            adapter = adapter.get_info().name,
            "GPU PoH verifier initialized"
        );
        metrics::counter!("nusantara_gpu_verifier_initialized_total").increment(1);

        Ok(Some(Self {
            device,
            queue,
            pipeline,
        }))
    }

    pub fn is_available(&self) -> bool {
        true
    }

    /// Batch verify PoH entries on GPU.
    /// Each entry is (initial_hash, num_hashes, expected_hash).
    #[instrument(skip(self, entries), level = "debug")]
    pub fn verify_batch(&self, entries: &[(Hash, u64, Hash)]) -> Result<Vec<bool>, ConsensusError> {
        if entries.is_empty() {
            return Ok(Vec::new());
        }

        let gpu_entries: Vec<GpuPohEntry> = entries
            .iter()
            .map(|(initial, num_hashes, expected)| {
                let mut initial_hash = [0u32; 16];
                let mut expected_hash = [0u32; 16];

                for (i, chunk) in initial.as_bytes().chunks(4).enumerate() {
                    initial_hash[i] = u32::from_le_bytes(chunk.try_into().unwrap_or([0; 4]));
                }
                for (i, chunk) in expected.as_bytes().chunks(4).enumerate() {
                    expected_hash[i] = u32::from_le_bytes(chunk.try_into().unwrap_or([0; 4]));
                }

                GpuPohEntry {
                    initial_hash,
                    num_hashes_lo: *num_hashes as u32,
                    num_hashes_hi: (*num_hashes >> 32) as u32,
                    expected_hash,
                }
            })
            .collect();

        let entry_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Entry Buffer"),
                contents: bytemuck::cast_slice(&gpu_entries),
                usage: wgpu::BufferUsages::STORAGE,
            });

        let result_size = (entries.len() * std::mem::size_of::<u32>()) as u64;
        let result_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Result Buffer"),
            size: result_size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });

        let staging_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Staging Buffer"),
            size: result_size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group_layout = self.pipeline.get_bind_group_layout(0);
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("PoH Bind Group"),
            layout: &bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: entry_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: result_buffer.as_entire_binding(),
                },
            ],
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("PoH Verify Encoder"),
            });

        {
            let mut compute_pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("PoH Verify Pass"),
                timestamp_writes: None,
            });
            compute_pass.set_pipeline(&self.pipeline);
            compute_pass.set_bind_group(0, &bind_group, &[]);
            compute_pass.dispatch_workgroups(entries.len() as u32, 1, 1);
        }

        encoder.copy_buffer_to_buffer(&result_buffer, 0, &staging_buffer, 0, result_size);
        self.queue.submit(Some(encoder.finish()));

        let buffer_slice = staging_buffer.slice(..);
        let (sender, receiver) = std::sync::mpsc::channel();
        buffer_slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
        self.device.poll(wgpu::Maintain::Wait);

        receiver
            .recv()
            .map_err(|e| ConsensusError::Gpu(e.to_string()))?
            .map_err(|e| ConsensusError::Gpu(e.to_string()))?;

        let data = buffer_slice.get_mapped_range();
        let results: &[u32] = bytemuck::cast_slice(&data);
        let bool_results: Vec<bool> = results.iter().map(|&r| r == 1).collect();

        metrics::counter!("nusantara_gpu_poh_entries_verified_total")
            .increment(entries.len() as u64);

        Ok(bool_results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpu_entry_size() {
        assert_eq!(std::mem::size_of::<GpuPohEntry>(), 136);
    }

    #[test]
    fn gpu_init_graceful() {
        // Should not panic even without GPU
        let result = GpuPohVerifier::new();
        assert!(result.is_ok());
        // Result may be None if no GPU
    }
}
