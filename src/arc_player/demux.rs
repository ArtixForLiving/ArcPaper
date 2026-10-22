//! Minimalist pure Rust MP4 (ISOBMFF) parser and demuxer.
//!
//! Extracts video sample tables and bitstreams without heavy external dependencies,
//! converting AVCC/HVCC length-prefixed NAL units into Annex-B start codes inside
//! a reusable pre-allocated buffer with zero per-frame heap allocations.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use super::loop_ctrl::LoopController;

/// Supported video bitstream codecs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoCodec {
    H264,
    Hevc,
}

/// Metadata entry for a single compressed video frame.
#[derive(Debug, Clone, Copy)]
pub struct FrameEntry {
    pub offset: u64,
    pub size: u32,
    pub duration_100ns: i64,
    pub is_keyframe: bool,
}

/// Sample-to-Chunk entry mapping.
#[derive(Debug, Clone, Copy)]
struct StscEntry {
    first_chunk: u32,
    samples_per_chunk: u32,
}

/// Box header metadata.
#[derive(Debug, Clone, Copy)]
struct BoxHeader {
    box_type: [u8; 4],
    header_size: u64,
    total_size: u64,
    payload_offset: u64,
}

/// Minimalist MP4 demuxer optimized for wallpaper playback.
pub struct Demuxer {
    file: Option<File>,
    codec: Option<VideoCodec>,
    width: u32,
    height: u32,
    timescale: u32,
    nal_length_size: usize,
    codec_header_annexb: Vec<u8>,
    frames: Vec<FrameEntry>,
    packet_buffer: Vec<u8>,
    raw_buffer: Vec<u8>,
}

impl Demuxer {
    /// Instantiates a new Demuxer with a 2 MB pre-allocated packet buffer.
    pub fn new() -> Self {
        Self {
            file: None,
            codec: None,
            width: 0,
            height: 0,
            timescale: 0,
            nal_length_size: 4,
            codec_header_annexb: Vec::with_capacity(512),
            frames: Vec::new(),
            packet_buffer: Vec::with_capacity(2 * 1024 * 1024),
            raw_buffer: Vec::with_capacity(2 * 1024 * 1024),
        }
    }

    /// Opens an MP4 file, parses the ISOBMFF box hierarchy, and indexes the sample table.
    pub fn open<P: AsRef<Path>>(&mut self, path: P) -> std::io::Result<()> {
        let p = path.as_ref().to_path_buf();
        let mut file = File::open(&p)?;
        let file_len = file.metadata()?.len();

        self.frames.clear();
        self.codec_header_annexb.clear();
        self.codec = None;
        self.width = 0;
        self.height = 0;
        self.timescale = 0;
        self.nal_length_size = 4;

        // Parse top-level boxes to locate `moov`
        let mut cur_offset = 0;
        let mut moov_header = None;

        while cur_offset + 8 <= file_len {
            file.seek(SeekFrom::Start(cur_offset))?;
            let header = match Self::read_box_header(&mut file, file_len)? {
                Some(h) => h,
                None => break,
            };

            if header.total_size == 0 {
                break;
            }

            if &header.box_type == b"moov" {
                moov_header = Some(header);
                break;
            }

            cur_offset = cur_offset.saturating_add(header.total_size);
        }

        let moov = moov_header.ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "MP4 does not contain a 'moov' box",
            )
        })?;

        // Parse `moov` children looking for video `trak`
        self.parse_moov(&mut file, moov)?;

        if self.frames.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "No video frames found in MP4 file",
            ));
        }

        self.file = Some(file);
        Ok(())
    }

    fn read_box_header<R: Read + Seek>(
        reader: &mut R,
        container_end: u64,
    ) -> std::io::Result<Option<BoxHeader>> {
        let cur_pos = reader.stream_position()?;
        if cur_pos + 8 > container_end {
            return Ok(None);
        }

        let mut buf = [0u8; 8];
        if reader.read_exact(&mut buf).is_err() {
            return Ok(None);
        }

        let size32 = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);
        let box_type = [buf[4], buf[5], buf[6], buf[7]];

        let (total_size, header_size) = if size32 == 1 {
            let mut ext = [0u8; 8];
            reader.read_exact(&mut ext)?;
            (u64::from_be_bytes(ext), 16)
        } else if size32 == 0 {
            (container_end - cur_pos, 8)
        } else {
            (size32 as u64, 8)
        };

        if total_size < header_size {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Invalid box size",
            ));
        }

        Ok(Some(BoxHeader {
            box_type,
            header_size,
            total_size,
            payload_offset: cur_pos + header_size,
        }))
    }

    fn parse_moov(&mut self, file: &mut File, moov: BoxHeader) -> std::io::Result<()> {
        let moov_end = moov.payload_offset + (moov.total_size - moov.header_size);
        let mut cur = moov.payload_offset;

        while cur + 8 <= moov_end {
            file.seek(SeekFrom::Start(cur))?;
            let header = match Self::read_box_header(file, moov_end)? {
                Some(h) => h,
                None => break,
            };
            if header.total_size == 0 {
                break;
            }

            if &header.box_type == b"trak" && self.try_parse_video_track(file, header)? {
                // Successfully found and parsed the video track
                break;
            }

            cur = cur.saturating_add(header.total_size);
        }

        Ok(())
    }

    fn try_parse_video_track(&mut self, file: &mut File, trak: BoxHeader) -> std::io::Result<bool> {
        let trak_end = trak.payload_offset + (trak.total_size - trak.header_size);
        let mut cur = trak.payload_offset;
        let mut mdia_header = None;

        while cur + 8 <= trak_end {
            file.seek(SeekFrom::Start(cur))?;
            let header = match Self::read_box_header(file, trak_end)? {
                Some(h) => h,
                None => break,
            };
            if header.total_size == 0 {
                break;
            }

            if &header.box_type == b"mdia" {
                mdia_header = Some(header);
                break;
            }

            cur = cur.saturating_add(header.total_size);
        }

        let mdia = match mdia_header {
            Some(m) => m,
            None => return Ok(false),
        };

        let mdia_end = mdia.payload_offset + (mdia.total_size - mdia.header_size);
        cur = mdia.payload_offset;

        let mut is_video = false;
        let mut minf_header = None;

        while cur + 8 <= mdia_end {
            file.seek(SeekFrom::Start(cur))?;
            let header = match Self::read_box_header(file, mdia_end)? {
                Some(h) => h,
                None => break,
            };
            if header.total_size == 0 {
                break;
            }

            if &header.box_type == b"hdlr" {
                // Check if handler is 'vide'
                let payload_len = (header.total_size - header.header_size) as usize;
                let mut hdlr_bytes = vec![0u8; payload_len.min(24)];
                file.read_exact(&mut hdlr_bytes)?;
                if hdlr_bytes.len() >= 12 && &hdlr_bytes[8..12] == b"vide" {
                    is_video = true;
                }
            } else if &header.box_type == b"mdhd" {
                let payload_len = (header.total_size - header.header_size) as usize;
                let mut mdhd_bytes = vec![0u8; payload_len.min(32)];
                file.read_exact(&mut mdhd_bytes)?;
                if !mdhd_bytes.is_empty() {
                    let version = mdhd_bytes[0];
                    if version == 0 && mdhd_bytes.len() >= 16 {
                        self.timescale = u32::from_be_bytes([
                            mdhd_bytes[12],
                            mdhd_bytes[13],
                            mdhd_bytes[14],
                            mdhd_bytes[15],
                        ]);
                    } else if version == 1 && mdhd_bytes.len() >= 24 {
                        self.timescale = u32::from_be_bytes([
                            mdhd_bytes[20],
                            mdhd_bytes[21],
                            mdhd_bytes[22],
                            mdhd_bytes[23],
                        ]);
                    }
                }
            } else if &header.box_type == b"minf" {
                minf_header = Some(header);
            }

            cur = cur.saturating_add(header.total_size);
        }

        if !is_video {
            return Ok(false);
        }

        let minf = match minf_header {
            Some(m) => m,
            None => return Ok(false),
        };

        // Find `stbl` inside `minf`
        let minf_end = minf.payload_offset + (minf.total_size - minf.header_size);
        cur = minf.payload_offset;
        let mut stbl_header = None;

        while cur + 8 <= minf_end {
            file.seek(SeekFrom::Start(cur))?;
            let header = match Self::read_box_header(file, minf_end)? {
                Some(h) => h,
                None => break,
            };
            if header.total_size == 0 {
                break;
            }

            if &header.box_type == b"stbl" {
                stbl_header = Some(header);
                break;
            }

            cur = cur.saturating_add(header.total_size);
        }

        let stbl = match stbl_header {
            Some(s) => s,
            None => return Ok(false),
        };

        self.parse_stbl(file, stbl)?;
        Ok(true)
    }

    fn parse_stbl(&mut self, file: &mut File, stbl: BoxHeader) -> std::io::Result<()> {
        let stbl_end = stbl.payload_offset + (stbl.total_size - stbl.header_size);
        let mut cur = stbl.payload_offset;

        let mut raw_stsd = Vec::new();
        let mut raw_stts = Vec::new();
        let mut raw_stsz = Vec::new();
        let mut raw_stsc = Vec::new();
        let mut raw_stco = Vec::new();
        let mut raw_co64 = Vec::new();
        let mut raw_stss = Vec::new();

        while cur + 8 <= stbl_end {
            file.seek(SeekFrom::Start(cur))?;
            let header = match Self::read_box_header(file, stbl_end)? {
                Some(h) => h,
                None => break,
            };
            if header.total_size == 0 {
                break;
            }

            let payload_len = (header.total_size - header.header_size) as usize;

            match &header.box_type {
                b"stsd" => {
                    raw_stsd.resize(payload_len, 0);
                    file.read_exact(&mut raw_stsd)?;
                }
                b"stts" => {
                    raw_stts.resize(payload_len, 0);
                    file.read_exact(&mut raw_stts)?;
                }
                b"stsz" => {
                    raw_stsz.resize(payload_len, 0);
                    file.read_exact(&mut raw_stsz)?;
                }
                b"stsc" => {
                    raw_stsc.resize(payload_len, 0);
                    file.read_exact(&mut raw_stsc)?;
                }
                b"stco" => {
                    raw_stco.resize(payload_len, 0);
                    file.read_exact(&mut raw_stco)?;
                }
                b"co64" => {
                    raw_co64.resize(payload_len, 0);
                    file.read_exact(&mut raw_co64)?;
                }
                b"stss" => {
                    raw_stss.resize(payload_len, 0);
                    file.read_exact(&mut raw_stss)?;
                }
                _ => {}
            }

            cur = cur.saturating_add(header.total_size);
        }

        // 1. Parse `stsd`
        self.parse_stsd_payload(&raw_stsd)?;

        // 2. Parse `stts` -> Durations
        let sample_durations = self.parse_stts_payload(&raw_stts);

        // 3. Parse `stsz` -> Sizes
        let sample_sizes = self.parse_stsz_payload(&raw_stsz);

        // 4. Parse `stsc` -> Chunks map
        let stsc_entries = self.parse_stsc_payload(&raw_stsc);

        // 5. Parse `stco` or `co64` -> Chunk byte offsets
        let chunk_offsets = if !raw_co64.is_empty() {
            self.parse_co64_payload(&raw_co64)
        } else {
            self.parse_stco_payload(&raw_stco)
        };

        // 6. Parse `stss` -> Sync/Keyframe sample numbers (1-based)
        let sync_samples = self.parse_stss_payload(&raw_stss);
        let has_stss = !raw_stss.is_empty();

        // 7. Combine tables into flat FrameEntry array
        self.build_frame_entries(
            sample_sizes,
            sample_durations,
            stsc_entries,
            chunk_offsets,
            sync_samples,
            has_stss,
        );

        Ok(())
    }

    fn parse_stsd_payload(&mut self, data: &[u8]) -> std::io::Result<()> {
        if data.len() < 8 {
            return Ok(());
        }
        let entry_count = u32::from_be_bytes([data[4], data[5], data[6], data[7]]) as usize;
        let mut pos = 8;

        for _ in 0..entry_count {
            if pos + 8 > data.len() {
                break;
            }
            let entry_size =
                u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]])
                    as usize;
            let format = &data[pos + 4..pos + 8];

            if format == b"avc1" {
                self.codec = Some(VideoCodec::H264);
            } else if format == b"hev1" || format == b"hvc1" {
                self.codec = Some(VideoCodec::Hevc);
            }

            // VisualSampleEntry has width at offset +32, height at offset +34
            if pos + 36 <= data.len() {
                self.width = u16::from_be_bytes([data[pos + 32], data[pos + 33]]) as u32;
                self.height = u16::from_be_bytes([data[pos + 34], data[pos + 35]]) as u32;
            }

            // Sub-boxes start after 86 bytes
            let sub_start = pos + 86;
            let sub_end = (pos + entry_size).min(data.len());
            let mut sub_pos = sub_start;

            while sub_pos + 8 <= sub_end {
                let box_size = u32::from_be_bytes([
                    data[sub_pos],
                    data[sub_pos + 1],
                    data[sub_pos + 2],
                    data[sub_pos + 3],
                ]) as usize;
                let box_type = &data[sub_pos + 4..sub_pos + 8];

                if box_size < 8 || sub_pos + box_size > sub_end {
                    break;
                }

                let payload = &data[sub_pos + 8..sub_pos + box_size];

                if box_type == b"avcC" {
                    self.parse_avcc(payload);
                } else if box_type == b"hvcC" {
                    self.parse_hvcc(payload);
                }

                sub_pos += box_size;
            }

            pos += entry_size;
        }

        Ok(())
    }

    fn parse_avcc(&mut self, data: &[u8]) {
        if data.len() < 7 {
            return;
        }
        self.nal_length_size = (data[4] & 0x03) as usize + 1;
        let num_sps = (data[5] & 0x1F) as usize;
        let mut pos = 6;

        for _ in 0..num_sps {
            if pos + 2 > data.len() {
                break;
            }
            let sps_len = u16::from_be_bytes([data[pos], data[pos + 1]]) as usize;
            pos += 2;
            if pos + sps_len > data.len() {
                break;
            }
            self.codec_header_annexb
                .extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
            self.codec_header_annexb
                .extend_from_slice(&data[pos..pos + sps_len]);
            pos += sps_len;
        }

        if pos >= data.len() {
            return;
        }

        let num_pps = data[pos] as usize;
        pos += 1;

        for _ in 0..num_pps {
            if pos + 2 > data.len() {
                break;
            }
            let pps_len = u16::from_be_bytes([data[pos], data[pos + 1]]) as usize;
            pos += 2;
            if pos + pps_len > data.len() {
                break;
            }
            self.codec_header_annexb
                .extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
            self.codec_header_annexb
                .extend_from_slice(&data[pos..pos + pps_len]);
            pos += pps_len;
        }
    }

    fn parse_hvcc(&mut self, data: &[u8]) {
        if data.len() < 23 {
            return;
        }
        self.nal_length_size = (data[21] & 0x03) as usize + 1;
        let num_arrays = data[22] as usize;
        let mut pos = 23;

        for _ in 0..num_arrays {
            if pos + 3 > data.len() {
                break;
            }
            pos += 1; // array completeness & NAL unit type
            let num_nalus = u16::from_be_bytes([data[pos], data[pos + 1]]) as usize;
            pos += 2;

            for _ in 0..num_nalus {
                if pos + 2 > data.len() {
                    break;
                }
                let nalu_len = u16::from_be_bytes([data[pos], data[pos + 1]]) as usize;
                pos += 2;
                if pos + nalu_len > data.len() {
                    break;
                }
                self.codec_header_annexb
                    .extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
                self.codec_header_annexb
                    .extend_from_slice(&data[pos..pos + nalu_len]);
                pos += nalu_len;
            }
        }
    }

    fn parse_stts_payload(&self, data: &[u8]) -> Vec<i64> {
        let mut durations = Vec::new();
        if data.len() < 8 {
            return durations;
        }
        let entry_count = u32::from_be_bytes([data[4], data[5], data[6], data[7]]) as usize;
        let mut pos = 8;
        let ts = self.timescale.max(1) as u64;

        for _ in 0..entry_count {
            if pos + 8 > data.len() {
                break;
            }
            let count = u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]])
                as usize;
            let delta =
                u32::from_be_bytes([data[pos + 4], data[pos + 5], data[pos + 6], data[pos + 7]])
                    as u64;
            pos += 8;

            let dur_100ns = (delta * 10_000_000 / ts) as i64;
            durations.resize(durations.len() + count, dur_100ns);
        }

        durations
    }

    fn parse_stsz_payload(&self, data: &[u8]) -> Vec<u32> {
        let mut sizes = Vec::new();
        if data.len() < 12 {
            return sizes;
        }
        let uniform_size = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
        let sample_count = u32::from_be_bytes([data[8], data[9], data[10], data[11]]) as usize;

        if uniform_size > 0 {
            sizes.resize(sample_count, uniform_size);
        } else {
            let mut pos = 12;
            for _ in 0..sample_count {
                if pos + 4 > data.len() {
                    break;
                }
                let s =
                    u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]);
                sizes.push(s);
                pos += 4;
            }
        }

        sizes
    }

    fn parse_stsc_payload(&self, data: &[u8]) -> Vec<StscEntry> {
        let mut entries = Vec::new();
        if data.len() < 8 {
            return entries;
        }
        let entry_count = u32::from_be_bytes([data[4], data[5], data[6], data[7]]) as usize;
        let mut pos = 8;

        for _ in 0..entry_count {
            if pos + 12 > data.len() {
                break;
            }
            let first_chunk =
                u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]);
            let samples_per_chunk =
                u32::from_be_bytes([data[pos + 4], data[pos + 5], data[pos + 6], data[pos + 7]]);
            pos += 12;

            entries.push(StscEntry {
                first_chunk,
                samples_per_chunk,
            });
        }

        entries
    }

    fn parse_stco_payload(&self, data: &[u8]) -> Vec<u64> {
        let mut offsets = Vec::new();
        if data.len() < 8 {
            return offsets;
        }
        let entry_count = u32::from_be_bytes([data[4], data[5], data[6], data[7]]) as usize;
        let mut pos = 8;

        for _ in 0..entry_count {
            if pos + 4 > data.len() {
                break;
            }
            let off =
                u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]) as u64;
            offsets.push(off);
            pos += 4;
        }

        offsets
    }

    fn parse_co64_payload(&self, data: &[u8]) -> Vec<u64> {
        let mut offsets = Vec::new();
        if data.len() < 8 {
            return offsets;
        }
        let entry_count = u32::from_be_bytes([data[4], data[5], data[6], data[7]]) as usize;
        let mut pos = 8;

        for _ in 0..entry_count {
            if pos + 8 > data.len() {
                break;
            }
            let off = u64::from_be_bytes([
                data[pos],
                data[pos + 1],
                data[pos + 2],
                data[pos + 3],
                data[pos + 4],
                data[pos + 5],
                data[pos + 6],
                data[pos + 7],
            ]);
            offsets.push(off);
            pos += 8;
        }

        offsets
    }

    fn parse_stss_payload(&self, data: &[u8]) -> Vec<u32> {
        let mut sync = Vec::new();
        if data.len() < 8 {
            return sync;
        }
        let entry_count = u32::from_be_bytes([data[4], data[5], data[6], data[7]]) as usize;
        let mut pos = 8;

        for _ in 0..entry_count {
            if pos + 4 > data.len() {
                break;
            }
            let s = u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]);
            sync.push(s);
            pos += 4;
        }

        sync.sort_unstable();
        sync
    }

    fn build_frame_entries(
        &mut self,
        sample_sizes: Vec<u32>,
        sample_durations: Vec<i64>,
        stsc_entries: Vec<StscEntry>,
        chunk_offsets: Vec<u64>,
        sync_samples: Vec<u32>,
        has_stss: bool,
    ) {
        let num_samples = sample_sizes.len();
        let num_chunks = chunk_offsets.len();
        let mut sample_offsets = Vec::with_capacity(num_samples);

        let mut stsc_idx = 0;
        let mut sample_idx = 0;

        for chunk_idx in 1..=num_chunks {
            if stsc_idx + 1 < stsc_entries.len()
                && (chunk_idx as u32) >= stsc_entries[stsc_idx + 1].first_chunk
            {
                stsc_idx += 1;
            }

            let samples_in_chunk = if stsc_idx < stsc_entries.len() {
                stsc_entries[stsc_idx].samples_per_chunk as usize
            } else {
                1
            };

            let mut cur_chunk_offset = chunk_offsets[chunk_idx - 1];

            for _ in 0..samples_in_chunk {
                if sample_idx >= num_samples {
                    break;
                }
                sample_offsets.push(cur_chunk_offset);
                cur_chunk_offset += sample_sizes[sample_idx] as u64;
                sample_idx += 1;
            }
        }

        let fallback_dur = if !sample_durations.is_empty() {
            sample_durations[0]
        } else {
            166_666 // ~60fps
        };

        self.frames.reserve(sample_sizes.len());
        for (i, &size) in sample_sizes.iter().enumerate() {
            let offset = sample_offsets.get(i).copied().unwrap_or(0);
            let duration_100ns = sample_durations.get(i).copied().unwrap_or(fallback_dur);
            let is_keyframe = if has_stss {
                sync_samples.binary_search(&(i as u32 + 1)).is_ok()
            } else {
                true
            };

            self.frames.push(FrameEntry {
                offset,
                size,
                duration_100ns,
                is_keyframe,
            });
        }
    }

    /// Reads the next video packet, converts NAL prefixes to Annex-B start codes,
    /// and prepends SPS/PPS headers on keyframes or loop wrap boundaries.
    ///
    /// Borrows from the pre-allocated internal `packet_buffer` with zero dynamic allocations.
    pub fn read_next_annexb_packet(
        &mut self,
        loop_ctrl: &mut LoopController,
    ) -> Result<(&[u8], i64, i64, bool), String> {
        if self.frames.is_empty() {
            return Err("No video frames available in demuxer".to_string());
        }

        let file = self.file.as_mut().ok_or("No video file open")?;

        let dur = self.frames[loop_ctrl.current_frame_idx].duration_100ns;
        let (frame_idx, pts_100ns, wrapped) = loop_ctrl.advance(dur);
        let entry = self.frames[frame_idx];

        let raw_len = entry.size as usize;
        if self.raw_buffer.len() < raw_len {
            self.raw_buffer.resize(raw_len, 0);
        }

        file.seek(SeekFrom::Start(entry.offset)).map_err(|e| {
            format!(
                "Failed to seek to frame {frame_idx} offset {}: {e}",
                entry.offset
            )
        })?;
        file.read_exact(&mut self.raw_buffer[..raw_len])
            .map_err(|e| format!("Failed to read frame {frame_idx} (len {raw_len}): {e}"))?;

        self.packet_buffer.clear();

        // On keyframes or when wrapping around at the loop boundary, prepend SPS/PPS/VPS header
        if (entry.is_keyframe || wrapped) && !self.codec_header_annexb.is_empty() {
            self.packet_buffer
                .extend_from_slice(&self.codec_header_annexb);
        }

        let nals = &self.raw_buffer[..raw_len];
        let mut offset = 0;
        let nal_len_size = if self.nal_length_size > 0 {
            self.nal_length_size
        } else {
            4
        };

        while offset + nal_len_size <= nals.len() {
            let nalu_size = match nal_len_size {
                4 => u32::from_be_bytes([
                    nals[offset],
                    nals[offset + 1],
                    nals[offset + 2],
                    nals[offset + 3],
                ]) as usize,
                2 => u16::from_be_bytes([nals[offset], nals[offset + 1]]) as usize,
                1 => nals[offset] as usize,
                _ => 0,
            };
            offset += nal_len_size;

            if nalu_size == 0 {
                continue;
            }

            if offset + nalu_size > nals.len() {
                self.packet_buffer
                    .extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
                self.packet_buffer.extend_from_slice(&nals[offset..]);
                break;
            }

            self.packet_buffer
                .extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
            self.packet_buffer
                .extend_from_slice(&nals[offset..offset + nalu_size]);
            offset += nalu_size;
        }

        Ok((
            &self.packet_buffer[..],
            pts_100ns,
            entry.duration_100ns,
            entry.is_keyframe,
        ))
    }

    pub fn codec(&self) -> Option<VideoCodec> {
        self.codec
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    pub fn frames(&self) -> &[FrameEntry] {
        &self.frames
    }
}

impl Default for Demuxer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_real_video_and_loop() {
        let test_path = Path::new(r"D:\Arc Paper\SavedWallpapers\cyanex-in-warframe.1920x1080.mp4");
        if !test_path.exists() {
            println!("Test wallpaper not present, skipping real file test.");
            return;
        }

        let mut demuxer = Demuxer::new();
        demuxer.open(test_path).expect("Failed to open test MP4");

        assert!(demuxer.width() > 0);
        assert!(demuxer.height() > 0);
        assert!(demuxer.codec().is_some());
        assert!(demuxer.frame_count() > 0);

        let total_frames = demuxer.frame_count();
        let mut loop_ctrl = LoopController::new(total_frames);

        let mut prev_pts = -1;
        // Read through 2 full loops without error
        for i in 0..(total_frames * 2) {
            let (packet, pts, dur, is_key) = demuxer
                .read_next_annexb_packet(&mut loop_ctrl)
                .expect("Failed reading packet");

            assert!(!packet.is_empty());
            assert!(pts >= prev_pts, "PTS must be strictly monotonic!");
            assert!(dur > 0);

            // First 4 bytes must be Annex-B start code
            assert_eq!(&packet[0..4], &[0x00, 0x00, 0x00, 0x01]);

            if i % total_frames == 0 {
                assert!(is_key, "Frame 0 must be marked keyframe");
            }
            prev_pts = pts;
        }
    }
}
