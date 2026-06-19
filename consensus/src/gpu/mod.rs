use bytemuck::{Pod, Zeroable};
use nusantara_crypto::Hash;
use parking_lot::Mutex;
use tracing::instrument;

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

/// Reusable GPU buffer pool to avoid per-call buffer allocation overhead.
///
/// Buffers are recreated only when the batch size exceeds the current capacity.
/// Capacity grows to the next power of two to amortize reallocations.
struct GpuBufferPool {
    entry: Option<wgpu::Buffer>,
    result: Option<wgpu::Buffer>,
    staging: Option<wgpu::Buffer>,
    capacity_entries: usize,
}

impl GpuBufferPool {
    fn new() -> Self {
        Self {
            entry: None,
            result: None,
            staging: None,
            capacity_entries: 0,
        }
    }

    /// Ensure the pool buffers can hold at least `needed` entries.
    /// Recreates all three buffers if current capacity is insufficient.
    fn ensure_capacity(&mut self, device: &wgpu::Device, needed: usize) {
        if self.capacity_entries >= needed {
            return;
        }
        // Grow to next power of two to amortize reallocations.
        let new_cap = needed.next_power_of_two();
        let entry_size = (new_cap * std::mem::size_of::<GpuPohEntry>()) as u64;
        let result_size = (new_cap * std::mem::size_of::<u32>()) as u64;

        self.entry = Some(device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Pool Entry Buffer"),
            size: entry_size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }));
        self.result = Some(device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Pool Result Buffer"),
            size: result_size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        }));
        self.staging = Some(device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Pool Staging Buffer"),
            size: result_size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }));
        self.capacity_entries = new_cap;
    }
}

pub struct GpuPohVerifier {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::ComputePipeline,
    /// Reusable buffer pool guarded by a Mutex.
    /// Not held across `.await` points — `verify_batch` is synchronous.
    pool: Mutex<GpuBufferPool>,
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

        let Ok(adapter) = adapter else {
            tracing::info!("No GPU adapter found, falling back to CPU verification");
            return Ok(None);
        };

        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("PoH Verifier"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
                memory_hints: wgpu::MemoryHints::Performance,
                trace: wgpu::Trace::Off,
            },
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
            pool: Mutex::new(GpuBufferPool::new()),
        }))
    }

    /// Batch verify PoH entries on GPU.
    /// Each entry is (initial_hash, num_hashes, expected_hash).
    ///
    /// Handles the device `max_compute_workgroups_per_dimension` limit by splitting
    /// large batches into chunks, each dispatched in a separate command encoder.
    /// Reuses pooled GPU buffers across calls to amortize allocation overhead.
    #[instrument(skip(self, entries), level = "debug")]
    pub fn verify_batch(&self, entries: &[(Hash, u64, Hash)]) -> Result<Vec<bool>, ConsensusError> {
        if entries.is_empty() {
            return Ok(Vec::new());
        }

        let max_dispatch = self.device.limits().max_compute_workgroups_per_dimension as usize;
        let mut all_results = vec![false; entries.len()];

        // Ensure pool buffers are large enough for the full batch before chunking.
        {
            let mut pool = self.pool.lock();
            pool.ensure_capacity(&self.device, entries.len());
        }

        // Process in chunks no larger than the device's workgroup limit.
        let mut offset = 0usize;
        for chunk in entries.chunks(max_dispatch) {
            let chunk_results = self.verify_chunk(chunk)?;
            all_results[offset..offset + chunk.len()].copy_from_slice(&chunk_results);
            offset += chunk.len();
        }

        metrics::counter!("nusantara_gpu_poh_entries_verified_total")
            .increment(entries.len() as u64);

        Ok(all_results)
    }

    /// Verify a single chunk of entries (fits within device workgroup limits).
    /// Writes GPU entry data into the pooled buffers and reads back results.
    fn verify_chunk(&self, entries: &[(Hash, u64, Hash)]) -> Result<Vec<bool>, ConsensusError> {
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

        let entry_data = bytemuck::cast_slice(&gpu_entries);
        let result_size = (entries.len() * std::mem::size_of::<u32>()) as u64;

        // Lock the pool for the duration of this synchronous GPU dispatch.
        // Not held across .await points — verify_batch is called via block_in_place.
        let pool = self.pool.lock();

        let entry_buffer = pool.entry.as_ref().expect("pool initialized by verify_batch");
        let result_buffer = pool.result.as_ref().expect("pool initialized by verify_batch");
        let staging_buffer = pool.staging.as_ref().expect("pool initialized by verify_batch");

        // Upload entry data into the pooled entry buffer.
        self.queue.write_buffer(entry_buffer, 0, entry_data);

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

        // Copy only the portion of result_buffer used by this chunk.
        encoder.copy_buffer_to_buffer(result_buffer, 0, staging_buffer, 0, result_size);
        self.queue.submit(Some(encoder.finish()));

        let buffer_slice = staging_buffer.slice(..result_size);
        let (sender, receiver) = std::sync::mpsc::channel();
        buffer_slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());

        receiver
            .recv()
            .map_err(|e| ConsensusError::Gpu(e.to_string()))?
            .map_err(|e| ConsensusError::Gpu(e.to_string()))?;

        let data = buffer_slice.get_mapped_range();
        let results: &[u32] = bytemuck::cast_slice(&data);
        let bool_results: Vec<bool> = results.iter().map(|&r| r == 1).collect();

        // Drop the mapped range before unmapping to prevent use-after-unmap.
        drop(data);
        staging_buffer.unmap();

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
