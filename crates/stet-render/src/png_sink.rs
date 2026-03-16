// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! PNG output sink — streams rendered RGBA rows to a PNG file.

use std::io::Write;

use stet_graphics::device::{PageSink, PageSinkFactory};

/// Factory that creates `PngSink` instances for each page.
pub struct PngSinkFactory;

impl PageSinkFactory for PngSinkFactory {
    fn create_sink(&self, output_path: &str) -> Result<Box<dyn PageSink>, String> {
        Ok(Box::new(PngSink {
            output_path: output_path.to_string(),
            writer: None,
        }))
    }
}

/// Streams RGBA rows to a PNG file.
struct PngSink {
    output_path: String,
    writer: Option<PngStreamWriter>,
}

/// Wraps the png crate's streaming writer.
struct PngStreamWriter {
    stream: png::StreamWriter<'static, std::io::BufWriter<std::fs::File>>,
    // Keep the Writer alive — StreamWriter borrows from it.
    // We use `Box::leak` to give the Writer a 'static lifetime,
    // then reclaim it in `finish()`.
    _writer_box: *mut png::Writer<std::io::BufWriter<std::fs::File>>,
}

// Safety: PngStreamWriter is only used from a single thread at a time.
unsafe impl Send for PngStreamWriter {}

impl PngStreamWriter {
    fn new(output_path: &str, width: u32, height: u32, band_h: u32) -> Result<Self, String> {
        let file = std::fs::File::create(output_path)
            .map_err(|e| format!("Failed to create PNG '{}': {}", output_path, e))?;
        let buf_writer = std::io::BufWriter::with_capacity(256 * 1024, file);

        let mut encoder = png::Encoder::new(buf_writer, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::Default);
        encoder.set_adaptive_filter(png::AdaptiveFilterType::Adaptive);

        let writer = encoder
            .write_header()
            .map_err(|e| format!("Failed to write PNG header '{}': {}", output_path, e))?;

        // Leak the Writer to get a 'static reference for StreamWriter.
        let writer_box = Box::into_raw(Box::new(writer));
        let stream = unsafe { &mut *writer_box }
            .stream_writer_with_size(band_h as usize * width as usize * 4)
            .map_err(|e| format!("Failed to create PNG stream '{}': {}", output_path, e))?;

        Ok(Self {
            stream,
            _writer_box: writer_box,
        })
    }

    fn write_all(&mut self, data: &[u8]) -> Result<(), String> {
        self.stream
            .write_all(data)
            .map_err(|e| format!("Failed to write PNG data: {}", e))
    }

    fn finish(self) -> Result<(), String> {
        self.stream
            .finish()
            .map_err(|e| format!("Failed to finish PNG: {}", e))?;
        // Reclaim the leaked Writer box.
        unsafe {
            let _ = Box::from_raw(self._writer_box);
        }
        Ok(())
    }
}

impl PageSink for PngSink {
    fn begin_page(&mut self, width: u32, height: u32) -> Result<(), String> {
        let writer = PngStreamWriter::new(&self.output_path, width, height, height)?;
        self.writer = Some(writer);
        Ok(())
    }

    fn write_rows(&mut self, rgba_rows: &[u8], _num_rows: u32) -> Result<(), String> {
        let writer = self
            .writer
            .as_mut()
            .ok_or("PngSink: begin_page not called")?;
        writer.write_all(rgba_rows)
    }

    fn end_page(&mut self) -> Result<(), String> {
        let writer = self.writer.take().ok_or("PngSink: begin_page not called")?;
        writer.finish()
    }
}
