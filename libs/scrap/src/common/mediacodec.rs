use hbb_common::{anyhow::Error, bail, log, ResultType};
use ndk::media::{
    media_codec::{MediaCodec, MediaCodecDirection, MediaFormat},
    NdkMediaError,
};
use std::ops::Deref;
use std::{
    io::Write,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use crate::ImageFormat;
use crate::{CodecFormat, I420ToABGR, I420ToARGB, ImageRgb};

/// MediaCodec mime type name
const H264_MIME_TYPE: &str = "video/avc";
const H265_MIME_TYPE: &str = "video/hevc";
const VP9_MIME_TYPE: &str = "video/x-vnd.on2.vp9";
const AV1_MIME_TYPE: &str = "video/av01";

// TODO MediaCodecEncoder

pub static H264_DECODER_SUPPORT: AtomicBool = AtomicBool::new(false);
pub static H265_DECODER_SUPPORT: AtomicBool = AtomicBool::new(false);

// Info codes from AMediaCodec_dequeueOutputBuffer. The ndk crate maps these
// negative codes to NdkMediaError and does not re-export the constants.
const AMEDIACODEC_INFO_OUTPUT_FORMAT_CHANGED: i32 = -2;
const AMEDIACODEC_INFO_OUTPUT_BUFFERS_CHANGED: i32 = -3;

// OMX_COLOR_FormatYUV420Planar, the only layout convert_output handles.
const COLOR_FORMAT_YUV420_PLANAR: i32 = 19;

pub struct MediaCodecDecoder {
    decoder: MediaCodec,
}

impl Deref for MediaCodecDecoder {
    type Target = MediaCodec;

    fn deref(&self) -> &Self::Target {
        &self.decoder
    }
}

pub struct MediaCodecDecoders {
    pub h264: Option<MediaCodecDecoder>,
    pub h265: Option<MediaCodecDecoder>,
    pub vp9: Option<MediaCodecDecoder>,
    pub av1: Option<MediaCodecDecoder>,
}

impl MediaCodecDecoder {
    pub fn new_decoders() -> MediaCodecDecoders {
        // Capability probe only: a default size every codec supports. Real
        // sessions configure with the peer display's dimensions.
        MediaCodecDecoders {
            h264: MediaCodecDecoder::new_with_size(CodecFormat::H264, 1280, 720),
            h265: MediaCodecDecoder::new_with_size(CodecFormat::H265, 1280, 720),
            vp9: MediaCodecDecoder::new_with_size(CodecFormat::VP9, 1280, 720),
            av1: MediaCodecDecoder::new_with_size(CodecFormat::AV1, 1280, 720),
        }
    }

    pub fn new_with_size(format: CodecFormat, width: i32, height: i32) -> Option<MediaCodecDecoder> {
        match format {
            CodecFormat::H264 => {
                create_media_codec(H264_MIME_TYPE, MediaCodecDirection::Decoder, width, height)
            }
            CodecFormat::H265 => {
                create_media_codec(H265_MIME_TYPE, MediaCodecDirection::Decoder, width, height)
            }
            CodecFormat::VP9 => {
                create_media_codec(VP9_MIME_TYPE, MediaCodecDirection::Decoder, width, height)
            }
            CodecFormat::AV1 => {
                create_media_codec(AV1_MIME_TYPE, MediaCodecDirection::Decoder, width, height)
            }
            _ => {
                log::error!("Unsupported codec format: {:?}", format);
                None
            }
        }
    }

    // rgb [in/out] fmt must be set; w, h and raw are written.
    // Ok(true) = a frame was decoded into rgb; Ok(false) = no output was
    // available this call (decoder warm-up or no new frame).
    pub fn decode(&mut self, data: &[u8], rgb: &mut ImageRgb) -> ResultType<bool> {
        match self.dequeue_input_buffer(Duration::from_millis(10))? {
            Some(mut input_buffer) => {
                let mut buf = input_buffer.buffer_mut();
                if data.len() > buf.len() {
                    // InputBuffer has no Drop: it is only recycled by
                    // queue_input_buffer, so return it or it is lost and the
                    // decoder stalls after a few oversized frames.
                    if let Err(e) = self.queue_input_buffer(input_buffer, 0, 0, 0, 0) {
                        log::debug!("Failed to recycle oversized input buffer: {e}");
                    }
                    bail!("The input data size is bigger than input buf");
                }
                buf.write_all(&data)?;
                self.queue_input_buffer(input_buffer, 0, data.len(), 0, 0)?;
            }
            None => {
                log::trace!("No available input buffer");
            }
        };

        let output_buffer = match self.dequeue_output_buffer(Duration::from_millis(100)) {
            Ok(Some(output_buffer)) => output_buffer,
            Ok(None) => return Ok(false),
            Err(err) => {
                // c2 reports INFO_OUTPUT_FORMAT_CHANGED (-2) after the first
                // input buffer and INFO_OUTPUT_BUFFERS_CHANGED (-3) on buffer
                // reallocation; both mean no output this dequeue.
                if matches!(
                    &err,
                    NdkMediaError::UnknownResult(m)
                        if m.0 == AMEDIACODEC_INFO_OUTPUT_FORMAT_CHANGED
                            || m.0 == AMEDIACODEC_INFO_OUTPUT_BUFFERS_CHANGED
                ) {
                    return Ok(false);
                }
                return Err(err.into());
            }
        };

        let res = Self::convert_output(output_buffer.buffer(), &self.output_format(), rgb);
        // OutputBuffer has no Drop: release even when the conversion failed.
        self.release_output_buffer(output_buffer, false)?;
        res?;
        Ok(true)
    }

    // The plane geometry comes from the codec's output format and every read
    // is bounds-checked: vendor decoders do not always pack tightly, and the
    // format can be re-keyed to a new resolution between the dequeue and the
    // format query (the check turns that race into a clean error, not UB).
    fn convert_output(buf: &[u8], format: &MediaFormat, rgb: &mut ImageRgb) -> ResultType<()> {
        if let Some(fmt) = format.i32("color-format") {
            if fmt != COLOR_FORMAT_YUV420_PLANAR {
                bail!("Unsupported decoder color format: {}", fmt);
            }
        }
        let w = format
            .i32("width")
            .ok_or(Error::msg("width missing in output format"))?
            as usize;
        let h = format
            .i32("height")
            .ok_or(Error::msg("height missing in output format"))?
            as usize;
        let stride = format
            .i32("stride")
            .ok_or(Error::msg("stride missing in output format"))?
            as usize;
        let slice_h = format.i32("slice-height").unwrap_or(h as i32) as usize;
        if w == 0 || h == 0 || slice_h < h || stride < w || stride % 2 != 0 {
            bail!(
                "Invalid decoder output geometry: {}x{}, stride {}, slice-height {}",
                w,
                h,
                stride,
                slice_h
            );
        }
        let (y_size, chroma_size) = (
            stride * slice_h,
            // libyuv processes ceil(h/2) chroma rows, not floor
            stride / 2 * ((slice_h + 1) / 2),
        );
        if y_size + 2 * chroma_size > buf.len() {
            bail!(
                "Decoder output buffer too small: {} needed, {} available",
                y_size + 2 * chroma_size,
                buf.len()
            );
        }
        let bps = 4;
        rgb.w = w;
        rgb.h = h;
        let dst_align = rgb.align();
        let bytes_per_row = (rgb.w * bps + dst_align - 1) & !(dst_align - 1);
        rgb.raw.resize(rgb.h * bytes_per_row, 0);
        let u = y_size;
        let v = y_size + chroma_size;
        let u_ptr = buf[u..].as_ptr();
        let v_ptr = buf[v..].as_ptr();
        unsafe {
            match rgb.fmt() {
                ImageFormat::ARGB => {
                    I420ToARGB(
                        buf.as_ptr(),
                        stride as _,
                        u_ptr,
                        (stride / 2) as _,
                        v_ptr,
                        (stride / 2) as _,
                        rgb.raw.as_mut_ptr(),
                        bytes_per_row as _,
                        w as _,
                        h as _,
                    );
                }
                ImageFormat::ABGR => {
                    I420ToABGR(
                        buf.as_ptr(),
                        stride as _,
                        u_ptr,
                        (stride / 2) as _,
                        v_ptr,
                        (stride / 2) as _,
                        rgb.raw.as_mut_ptr(),
                        bytes_per_row as _,
                        w as _,
                        h as _,
                    );
                }
                _ => {
                    bail!("Unsupported image format");
                }
            }
        }
        Ok(())
    }
}

fn create_media_codec(
    name: &str,
    direction: MediaCodecDirection,
    width: i32,
    height: i32,
) -> Option<MediaCodecDecoder> {
    let codec = MediaCodec::from_decoder_type(name)?;
    let media_format = MediaFormat::new();
    media_format.set_str("mime", name);
    // MediaCodec (c2) refuses a 0x0 configure; the caller passes the peer
    // display's dimensions as the initial hint. The decoder's real output
    // dimensions are read from the output format on every frame, and c2
    // reconfigures itself when the stream's actual resolution differs.
    media_format.set_i32("width", width);
    media_format.set_i32("height", height);
    media_format.set_i32("color-format", COLOR_FORMAT_YUV420_PLANAR);
    if let Err(e) = codec.configure(&media_format, None, direction) {
        log::error!("Failed to init decoder: {:?}", e);
        return None;
    };
    log::info!("decoder init success");
    if let Err(e) = codec.start() {
        log::error!("Failed to start decoder: {:?}", e);
        return None;
    };
    log::debug!("Init decoder succeeded!: {:?}", name);
    return Some(MediaCodecDecoder {
        decoder: codec,
    });
}

pub fn check_mediacodec() {
    std::thread::spawn(move || {
        // check decoders
        let decoders = MediaCodecDecoder::new_decoders();
        H264_DECODER_SUPPORT.swap(decoders.h264.is_some(), Ordering::SeqCst);
        H265_DECODER_SUPPORT.swap(decoders.h265.is_some(), Ordering::SeqCst);
        decoders.h264.map(|d| d.stop());
        decoders.h265.map(|d| d.stop());
        decoders.vp9.map(|d| d.stop());
        decoders.av1.map(|d| d.stop());
        // TODO encoders
    });
}
