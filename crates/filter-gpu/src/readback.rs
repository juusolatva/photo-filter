//! Double-buffered asynchronous readback shared by bounded GPU measurements.
//!
//! Callers record their GPU work into a free fixed-size buffer and later
//! decode completed maps after the app's ordinary nonblocking device poll.
//! Nothing here polls or waits. Each job carries a caller-defined tag (for
//! example a generation) so superseded results can be rejected.
use anyhow::{Context, Result, anyhow};
use std::sync::mpsc::{self, Receiver, TryRecvError};

type MapResult = Result<(), wgpu::BufferAsyncError>;

struct Pending<T> {
    tag: T,
    ready: Receiver<MapResult>,
}

struct Slot<T> {
    buffer: wgpu::Buffer,
    pending: Option<Pending<T>>,
    replace_on_submit: bool,
}

pub(crate) struct Readback<T> {
    name: &'static str,
    size: u64,
    slots: [Slot<T>; 2],
}

impl<T> Readback<T> {
    /// `name` prefixes buffer labels and mapping errors, e.g. "Histogram".
    pub fn new(device: &wgpu::Device, name: &'static str, size: u64) -> Self {
        Self {
            name,
            size,
            slots: std::array::from_fn(|_| Slot {
                buffer: Self::buffer(device, name, size),
                pending: None,
                replace_on_submit: false,
            }),
        }
    }

    fn buffer(device: &wgpu::Device, name: &str, size: u64) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(&format!("{name} readback")),
            size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        })
    }

    pub fn is_pending(&self) -> bool {
        self.slots.iter().any(|s| s.pending.is_some())
    }

    /// Calls `record` with a free readback buffer of exactly `size` bytes; it
    /// must submit work that copies the result into that buffer. Mapping then
    /// starts asynchronously. Returns false without calling `record` when both
    /// slots are pending, leaving the caller free to coalesce requests.
    pub fn submit(
        &mut self,
        device: &wgpu::Device,
        tag: T,
        record: impl FnOnce(&wgpu::Buffer),
    ) -> bool {
        let (name, size) = (self.name, self.size);
        let Some(slot) = self.slots.iter_mut().find(|s| s.pending.is_none()) else {
            return false;
        };
        if slot.replace_on_submit {
            // A failed/disconnected map has no usable mapped view. Retire that
            // buffer rather than unmapping an idle buffer or reusing map state.
            slot.buffer = Self::buffer(device, name, size);
            slot.replace_on_submit = false;
        }
        record(&slot.buffer);
        let (tx, ready) = mpsc::sync_channel(1);
        slot.buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = tx.send(result);
            });
        slot.pending = Some(Pending { tag, ready });
        true
    }

    /// Decodes completed maps with `read`, which receives exactly `size`
    /// mapped bytes. Only a successfully mapped buffer is unmapped: unmap() on
    /// an idle buffer is a wgpu validation error, and a disconnected callback
    /// leaves map state uncertain, so both retire the buffer on next submit.
    pub fn collect<R>(
        &mut self,
        mut read: impl FnMut(&[u8], &T) -> Result<R>,
    ) -> Vec<(T, Result<R>)> {
        let name = self.name;
        let mut results = Vec::new();
        for slot in &mut self.slots {
            let Some(pending) = &slot.pending else {
                continue;
            };
            let mapping = match pending.ready.try_recv() {
                Err(TryRecvError::Empty) => continue,
                Err(TryRecvError::Disconnected) => {
                    Err(anyhow!("{name} mapping callback disconnected"))
                }
                Ok(result) => result.with_context(|| format!("{name} mapping failed")),
            };
            let Some(Pending { tag, .. }) = slot.pending.take() else {
                unreachable!("pending slot checked above");
            };
            let mapped = mapping.is_ok();
            let result = mapping.and_then(|()| {
                let bytes = slot.buffer.slice(..).get_mapped_range()?;
                read(&bytes, &tag)
            });
            if mapped {
                slot.buffer.unmap();
            } else {
                slot.replace_on_submit = true;
            }
            results.push((tag, result));
        }
        results
    }
}

#[cfg(test)]
impl<T> Readback<T> {
    /// Fault injection: occupy the first slot with a job whose map callback
    /// reports failure (or is dropped), without exhausting memory or losing
    /// the test device. Returns the buffer that should be retired.
    pub fn inject_failed_map(&mut self, tag: T, disconnected: bool) -> wgpu::Buffer {
        let (tx, ready) = mpsc::sync_channel(1);
        if !disconnected {
            tx.send(Err(wgpu::BufferAsyncError)).unwrap();
        }
        drop(tx);
        let slot = &mut self.slots[0];
        slot.pending = Some(Pending { tag, ready });
        slot.buffer.clone()
    }

    /// The buffer the next submission uses when both slots are idle.
    pub fn first_buffer(&self) -> &wgpu::Buffer {
        &self.slots[0].buffer
    }
}
