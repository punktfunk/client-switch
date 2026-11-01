//! How long a command buffer ran on the GPU, from a timestamp at its start and one at its end:
//! the margin the GPU clock leaves (plan WP5). One pair per command buffer slot, read once that
//! slot's fence has been waited, so a read never stalls. A queue without timestamps reads
//! nothing and records nothing.

use ash::vk;

use crate::device::GpuDevice;

pub(crate) struct GpuTimer {
    pool: vk::QueryPool,
    /// Nanoseconds per tick.
    period_ns: f64,
    /// The bits a timestamp carries on this queue.
    mask: u64,
    /// Whether each slot's pair has been recorded since the pool was made.
    recorded: Vec<bool>,
}

impl GpuTimer {
    /// Timestamps for `slots` command buffers, or `None` where the queue has none.
    pub fn new(dev: &GpuDevice, slots: usize) -> Option<Self> {
        // SAFETY: read-only queries on the live physical device.
        let (limits, families) = unsafe {
            (
                dev.instance.get_physical_device_properties(dev.pdev).limits,
                dev.instance.get_physical_device_queue_family_properties(dev.pdev),
            )
        };
        let bits = families.get(dev.qf as usize)?.timestamp_valid_bits;
        if bits == 0 || limits.timestamp_period <= 0.0 {
            return None;
        }
        // SAFETY: the create-info is a local; the pool is destroyed in `destroy`.
        let pool = unsafe {
            dev.device.create_query_pool(
                &vk::QueryPoolCreateInfo::default()
                    .query_type(vk::QueryType::TIMESTAMP)
                    .query_count(slots as u32 * 2),
                None,
            )
        }
        .ok()?;
        Some(Self {
            pool,
            period_ns: f64::from(limits.timestamp_period),
            mask: if bits >= 64 { u64::MAX } else { (1 << bits) - 1 },
            recorded: vec![false; slots],
        })
    }

    /// Slot `i`'s last run in µs. Call only after its fence was waited.
    pub fn read(&self, dev: &GpuDevice, i: usize) -> Option<u32> {
        if !self.recorded[i] {
            return None;
        }
        let mut ticks = [0u64; 2];
        // SAFETY: both queries were written by a submission whose fence the caller waited, so
        // the results are available without waiting; `ticks` holds exactly two.
        unsafe {
            dev.device
                .get_query_pool_results(self.pool, i as u32 * 2, &mut ticks, vk::QueryResultFlags::TYPE_64)
        }
        .ok()?;
        let ticks = ticks[1].wrapping_sub(ticks[0]) & self.mask;
        u32::try_from((ticks as f64 * self.period_ns / 1000.0) as u64).ok()
    }

    /// Records slot `i`'s start. Outside any render pass, first in the command buffer.
    ///
    /// # Safety
    /// `cmd` is recording and is slot `i`'s, whose last submission has finished.
    pub unsafe fn begin(&mut self, dev: &GpuDevice, cmd: vk::CommandBuffer, i: usize) {
        let q = i as u32 * 2;
        // SAFETY: per the contract above.
        unsafe {
            dev.device.cmd_reset_query_pool(cmd, self.pool, q, 2);
            dev.device
                .cmd_write_timestamp(cmd, vk::PipelineStageFlags::TOP_OF_PIPE, self.pool, q);
        }
        self.recorded[i] = true;
    }

    /// Records slot `i`'s end, after everything else the command buffer does.
    ///
    /// # Safety
    /// As [`Self::begin`], which was recorded into the same `cmd`.
    pub unsafe fn end(&self, dev: &GpuDevice, cmd: vk::CommandBuffer, i: usize) {
        // SAFETY: per the contract above.
        unsafe {
            dev.device
                .cmd_write_timestamp(cmd, vk::PipelineStageFlags::BOTTOM_OF_PIPE, self.pool, i as u32 * 2 + 1);
        }
    }

    /// # Safety
    /// No submission that writes the pool is in flight.
    pub unsafe fn destroy(&self, dev: &GpuDevice) {
        // SAFETY: per the contract above.
        unsafe { dev.device.destroy_query_pool(self.pool, None) };
    }
}
