//! Opt-in software/ConPTY timing. No message text is recorded.
use std::{
    fs::File,
    io::{self, BufWriter, Write},
};

use serde::Serialize;

#[derive(Default, Serialize)]
struct FrameSample {
    frame_id: u64,
    event_cursor: u64,
    revision_received_qpc: Option<u64>,
    draw_start_qpc: u64,
    layout_end_qpc: u64,
    first_write_qpc: Option<u64>,
    last_write_qpc: Option<u64>,
    flush_end_qpc: Option<u64>,
    draw_end_qpc: u64,
    marker_flush_qpc: u64,
    bytes: usize,
    writes: usize,
}

pub(super) struct DiagnosticWriter {
    stdout: io::Stdout,
    log: Option<BufWriter<File>>,
    frame: Option<FrameSample>,
    next_frame: u64,
    revision: Option<(u64, u64)>,
}

impl DiagnosticWriter {
    pub(super) fn new() -> io::Result<Self> {
        let log = std::env::var_os("PROTIUM_TUI_FRAME_TRACE")
            .map(|path| File::create_new(path).map(BufWriter::new))
            .transpose()?;
        let mut writer = Self {
            stdout: io::stdout(),
            log,
            frame: None,
            next_frame: 0,
            revision: None,
        };
        if let Some(log) = writer.log.as_mut() {
            writeln!(
                log,
                "{{\"clock\":\"{}\",\"frequency\":{},\"max_frames\":100000}}",
                if cfg!(windows) { "QPC" } else { "monotonic_ns" },
                frequency()
            )?;
            log.flush()?;
        }
        Ok(writer)
    }

    pub(super) fn revision_received(&mut self, cursor: u64) {
        if self.log.is_some() {
            self.revision = Some((cursor, tick()));
        }
    }

    pub(super) fn begin_frame(&mut self, cursor: u64) {
        if self.log.is_none() || self.next_frame >= 100_000 {
            return;
        }
        self.next_frame += 1;
        self.frame = Some(FrameSample {
            frame_id: self.next_frame,
            event_cursor: cursor,
            revision_received_qpc: self
                .revision
                .filter(|(seen, _)| *seen == cursor)
                .map(|(_, qpc)| qpc),
            draw_start_qpc: tick(),
            ..FrameSample::default()
        });
    }

    pub(super) fn finish_frame(&mut self, layout_end: u64) -> io::Result<()> {
        let Some(mut frame) = self.frame.take() else {
            return Ok(());
        };
        frame.layout_end_qpc = layout_end;
        frame.draw_end_qpc = tick();
        // OSC title is transported by ConPTY. It is an arrival marker, not a
        // guarantee that a terminal renderer has presented the screen.
        write!(self.stdout, "\x1b]0;PROTIUM_FRAME_{}\x07", frame.frame_id)?;
        self.stdout.flush()?;
        frame.marker_flush_qpc = tick();
        if let Some(log) = self.log.as_mut() {
            serde_json::to_writer(&mut *log, &frame)?;
            writeln!(log)?;
            log.flush()?;
        }
        Ok(())
    }

    pub(super) fn enabled(&self) -> bool {
        self.log.is_some()
    }
}

impl Write for DiagnosticWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let start = self.frame.as_ref().map(|_| tick());
        let written = self.stdout.write(bytes)?;
        if let Some(frame) = self.frame.as_mut() {
            frame.first_write_qpc = frame.first_write_qpc.or(start);
            frame.last_write_qpc = Some(tick());
            frame.bytes += written;
            frame.writes += 1;
        }
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stdout.flush()?;
        if let Some(frame) = self.frame.as_mut() {
            frame.flush_end_qpc = Some(tick());
        }
        Ok(())
    }
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn QueryPerformanceCounter(value: *mut i64) -> i32;
    fn QueryPerformanceFrequency(value: *mut i64) -> i32;
}

pub(super) fn tick() -> u64 {
    #[cfg(windows)]
    {
        let mut value = 0;
        // SAFETY: Windows writes one i64 to this valid stack pointer.
        unsafe {
            QueryPerformanceCounter(&mut value);
        }
        value as u64
    }
    #[cfg(not(windows))]
    {
        static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
        START
            .get_or_init(std::time::Instant::now)
            .elapsed()
            .as_nanos() as u64
    }
}

fn frequency() -> u64 {
    #[cfg(windows)]
    {
        let mut value = 0;
        // SAFETY: Windows writes one i64 to this valid stack pointer.
        unsafe {
            QueryPerformanceFrequency(&mut value);
        }
        value as u64
    }
    #[cfg(not(windows))]
    {
        1_000_000_000
    }
}
