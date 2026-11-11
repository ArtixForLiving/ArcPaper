//! Minimalist pure Rust MP4 (ISOBMFF) parser and demuxer.
//!
//! Extracts video sample tables and bitstreams without heavy external dependencies,
//! converting AVCC/HVCC length-prefixed NAL units into Annex-B start codes inside
//! a reusable pre-allocated buffer with zero per-frame heap allocations.

use std::fs::File;
use std::io::{Error, ErrorKind, Read, Seek, SeekFrom};
use std::path::Path;

use super::loop_ctrl::LoopController;

const MAX_METADATA_BOX_BYTES: usize = 32 * 1024 * 1024;
const MAX_SAMPLES: usize = 2_000_000;
const MAX_STSD_ENTRIES: usize = 64;
const MAX_PACKET_BYTES: usize = 32 * 1024 * 1024;

/// Combined seekable stream trait allowing `Demuxer` to read files or in-memory buffers.
pub trait ReadSeek: Read + Seek + Send {}
impl<T: Read + Seek + Send> ReadSeek for T {}

fn invalid_data(msg: impl Into<String>) -> std::io::Error {
    Error::new(ErrorKind::InvalidData, msg.into())
}

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
    reader: Option<Box<dyn ReadSeek>>,
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
            reader: None,
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

    /// Opens an MP4 file on disk, parses the ISOBMFF box hierarchy, and indexes the sample table.
    pub fn open<P: AsRef<Path>>(&mut self, path: P) -> std::io::Result<()> {
        let p = path.as_ref().to_path_buf();
        let file = File::open(&p)?;
        let file_len = file.metadata()?.len();
        self.open_reader(file, file_len)
    }

    /// Parses an MP4 stream from any seekable reader bounded by `file_len`.
    pub fn open_reader<R: Read + Seek + Send + 'static>(
        &mut self,
        mut reader: R,
        file_len: u64,
    ) -> std::io::Result<()> {
        self.reader = None;
        self.frames.clear();
        self.codec_header_annexb.clear();
        self.codec = None;
        self.width = 0;
        self.height = 0;
        self.timescale = 0;
        self.nal_length_size = 4;

        // Parse top-level boxes to locate `moov`
        let mut cur_offset = 0u64;
        let mut moov_header = None;

        while cur_offset.saturating_add(8) <= file_len {
            reader.seek(SeekFrom::Start(cur_offset))?;
            let header = match Self::read_box_header(&mut reader, file_len)? {
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

            cur_offset = cur_offset
                .checked_add(header.total_size)
                .ok_or_else(|| invalid_data("Top-level MP4 box offset overflow"))?;
        }

        let moov = moov_header.ok_or_else(|| invalid_data("MP4 does not contain a 'moov' box"))?;

        // Parse `moov` children looking for video `trak`
        self.parse_moov(&mut reader, moov, file_len)?;

        if self.frames.is_empty() {
            return Err(invalid_data("No video frames found in MP4 stream"));
        }
        if self.codec.is_none() {
            return Err(invalid_data("Unsupported or missing video codec in MP4 stream"));
        }

        self.reader = Some(Box::new(reader));
        Ok(())
    }

    fn read_box_header<R: Read + Seek>(
        reader: &mut R,
        container_end: u64,
    ) -> std::io::Result<Option<BoxHeader>> {
        let cur_pos = reader.stream_position()?;
        if cur_pos.saturating_add(8) > container_end {
            return Ok(None);
        }

        let mut buf = [0u8; 8];
        if reader.read_exact(&mut buf).is_err() {
            return Ok(None);
        }

        let size32 = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);
        let box_type = [buf[4], buf[5], buf[6], buf[7]];

        let (total_size, header_size) = if size32 == 1 {
            if cur_pos.saturating_add(16) > container_end {
                return Err(invalid_data("Truncated 64-bit MP4 box header"));
            }
            let mut ext = [0u8; 8];
            reader.read_exact(&mut ext)?;
            (u64::from_be_bytes(ext), 16u64)
        } else if size32 == 0 {
            (container_end - cur_pos, 8u64)
        } else {
            (size32 as u64, 8u64)
        };

        if total_size < header_size {
            return Err(invalid_data("Invalid box size smaller than header"));
        }

        let box_end = cur_pos
            .checked_add(total_size)
            .ok_or_else(|| invalid_data("MP4 box end offset overflow"))?;
        if box_end > container_end {
            return Err(invalid_data("MP4 box extends beyond container boundary"));
        }

        Ok(Some(BoxHeader {
            box_type,
            header_size,
            total_size,
            payload_offset: cur_pos + header_size,
        }))
    }

    fn box_end(header: BoxHeader) -> std::io::Result<u64> {
        header
            .payload_offset
            .checked_add(header.total_size - header.header_size)
            .ok_or_else(|| invalid_data("Box payload end overflow"))
    }

    fn parse_moov<R: Read + Seek>(
        &mut self,
        reader: &mut R,
        moov: BoxHeader,
        file_len: u64,
    ) -> std::io::Result<()> {
        let moov_end = Self::box_end(moov)?.min(file_len);
        let mut cur = moov.payload_offset;

        while cur.saturating_add(8) <= moov_end {
            reader.seek(SeekFrom::Start(cur))?;
            let header = match Self::read_box_header(reader, moov_end)? {
                Some(h) => h,
                None => break,
            };
            if header.total_size == 0 {
                break;
            }

            if &header.box_type == b"trak"
                && self.try_parse_video_track(reader, header, file_len)?
            {
                // Successfully found and parsed the video track
                break;
            }

            cur = cur
                .checked_add(header.total_size)
                .ok_or_else(|| invalid_data("moov child box offset overflow"))?;
        }

        Ok(())
    }

    fn try_parse_video_track<R: Read + Seek>(
        &mut self,
        reader: &mut R,
        trak: BoxHeader,
        file_len: u64,
    ) -> std::io::Result<bool> {
        let trak_end = Self::box_end(trak)?.min(file_len);
        let mut cur = trak.payload_offset;
        let mut mdia_header = None;

        while cur.saturating_add(8) <= trak_end {
            reader.seek(SeekFrom::Start(cur))?;
            let header = match Self::read_box_header(reader, trak_end)? {
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

            cur = cur
                .checked_add(header.total_size)
                .ok_or_else(|| invalid_data("trak child box offset overflow"))?;
        }

        let mdia = match mdia_header {
            Some(m) => m,
            None => return Ok(false),
        };

        let mdia_end = Self::box_end(mdia)?.min(file_len);
        cur = mdia.payload_offset;

        let mut is_video = false;
        let mut minf_header = None;

        while cur.saturating_add(8) <= mdia_end {
            reader.seek(SeekFrom::Start(cur))?;
            let header = match Self::read_box_header(reader, mdia_end)? {
                Some(h) => h,
                None => break,
            };
            if header.total_size == 0 {
                break;
            }

            if &header.box_type == b"hdlr" {
                let payload_len = (header.total_size - header.header_size).min(24) as usize;
                let mut hdlr_bytes = vec![0u8; payload_len];
                reader.read_exact(&mut hdlr_bytes)?;
                if hdlr_bytes.len() >= 12 && &hdlr_bytes[8..12] == b"vide" {
                    is_video = true;
                }
            } else if &header.box_type == b"mdhd" {
                let payload_len = (header.total_size - header.header_size).min(32) as usize;
                let mut mdhd_bytes = vec![0u8; payload_len];
                reader.read_exact(&mut mdhd_bytes)?;
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

            cur = cur
                .checked_add(header.total_size)
                .ok_or_else(|| invalid_data("mdia child box offset overflow"))?;
        }

        if !is_video {
            return Ok(false);
        }

        let minf = match minf_header {
            Some(m) => m,
            None => return Ok(false),
        };

        let minf_end = Self::box_end(minf)?.min(file_len);
        cur = minf.payload_offset;
        let mut stbl_header = None;

        while cur.saturating_add(8) <= minf_end {
            reader.seek(SeekFrom::Start(cur))?;
            let header = match Self::read_box_header(reader, minf_end)? {
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

            cur = cur
                .checked_add(header.total_size)
                .ok_or_else(|| invalid_data("minf child box offset overflow"))?;
        }

        let stbl = match stbl_header {
            Some(s) => s,
            None => return Ok(false),
        };

        self.parse_stbl(reader, stbl, file_len)?;
        Ok(true)
    }

    fn read_bounded_box_payload<R: Read + Seek>(
        reader: &mut R,
        header: BoxHeader,
        container_end: u64,
    ) -> std::io::Result<Vec<u8>> {
        let payload_u64 = header.total_size - header.header_size;
        if payload_u64 > MAX_METADATA_BOX_BYTES as u64 {
            return Err(invalid_data("MP4 metadata box exceeds safety size limit"));
        }
        if header.payload_offset.saturating_add(payload_u64) > container_end {
            return Err(invalid_data("MP4 metadata box extends beyond container"));
        }
        let mut buf = vec![0u8; payload_u64 as usize];
        reader.read_exact(&mut buf)?;
        Ok(buf)
    }

    fn parse_stbl<R: Read + Seek>(
        &mut self,
        reader: &mut R,
        stbl: BoxHeader,
        file_len: u64,
    ) -> std::io::Result<()> {
        let stbl_end = Self::box_end(stbl)?.min(file_len);
        let mut cur = stbl.payload_offset;

        let mut raw_stsd = Vec::new();
        let mut raw_stts = Vec::new();
        let mut raw_stsz = Vec::new();
        let mut raw_stsc = Vec::new();
        let mut raw_stco = Vec::new();
        let mut raw_co64 = Vec::new();
        let mut raw_stss = Vec::new();

        while cur.saturating_add(8) <= stbl_end {
            reader.seek(SeekFrom::Start(cur))?;
            let header = match Self::read_box_header(reader, stbl_end)? {
                Some(h) => h,
                None => break,
            };
            if header.total_size == 0 {
                break;
            }

            match &header.box_type {
                b"stsd" => raw_stsd = Self::read_bounded_box_payload(reader, header, stbl_end)?,
                b"stts" => raw_stts = Self::read_bounded_box_payload(reader, header, stbl_end)?,
                b"stsz" => raw_stsz = Self::read_bounded_box_payload(reader, header, stbl_end)?,
                b"stsc" => raw_stsc = Self::read_bounded_box_payload(reader, header, stbl_end)?,
                b"stco" => raw_stco = Self::read_bounded_box_payload(reader, header, stbl_end)?,
                b"co64" => raw_co64 = Self::read_bounded_box_payload(reader, header, stbl_end)?,
                b"stss" => raw_stss = Self::read_bounded_box_payload(reader, header, stbl_end)?,
                _ => {}
            }

            cur = cur
                .checked_add(header.total_size)
                .ok_or_else(|| invalid_data("stbl child box offset overflow"))?;
        }

        // 1. Parse `stsd`
        self.parse_stsd_payload(&raw_stsd)?;

        // 2. Parse `stts` -> Durations
        let sample_durations = self.parse_stts_payload(&raw_stts)?;

        // 3. Parse `stsz` -> Sizes
        let sample_sizes = self.parse_stsz_payload(&raw_stsz)?;

        // 4. Parse `stsc` -> Chunks map
        let stsc_entries = self.parse_stsc_payload(&raw_stsc)?;

        // 5. Parse `stco` or `co64` -> Chunk byte offsets
        let chunk_offsets = if !raw_co64.is_empty() {
            self.parse_co64_payload(&raw_co64)?
        } else {
            self.parse_stco_payload(&raw_stco)?
        };

        // 6. Parse `stss` -> Sync/Keyframe sample numbers (1-based)
        let sync_samples = self.parse_stss_payload(&raw_stss)?;
        let has_stss = !raw_stss.is_empty();

        // 7. Combine tables into flat FrameEntry array
        self.build_frame_entries(
            sample_sizes,
            sample_durations,
            stsc_entries,
            chunk_offsets,
            sync_samples,
            has_stss,
            file_len,
        )?;

        Ok(())
    }

    fn parse_stsd_payload(&mut self, data: &[u8]) -> std::io::Result<()> {
        if data.len() < 8 {
            return Err(invalid_data("Truncated stsd box"));
        }
        let entry_count = u32::from_be_bytes([data[4], data[5], data[6], data[7]]) as usize;
        if entry_count > MAX_STSD_ENTRIES {
            return Err(invalid_data("stsd entry_count exceeds safety limit"));
        }
        let mut pos = 8usize;

        for _ in 0..entry_count {
            if pos.saturating_add(8) > data.len() {
                return Err(invalid_data("Truncated stsd sample entry header"));
            }
            let entry_size =
                u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]])
                    as usize;
            if entry_size < 8 {
                return Err(invalid_data("Invalid stsd sample entry size (< 8)"));
            }
            let sub_end = pos
                .checked_add(entry_size)
                .ok_or_else(|| invalid_data("stsd sample entry size overflow"))?;
            if sub_end > data.len() {
                return Err(invalid_data("stsd sample entry extends past box end"));
            }

            let format = &data[pos + 4..pos + 8];
            if format == b"avc1" {
                self.codec = Some(VideoCodec::H264);
            } else if format == b"hev1" || format == b"hvc1" {
                self.codec = Some(VideoCodec::Hevc);
            }

            // VisualSampleEntry has width at offset +32, height at offset +34
            if pos + 36 <= sub_end {
                self.width = u16::from_be_bytes([data[pos + 32], data[pos + 33]]) as u32;
                self.height = u16::from_be_bytes([data[pos + 34], data[pos + 35]]) as u32;
            }

            // Sub-boxes start after 86 bytes
            if let Some(sub_start) = pos.checked_add(86)
                && sub_start <= sub_end
            {
                let mut sub_pos = sub_start;
                while sub_pos.saturating_add(8) <= sub_end {
                    let box_size = u32::from_be_bytes([
                        data[sub_pos],
                        data[sub_pos + 1],
                        data[sub_pos + 2],
                        data[sub_pos + 3],
                    ]) as usize;
                    let box_type = &data[sub_pos + 4..sub_pos + 8];

                    if box_size < 8 {
                        return Err(invalid_data("Invalid stsd sub-box size (< 8)"));
                    }
                    let next_sub = sub_pos
                        .checked_add(box_size)
                        .ok_or_else(|| invalid_data("stsd sub-box overflow"))?;
                    if next_sub > sub_end {
                        return Err(invalid_data("stsd sub-box extends past sample entry"));
                    }

                    let payload = &data[sub_pos + 8..next_sub];
                    if box_type == b"avcC" {
                        self.parse_avcc(payload);
                    } else if box_type == b"hvcC" {
                        self.parse_hvcc(payload);
                    }

                    sub_pos = next_sub;
                }
            }

            pos = sub_end;
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

    fn parse_stts_payload(&self, data: &[u8]) -> std::io::Result<Vec<i64>> {
        let mut durations = Vec::new();
        if data.len() < 8 {
            return Err(invalid_data("Truncated stts box"));
        }
        let entry_count = u32::from_be_bytes([data[4], data[5], data[6], data[7]]) as usize;
        if entry_count > (data.len() - 8) / 8 {
            return Err(invalid_data("stts entry_count exceeds box payload length"));
        }
        let mut pos = 8;
        let ts = self.timescale.max(1) as u64;

        for _ in 0..entry_count {
            let count = u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]])
                as usize;
            let delta =
                u32::from_be_bytes([data[pos + 4], data[pos + 5], data[pos + 6], data[pos + 7]])
                    as u64;
            pos += 8;

            let new_len = durations
                .len()
                .checked_add(count)
                .filter(|&n| n <= MAX_SAMPLES)
                .ok_or_else(|| invalid_data("stts sample count exceeds safety limit"))?;
            let dur_100ns = delta
                .saturating_mul(10_000_000)
                .checked_div(ts)
                .unwrap_or(166_666)
                .min(i64::MAX as u64) as i64;
            durations.resize(new_len, dur_100ns);
        }

        Ok(durations)
    }

    fn parse_stsz_payload(&self, data: &[u8]) -> std::io::Result<Vec<u32>> {
        if data.len() < 12 {
            return Err(invalid_data("Truncated stsz box"));
        }
        let uniform_size = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
        let sample_count = u32::from_be_bytes([data[8], data[9], data[10], data[11]]) as usize;
        if sample_count > MAX_SAMPLES {
            return Err(invalid_data("stsz sample_count exceeds safety limit"));
        }

        if uniform_size > 0 {
            if (uniform_size as usize) > MAX_PACKET_BYTES {
                return Err(invalid_data("stsz uniform sample size exceeds limit"));
            }
            return Ok(vec![uniform_size; sample_count]);
        }

        if sample_count > (data.len() - 12) / 4 {
            return Err(invalid_data("stsz sample_count exceeds box payload length"));
        }

        let mut sizes = Vec::with_capacity(sample_count);
        let mut pos = 12;
        for _ in 0..sample_count {
            let s = u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]);
            if (s as usize) > MAX_PACKET_BYTES {
                return Err(invalid_data("stsz sample size exceeds safety limit"));
            }
            sizes.push(s);
            pos += 4;
        }

        Ok(sizes)
    }

    fn parse_stsc_payload(&self, data: &[u8]) -> std::io::Result<Vec<StscEntry>> {
        if data.len() < 8 {
            return Err(invalid_data("Truncated stsc box"));
        }
        let entry_count = u32::from_be_bytes([data[4], data[5], data[6], data[7]]) as usize;
        if entry_count > (data.len() - 8) / 12 || entry_count > MAX_SAMPLES {
            return Err(invalid_data("stsc entry_count exceeds bounds"));
        }

        let mut entries = Vec::with_capacity(entry_count);
        let mut pos = 8;
        for _ in 0..entry_count {
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

        Ok(entries)
    }

    fn parse_stco_payload(&self, data: &[u8]) -> std::io::Result<Vec<u64>> {
        if data.len() < 8 {
            return Err(invalid_data("Truncated stco box"));
        }
        let entry_count = u32::from_be_bytes([data[4], data[5], data[6], data[7]]) as usize;
        if entry_count > (data.len() - 8) / 4 || entry_count > MAX_SAMPLES {
            return Err(invalid_data("stco entry_count exceeds bounds"));
        }

        let mut offsets = Vec::with_capacity(entry_count);
        let mut pos = 8;
        for _ in 0..entry_count {
            let off =
                u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]) as u64;
            offsets.push(off);
            pos += 4;
        }

        Ok(offsets)
    }

    fn parse_co64_payload(&self, data: &[u8]) -> std::io::Result<Vec<u64>> {
        if data.len() < 8 {
            return Err(invalid_data("Truncated co64 box"));
        }
        let entry_count = u32::from_be_bytes([data[4], data[5], data[6], data[7]]) as usize;
        if entry_count > (data.len() - 8) / 8 || entry_count > MAX_SAMPLES {
            return Err(invalid_data("co64 entry_count exceeds bounds"));
        }

        let mut offsets = Vec::with_capacity(entry_count);
        let mut pos = 8;
        for _ in 0..entry_count {
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

        Ok(offsets)
    }

    fn parse_stss_payload(&self, data: &[u8]) -> std::io::Result<Vec<u32>> {
        if data.is_empty() {
            return Ok(Vec::new());
        }
        if data.len() < 8 {
            return Err(invalid_data("Truncated stss box"));
        }
        let entry_count = u32::from_be_bytes([data[4], data[5], data[6], data[7]]) as usize;
        if entry_count > (data.len() - 8) / 4 || entry_count > MAX_SAMPLES {
            return Err(invalid_data("stss entry_count exceeds bounds"));
        }

        let mut sync = Vec::with_capacity(entry_count);
        let mut pos = 8;
        for _ in 0..entry_count {
            let s = u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]);
            sync.push(s);
            pos += 4;
        }

        sync.sort_unstable();
        Ok(sync)
    }

    #[allow(clippy::too_many_arguments)]
    fn build_frame_entries(
        &mut self,
        sample_sizes: Vec<u32>,
        sample_durations: Vec<i64>,
        stsc_entries: Vec<StscEntry>,
        chunk_offsets: Vec<u64>,
        sync_samples: Vec<u32>,
        has_stss: bool,
        file_len: u64,
    ) -> std::io::Result<()> {
        let num_samples = sample_sizes.len();
        let num_chunks = chunk_offsets.len();
        if num_samples > 0 && num_chunks == 0 {
            return Err(invalid_data("MP4 has samples but no chunk offsets"));
        }
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
                let sample_size = sample_sizes[sample_idx] as u64;
                let sample_end = cur_chunk_offset
                    .checked_add(sample_size)
                    .ok_or_else(|| invalid_data("Sample byte range overflow"))?;
                if sample_end > file_len {
                    return Err(invalid_data("Sample byte range exceeds MP4 file length"));
                }
                sample_offsets.push(cur_chunk_offset);
                cur_chunk_offset = sample_end;
                sample_idx += 1;
            }
        }

        if sample_offsets.len() != num_samples {
            return Err(invalid_data(
                "Sample-to-chunk table does not cover all video samples",
            ));
        }

        let fallback_dur = if !sample_durations.is_empty() {
            sample_durations[0]
        } else {
            166_666 // ~60fps
        };

        self.frames.reserve(sample_sizes.len());
        for (i, &size) in sample_sizes.iter().enumerate() {
            let offset = sample_offsets[i];
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

        Ok(())
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
        if loop_ctrl.current_frame_idx >= self.frames.len() {
            return Err("LoopController frame index out of bounds".to_string());
        }

        let reader = self.reader.as_mut().ok_or("No video stream open")?;

        let dur = self.frames[loop_ctrl.current_frame_idx].duration_100ns;
        let (frame_idx, pts_100ns, wrapped) = loop_ctrl.advance(dur);
        let entry = self.frames[frame_idx];

        let raw_len = entry.size as usize;
        if raw_len > MAX_PACKET_BYTES {
            return Err(format!("Frame {frame_idx} exceeds max packet size limit"));
        }
        if self.raw_buffer.len() < raw_len {
            self.raw_buffer.resize(raw_len, 0);
        }

        reader.seek(SeekFrom::Start(entry.offset)).map_err(|e| {
            format!(
                "Failed to seek to frame {frame_idx} offset {}: {e}",
                entry.offset
            )
        })?;
        reader
            .read_exact(&mut self.raw_buffer[..raw_len])
            .map_err(|e| format!("Failed to read frame {frame_idx} (len {raw_len}): {e}"))?;

        self.packet_buffer.clear();

        // On keyframes or when wrapping around at the loop boundary, prepend SPS/PPS/VPS header
        if (entry.is_keyframe || wrapped) && !self.codec_header_annexb.is_empty() {
            self.packet_buffer
                .extend_from_slice(&self.codec_header_annexb);
        }

        let nals = &self.raw_buffer[..raw_len];
        let mut offset = 0usize;
        let nal_len_size = if self.nal_length_size > 0 {
            self.nal_length_size
        } else {
            4
        };

        while offset.saturating_add(nal_len_size) <= nals.len() {
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

            let Some(nal_end) = offset.checked_add(nalu_size) else {
                break;
            };
            if nal_end > nals.len() {
                self.packet_buffer
                    .extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
                self.packet_buffer.extend_from_slice(&nals[offset..]);
                break;
            }

            self.packet_buffer
                .extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
            self.packet_buffer
                .extend_from_slice(&nals[offset..nal_end]);
            offset = nal_end;
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

    fn box_bytes(tag: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let total = (8 + payload.len()) as u32;
        let mut out = Vec::with_capacity(total as usize);
        out.extend_from_slice(&total.to_be_bytes());
        out.extend_from_slice(tag);
        out.extend_from_slice(payload);
        out
    }

    fn full_box(tag: &[u8; 4], version: u8, flags: [u8; 3], payload: &[u8]) -> Vec<u8> {
        let mut p = Vec::with_capacity(4 + payload.len());
        p.push(version);
        p.extend_from_slice(&flags);
        p.extend_from_slice(payload);
        box_bytes(tag, &p)
    }

    fn build_synthetic_mp4(
        stsd_override: Option<Vec<u8>>,
        stts_override: Option<Vec<u8>>,
        stsz_override: Option<Vec<u8>>,
        stco_override: Option<Vec<u8>>,
    ) -> Vec<u8> {
        let ftyp = box_bytes(b"ftyp", b"isom\0\0\x02\0isomiso2avc1mp41");
        let mdat_payload = [
            0x00, 0x00, 0x00, 0x02, 0x65, 0x88, // frame 0 (6 bytes)
            0x00, 0x00, 0x00, 0x02, 0x41, 0x9A, // frame 1 (6 bytes)
        ];
        let mdat = box_bytes(b"mdat", &mdat_payload);
        let mdat_data_offset = (ftyp.len() + 8) as u32;

        // Default valid stsd with avc1 + avcC
        let stsd = stsd_override.unwrap_or_else(|| {
            let avcc = box_bytes(
                b"avcC",
                &[
                    1, 0x64, 0x00, 0x1F, 0xFF, // version, profile, compat, level, nal_len_size=4
                    0xE1, 0x00, 0x04, 0x67, 0x64, 0x00, 0x1F, // 1 SPS
                    0x01, 0x00, 0x02, 0x68, 0xEE, // 1 PPS
                ],
            );
            let mut avc1_payload = vec![0u8; 78];
            // width at +32 from box start => +24 in payload, height at +26 in payload
            avc1_payload[24..26].copy_from_slice(&320u16.to_be_bytes());
            avc1_payload[26..28].copy_from_slice(&180u16.to_be_bytes());
            avc1_payload.extend_from_slice(&avcc);
            let avc1 = box_bytes(b"avc1", &avc1_payload);
            let mut stsd_p = Vec::new();
            stsd_p.extend_from_slice(&1u32.to_be_bytes());
            stsd_p.extend_from_slice(&avc1);
            full_box(b"stsd", 0, [0; 3], &stsd_p)
        });

        let stts = stts_override.unwrap_or_else(|| {
            let mut p = Vec::new();
            p.extend_from_slice(&1u32.to_be_bytes());
            p.extend_from_slice(&2u32.to_be_bytes()); // count = 2
            p.extend_from_slice(&1000u32.to_be_bytes()); // delta = 1000
            full_box(b"stts", 0, [0; 3], &p)
        });

        let stsz = stsz_override.unwrap_or_else(|| {
            let mut p = Vec::new();
            p.extend_from_slice(&6u32.to_be_bytes()); // uniform sample size = 6
            p.extend_from_slice(&2u32.to_be_bytes()); // sample count = 2
            full_box(b"stsz", 0, [0; 3], &p)
        });

        let stsc = {
            let mut p = Vec::new();
            p.extend_from_slice(&1u32.to_be_bytes());
            p.extend_from_slice(&1u32.to_be_bytes());
            p.extend_from_slice(&2u32.to_be_bytes());
            p.extend_from_slice(&1u32.to_be_bytes());
            full_box(b"stsc", 0, [0; 3], &p)
        };

        let stco = stco_override.unwrap_or_else(|| {
            let mut p = Vec::new();
            p.extend_from_slice(&1u32.to_be_bytes());
            p.extend_from_slice(&mdat_data_offset.to_be_bytes());
            full_box(b"stco", 0, [0; 3], &p)
        });

        let stss = {
            let mut p = Vec::new();
            p.extend_from_slice(&1u32.to_be_bytes());
            p.extend_from_slice(&1u32.to_be_bytes());
            full_box(b"stss", 0, [0; 3], &p)
        };

        let mut stbl_p = Vec::new();
        stbl_p.extend_from_slice(&stsd);
        stbl_p.extend_from_slice(&stts);
        stbl_p.extend_from_slice(&stsz);
        stbl_p.extend_from_slice(&stsc);
        stbl_p.extend_from_slice(&stco);
        stbl_p.extend_from_slice(&stss);
        let stbl = box_bytes(b"stbl", &stbl_p);
        let minf = box_bytes(b"minf", &stbl);

        let mut hdlr_p = vec![0u8; 16];
        hdlr_p[4..8].copy_from_slice(b"vide");
        let hdlr = full_box(b"hdlr", 0, [0; 3], &hdlr_p);

        let mut mdhd_p = vec![0u8; 20];
        mdhd_p[8..12].copy_from_slice(&30_000u32.to_be_bytes()); // timescale = 30000
        let mdhd = full_box(b"mdhd", 0, [0; 3], &mdhd_p);

        let mut mdia_p = Vec::new();
        mdia_p.extend_from_slice(&mdhd);
        mdia_p.extend_from_slice(&hdlr);
        mdia_p.extend_from_slice(&minf);
        let mdia = box_bytes(b"mdia", &mdia_p);
        let trak = box_bytes(b"trak", &mdia);
        let moov = box_bytes(b"moov", &trak);

        let mut file = Vec::new();
        file.extend_from_slice(&ftyp);
        file.extend_from_slice(&mdat);
        file.extend_from_slice(&moov);
        file
    }

    fn write_temp_mp4(name: &str, data: &[u8]) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!(
            "arc_paper_demux_{name}_{}_{}.mp4",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::write(&p, data).unwrap();
        p
    }

    #[test]
    fn test_demux_valid_synthetic_mp4_and_loop() {
        let mp4 = build_synthetic_mp4(None, None, None, None);
        let path = write_temp_mp4("valid", &mp4);

        let mut demuxer = Demuxer::new();
        let mp4_len = mp4.len() as u64;
        demuxer
            .open_reader(std::io::Cursor::new(mp4), mp4_len)
            .expect("valid synthetic MP4 should open from in-memory Cursor");
        assert_eq!(demuxer.codec(), Some(VideoCodec::H264));
        assert_eq!(demuxer.width(), 320);
        assert_eq!(demuxer.height(), 180);
        assert_eq!(demuxer.frame_count(), 2);

        let mut ctrl = LoopController::new(2);
        for i in 0..4 {
            let (pkt, _pts, dur, is_key) = demuxer.read_next_annexb_packet(&mut ctrl).unwrap();
            assert_eq!(&pkt[..4], &[0x00, 0x00, 0x00, 0x01]);
            assert!(dur > 0);
            if i % 2 == 0 {
                assert!(is_key);
            }
        }

        // Also ensure file-based open path succeeds cleanly
        let mut file_demuxer = Demuxer::new();
        file_demuxer.open(&path).expect("file open should succeed");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_demux_rejects_oversized_box_header_without_oom() {
        // Box claiming 2GB total size inside a 16-byte stream
        let mut bad = Vec::new();
        bad.extend_from_slice(&0x7FFF_FFFFu32.to_be_bytes());
        bad.extend_from_slice(b"moov");
        bad.extend_from_slice(&[0u8; 8]);
        let len = bad.len() as u64;
        let mut demuxer = Demuxer::new();
        assert!(demuxer.open_reader(std::io::Cursor::new(bad), len).is_err());
    }

    #[test]
    fn test_demux_rejects_stts_count_overflow_without_oom() {
        let mut p = Vec::new();
        p.extend_from_slice(&1u32.to_be_bytes());
        p.extend_from_slice(&u32::MAX.to_be_bytes());
        p.extend_from_slice(&1000u32.to_be_bytes());
        let bad_stts = full_box(b"stts", 0, [0; 3], &p);

        let mp4 = build_synthetic_mp4(None, Some(bad_stts), None, None);
        let path = write_temp_mp4("bad_stts", &mp4);

        let mut demuxer = Demuxer::new();
        assert!(demuxer.open(&path).is_err(), "oversized stts count must return Err");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_demux_rejects_stsz_sample_count_overflow_without_oom() {
        let mut p = Vec::new();
        p.extend_from_slice(&16u32.to_be_bytes());
        p.extend_from_slice(&u32::MAX.to_be_bytes());
        let bad_stsz = full_box(b"stsz", 0, [0; 3], &p);

        let mp4 = build_synthetic_mp4(None, None, Some(bad_stsz), None);
        let path = write_temp_mp4("bad_stsz", &mp4);

        let mut demuxer = Demuxer::new();
        assert!(demuxer.open(&path).is_err(), "oversized stsz sample_count must return Err");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_demux_rejects_stsd_zero_entry_size() {
        let mut p = Vec::new();
        p.extend_from_slice(&2u32.to_be_bytes()); // entry_count = 2
        // Entry 0: minimal 8-byte zero-size malformed entry
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(b"avc1");
        let bad_stsd = full_box(b"stsd", 0, [0; 3], &p);

        let mp4 = build_synthetic_mp4(Some(bad_stsd), None, None, None);
        let path = write_temp_mp4("bad_stsd", &mp4);

        let mut demuxer = Demuxer::new();
        assert!(demuxer.open(&path).is_err(), "zero entry_size in stsd must return Err");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_demux_rejects_out_of_bounds_frame_offset() {
        let mut p = Vec::new();
        p.extend_from_slice(&1u32.to_be_bytes());
        p.extend_from_slice(&0x0FFF_FFFFu32.to_be_bytes());
        let bad_stco = full_box(b"stco", 0, [0; 3], &p);

        let mp4 = build_synthetic_mp4(None, None, None, Some(bad_stco));
        let path = write_temp_mp4("bad_stco", &mp4);

        let mut demuxer = Demuxer::new();
        assert!(demuxer.open(&path).is_err(), "out-of-bounds stco offset must return Err");
        let _ = std::fs::remove_file(&path);
    }

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
