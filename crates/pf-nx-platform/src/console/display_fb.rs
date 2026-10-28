//! The `Display` on libnx's linear framebuffer: Skia rasters the UI on the CPU and each frame is
//! copied into the next buffer. No stream layer. The probe and the emulator run on this, since
//! stock emulators do not run NVK (plan D15).

use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use pf_nx_app::platform::{Display, Fit, FrameReport, OperationMode, Refresh, Rgba};
use pf_nx_sys::{fb, vi};
use skia_safe::{AlphaType, Color, ColorType, ImageInfo};

/// libnx paces the framebuffer at the panel's 60 Hz.
const REFRESH: Refresh = Refresh(60_000);

pub struct FbDisplay {
    fb: fb::Framebuffer,
    /// RGBA8888, the framebuffer's own format, so a frame is a row copy.
    surface: skia_safe::Surface,
    size: (u32, u32),
    last_present: Option<Instant>,
    stream_refused: bool,
}

impl FbDisplay {
    pub fn new() -> Result<Self> {
        let size = match vi::window_size(vi::default_window()) {
            (0, _) | (_, 0) => (1280, 720),
            size => size,
        };
        let fb = fb::Framebuffer::create(size.0, size.1)?;
        let info = ImageInfo::new(
            (size.0 as i32, size.1 as i32),
            ColorType::RGBA8888,
            AlphaType::Premul,
            None,
        );
        let surface = skia_safe::surfaces::raster(&info, None, None).ok_or_else(|| anyhow!("raster UI surface"))?;
        Ok(Self {
            fb,
            surface,
            size,
            last_present: None,
            stream_refused: false,
        })
    }

    /// How the UI draws, for the startup log.
    pub fn describe(&self) -> String {
        format!("CPU raster {}x{} on the framebuffer", self.size.0, self.size.1)
    }
}

impl Display for FbDisplay {
    fn size(&self) -> (u32, u32) {
        self.size
    }

    fn refresh(&self) -> Refresh {
        REFRESH
    }

    fn mode(&self) -> OperationMode {
        super::operation_mode()
    }

    fn frame(&mut self, ui: &mut dyn FnMut(&skia_safe::Canvas)) -> Result<FrameReport> {
        let canvas = self.surface.canvas();
        canvas.clear(Color::BLACK);
        ui(canvas);
        let pixmap = self.surface.peek_pixels().context("raster pixels")?;
        let src = pixmap.bytes().context("raster bytes")?;
        let (row, src_stride) = (self.size.0 as usize * 4, pixmap.row_bytes());
        let (dst, dst_stride) = self.fb.begin();
        for y in 0..self.size.1 as usize {
            dst[y * dst_stride..y * dst_stride + row].copy_from_slice(&src[y * src_stride..y * src_stride + row]);
        }
        self.fb.end();
        let now = Instant::now();
        let period = Duration::from_nanos(REFRESH.period_ns());
        let report = FrameReport {
            missed: self.last_present.is_some_and(|t| now - t > period + period / 2),
            ..FrameReport::default()
        };
        self.last_present = Some(now);
        Ok(report)
    }

    fn set_stream(&mut self, on: bool, _fit: Fit) {
        if on && !std::mem::replace(&mut self.stream_refused, true) {
            tracing::warn!("display: the framebuffer shows no stream; pictures are decoded and dropped");
        }
    }

    fn read_back(&mut self) -> Result<Option<Rgba>> {
        let pixmap = self.surface.peek_pixels().context("raster pixels")?;
        let src = pixmap.bytes().context("raster bytes")?;
        let row = self.size.0 as usize * 4;
        let pixels = (0..self.size.1 as usize)
            .flat_map(|y| &src[y * pixmap.row_bytes()..y * pixmap.row_bytes() + row])
            .copied()
            .collect();
        Ok(Some(Rgba {
            width: self.size.0,
            height: self.size.1,
            pixels,
        }))
    }
}
