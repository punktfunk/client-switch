//! Check 6, `nvdec`: the elementary streams `scripts/nvdec-streams.sh` writes (1080p60, no
//! B-frames, one IDR, an access unit delimiter before every AU) read from the data folder and
//! decoded through pf-nx-sys's `nvtegra` decoder, one AU at a time as the session will send them.
//! Each stream reports its pictures, the first picture's latency, fps, decode and VIC transfer
//! times, frames in flight and the first picture's mean luma. 10-bit is reported, not required.
//! `probe 6 nvdec at` lines mark the way through: the first console run stopped in here unlogged.

use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use pf_nx_sys::av::{Codec, Decoder, SendStatus};

use crate::{err, Out, Steps};

const WIDTH: usize = 1920;
/// The decoder may pad 1080 to the macroblock: room for 1088 rows.
const ROWS: usize = 1088;

/// Step, file, codec, whether the check needs it.
const STREAMS: [(&str, &str, Codec, bool); 3] = [
    ("h264", "nvdec-h264.264", Codec::H264, true),
    ("hevc", "nvdec-hevc.265", Codec::Hevc, true),
    ("hevc10", "nvdec-hevc10.265", Codec::Hevc, false),
];

pub fn steps(dir: &Path, out: &mut Out) -> Steps {
    let mut steps = Steps::new();
    for (step, file, codec, needed) in STREAMS {
        out.line(&format!("probe 6 nvdec at {step} read"));
        let result = std::fs::read(dir.join(file))
            .map_err(|e| format!("{file}: {e}"))
            .and_then(|data| decode(step, codec, &data, out));
        steps.push((
            step,
            result.or_else(|e| if needed { Err(e) } else { Ok(format!("no({e})")) }),
        ));
    }
    steps
}

/// The AUs of an Annex B stream, each starting at its delimiter's start code.
fn access_units(data: &[u8], codec: Codec) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    for i in 0..data.len().saturating_sub(3) {
        if data[i..i + 3] != [0, 0, 1] {
            continue;
        }
        let header = data[i + 3];
        let aud = match codec {
            Codec::H264 => header & 0x1f == 9,
            Codec::Hevc => (header >> 1) & 0x3f == 35,
        };
        if aud {
            starts.push(if i > 0 && data[i - 1] == 0 { i - 1 } else { i });
        }
    }
    starts
        .iter()
        .enumerate()
        .map(|(n, &s)| &data[s..starts.get(n + 1).copied().unwrap_or(data.len())])
        .collect()
}

#[derive(Default)]
struct Times {
    sum: u64,
    max: u32,
}

impl Times {
    fn add(&mut self, us: u32) {
        self.sum += u64::from(us);
        self.max = self.max.max(us);
    }

    fn show(&self, n: u64) -> String {
        format!("{}/{}us", self.sum / n.max(1), self.max)
    }
}

fn decode(step: &str, codec: Codec, data: &[u8], out: &mut Out) -> Result<String, String> {
    let aus = access_units(data, codec);
    if aus.is_empty() {
        return Err("no access unit delimiters".into());
    }
    out.line(&format!(
        "probe 6 nvdec at {step} open {}B aus={}",
        data.len(),
        aus.len()
    ));
    let opened = Instant::now();
    let mut dec = Decoder::open(codec, 1920, 1080, false).map_err(err)?;
    let open_ms = opened.elapsed().as_millis();
    out.line(&format!("probe 6 nvdec at {step} send open={open_ms}ms"));

    let (mut y, mut uv) = (vec![0u8; WIDTH * ROWS], vec![0u8; WIDTH * ROWS / 2]);
    let (mut decode_us, mut transfer_us) = (Times::default(), Times::default());
    let (mut pictures, mut in_flight, mut size, mut first, mut luma) = (0u64, 0u32, (0, 0), None, 0);
    let start = Instant::now();
    let mut receive = |dec: &mut Decoder| -> Result<bool, String> {
        let Some(meta) = dec.receive_into(&mut y, WIDTH, &mut uv, WIDTH).map_err(err)? else {
            return Ok(false);
        };
        let s = dec.stats();
        decode_us.add(s.last_decode_us);
        transfer_us.add(s.last_transfer_us);
        if first.is_none() {
            first = Some(start.elapsed());
            size = (meta.width, meta.height);
            let n = meta.width as usize * meta.height as usize;
            luma = y[..n].iter().map(|&p| u64::from(p)).sum::<u64>() / n.max(1) as u64;
        }
        pictures += 1;
        Ok(true)
    };

    for (pts, au) in aus.iter().enumerate() {
        // After the first AU, then once a second of stream.
        if pts == 1 || (pts > 0 && pts % 60 == 0) {
            out.line(&format!(
                "probe 6 nvdec at {step} au={pts} pictures={} {}ms",
                dec.stats().decoded,
                start.elapsed().as_millis()
            ));
        }
        let mut full = 0;
        while dec.send(au, pts as u64).map_err(err)? == SendStatus::Again {
            if !receive(&mut dec)? {
                full += 1;
                if full > 1000 {
                    return Err(format!("the decoder stayed full at AU {pts}"));
                }
                thread::sleep(Duration::from_millis(1));
            }
        }
        in_flight = in_flight.max(dec.stats().in_flight);
        while receive(&mut dec)? {}
    }
    // Pictures still in the decoder: up to 200 ms for them, as the shim has no drain.
    let drain = Instant::now();
    while dec.stats().in_flight > 0 && drain.elapsed() < Duration::from_millis(200) {
        if !receive(&mut dec)? {
            thread::sleep(Duration::from_millis(1));
        }
    }
    let secs = start.elapsed().as_secs_f64();

    let report = format!(
        "{}x{} aus={} pictures={pictures} open={open_ms}ms first={}ms fps={:.1} decode={} transfer={} \
         in_flight={in_flight} luma={luma}",
        size.0,
        size.1,
        aus.len(),
        first.map_or(0, |f| f.as_millis()),
        pictures as f64 / secs,
        decode_us.show(pictures),
        transfer_us.show(pictures),
    );
    if pictures == aus.len() as u64 {
        Ok(report)
    } else {
        Err(report)
    }
}
