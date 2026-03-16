// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! PDF file writer — manages indirect objects, xref table, and file output.

use std::io::Write;

use flate2::Compression;
use flate2::write::ZlibEncoder;

use crate::pdf_objects::PdfObj;

/// Manages PDF indirect objects and writes complete PDF files.
pub struct PdfWriter {
    /// Serialized bytes for each object, indexed by object number.
    /// Index 0 is unused (PDF object numbers start at 1).
    objects: Vec<Option<Vec<u8>>>,
    next_obj: u32,
}

impl PdfWriter {
    pub fn new() -> Self {
        Self {
            objects: vec![None], // index 0 unused
            next_obj: 1,
        }
    }

    /// Allocate an object number without setting its content yet.
    pub fn alloc_obj(&mut self) -> u32 {
        let n = self.next_obj;
        self.next_obj += 1;
        // Ensure the vector is large enough
        while self.objects.len() <= n as usize {
            self.objects.push(None);
        }
        n
    }

    /// Set the content of a previously allocated object.
    pub fn set_object(&mut self, num: u32, obj: &PdfObj) {
        let mut buf = Vec::new();
        obj.write_to(&mut buf);
        while self.objects.len() <= num as usize {
            self.objects.push(None);
        }
        self.objects[num as usize] = Some(buf);
    }

    /// Add a new object and return its number.
    pub fn add_object(&mut self, obj: &PdfObj) -> u32 {
        let n = self.alloc_obj();
        self.set_object(n, obj);
        n
    }

    /// Add a stream object with optional flate compression.
    pub fn add_stream(
        &mut self,
        dict_entries: Vec<(Vec<u8>, PdfObj)>,
        data: &[u8],
        compress: bool,
    ) -> u32 {
        let n = self.alloc_obj();
        self.set_stream(n, dict_entries, data, compress);
        n
    }

    /// Set the content of a previously allocated stream object.
    pub fn set_stream(
        &mut self,
        num: u32,
        mut dict_entries: Vec<(Vec<u8>, PdfObj)>,
        data: &[u8],
        compress: bool,
    ) {
        let (final_data, filter) = if compress {
            let mut enc = ZlibEncoder::new(Vec::new(), Compression::fast());
            enc.write_all(data).unwrap();
            let compressed = enc.finish().unwrap();
            if compressed.len() < data.len() {
                (compressed, true)
            } else {
                (data.to_vec(), false)
            }
        } else {
            (data.to_vec(), false)
        };

        dict_entries.push((b"Length".to_vec(), PdfObj::Int(final_data.len() as i64)));
        if filter {
            dict_entries.push((b"Filter".to_vec(), PdfObj::name("FlateDecode")));
        }

        let mut buf = Vec::new();
        let dict = PdfObj::Dict(dict_entries);
        dict.write_to(&mut buf);
        buf.extend(b"\nstream\n");
        buf.extend(&final_data);
        buf.extend(b"\nendstream");

        while self.objects.len() <= num as usize {
            self.objects.push(None);
        }
        self.objects[num as usize] = Some(buf);
    }

    /// Write the complete PDF to a writer.
    pub fn write_pdf<W: Write>(
        &self,
        w: &mut W,
        catalog_ref: u32,
        info_ref: Option<u32>,
    ) -> std::io::Result<()> {
        let mut offset: usize = 0;
        let mut offsets: Vec<(u32, usize)> = Vec::new();

        // Header
        let header = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n";
        w.write_all(header)?;
        offset += header.len();

        // Objects
        for (i, obj_bytes) in self.objects.iter().enumerate() {
            if i == 0 {
                continue;
            }
            if let Some(bytes) = obj_bytes {
                offsets.push((i as u32, offset));
                let obj_header = format!("{} 0 obj\n", i);
                w.write_all(obj_header.as_bytes())?;
                offset += obj_header.len();
                w.write_all(bytes)?;
                offset += bytes.len();
                let footer = b"\nendobj\n";
                w.write_all(footer)?;
                offset += footer.len();
            }
        }

        // Cross-reference table
        let xref_offset = offset;
        let max_obj = offsets.iter().map(|(n, _)| *n).max().unwrap_or(0);
        let xref_header = format!("xref\n0 {}\n", max_obj + 1);
        w.write_all(xref_header.as_bytes())?;

        // Build offset map
        let mut offset_map: Vec<Option<usize>> = vec![None; (max_obj + 1) as usize];
        for &(num, off) in &offsets {
            offset_map[num as usize] = Some(off);
        }

        // Entry for object 0 (free)
        // Each xref entry must be exactly 20 bytes: 10+SP+5+SP+f/n+CR+LF
        w.write_all(b"0000000000 65535 f\r\n")?;
        for i in 1..=max_obj {
            if let Some(off) = offset_map[i as usize] {
                write!(w, "{:010} {:05} n\r\n", off, 0)?;
            } else {
                w.write_all(b"0000000000 00000 f\r\n")?;
            }
        }

        // Trailer
        let info_entry = match info_ref {
            Some(r) => format!(" /Info {} 0 R", r),
            None => String::new(),
        };
        write!(
            w,
            "trailer\n<</Size {} /Root {} 0 R{}>>\nstartxref\n{}\n%%EOF\n",
            max_obj + 1,
            catalog_ref,
            info_entry,
            xref_offset
        )?;

        Ok(())
    }
}
