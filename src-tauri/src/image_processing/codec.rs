use super::{Encoded, Format, Mode, Settings};
use image::{
    AnimationDecoder, DynamicImage, ImageDecoder, ImageEncoder, ImageFormat, ImageReader, Limits,
    codecs::{
        gif::GifDecoder,
        jpeg::JpegEncoder,
        png::{CompressionType, FilterType, PngEncoder},
        webp::WebPEncoder,
    },
};
use std::{io::Cursor, sync::Arc};

fn guarded_png(raw: &[u8]) -> bool {
    let Some(mut chunks) = raw.get(8..) else {
        return true;
    };
    while let Some((header, rest)) = chunks.split_at_checked(8) {
        let Some(n) = header
            .get(..4)
            .and_then(|v| v.try_into().ok())
            .map(u32::from_be_bytes)
        else {
            return true;
        };
        let kind = header.get(4..);
        if matches!(
            kind,
            Some(b"acTL" | b"iCCP" | b"eXIf" | b"cICP" | b"gAMA" | b"cHRM")
        ) {
            return true;
        }
        let Some(end) = usize::try_from(n).ok().and_then(|n| n.checked_add(4)) else {
            return true;
        };
        let Some(next) = rest.get(end..) else {
            return true;
        };
        if kind == Some(b"IEND") {
            return false;
        }
        chunks = next;
    }
    true
}

pub(super) fn encode(
    raw: &[u8],
    mime: &'static str,
    cfg: Settings,
    webp: bool,
) -> Result<Encoded, ()> {
    let fmt = image::guess_format(raw).map_err(|_| ())?;
    if fmt.to_mime_type() != mime {
        return Err(());
    }
    let reader = || ImageReader::with_format(Cursor::new(raw), fmt);
    let (w, h) = reader().into_dimensions().map_err(|_| ())?;
    if w == 0 || h == 0 || u64::from(w) * u64::from(h) > 40_000_000 {
        return Err(());
    }
    if raw.len() < cfg.min_bytes as usize
        && (cfg.mode == Mode::Lossless || w.max(h) <= cfg.max_side)
    {
        return Ok(Encoded::kept(mime, "threshold"));
    }
    if fmt == ImageFormat::Png && guarded_png(raw) {
        return Ok(Encoded::kept(mime, "preserve"));
    }
    if fmt == ImageFormat::WebP
        && image::codecs::webp::WebPDecoder::new(Cursor::new(raw))
            .map_err(|_| ())?
            .has_animation()
    {
        return Ok(Encoded::kept(mime, "preserve"));
    }
    let mut limit = Limits::default();
    limit.max_alloc = Some(192 * 1024 * 1024);
    let mut rd = reader();
    rd.limits(limit.clone());
    let mut dec = rd.into_decoder().map_err(|_| ())?;
    if !matches!(
        dec.color_type(),
        image::ColorType::Rgb8
            | image::ColorType::Rgba8
            | image::ColorType::L8
            | image::ColorType::La8
    ) || dec.icc_profile().map_err(|_| ())?.is_some()
        || dec.exif_metadata().map_err(|_| ())?.is_some()
    {
        return Ok(Encoded::kept(mime, "preserve"));
    }
    let mut img = if fmt == ImageFormat::Gif {
        let mut gif = GifDecoder::new(Cursor::new(raw)).map_err(|_| ())?;
        gif.set_limits(limit).map_err(|_| ())?;
        let mut frames = gif.into_frames();
        let first = frames.next().ok_or(())?.map_err(|_| ())?;
        if frames.next().is_some() {
            return Ok(Encoded::kept(mime, "preserve"));
        }
        DynamicImage::ImageRgba8(first.into_buffer())
    } else {
        DynamicImage::from_decoder(dec).map_err(|_| ())?
    };
    let lossless = cfg.mode == Mode::Lossless;
    let resized = !lossless && w.max(h) > cfg.max_side;
    if resized {
        img = img.resize(
            cfg.max_side,
            cfg.max_side,
            image::imageops::FilterType::Lanczos3,
        )
    }
    let mut out = Vec::new();
    let target = if lossless && webp {
        "image/webp"
    } else if lossless
        || cfg.format == Format::Png
        || (cfg.format == Format::Auto
            && img.color().has_alpha()
            && img.to_rgba8().pixels().any(|p| p.0[3] != 255))
    {
        "image/png"
    } else {
        "image/jpeg"
    };
    if target == "image/jpeg" {
        if img.color().has_alpha() {
            let mut rgba = img.into_rgba8();
            for p in rgba.pixels_mut() {
                let [r, g, b, a] = p.0;
                let blend = |v: u8| {
                    ((u32::from(v) * u32::from(a) + 255 * (255 - u32::from(a)) + 127) / 255) as u8
                };
                *p = image::Rgba([blend(r), blend(g), blend(b), 255]);
            }
            img = DynamicImage::ImageRgba8(rgba)
        }
        JpegEncoder::new_with_quality(&mut out, cfg.quality)
            .encode_image(&img.to_rgb8())
            .map_err(|_| ())?;
    } else if target == "image/webp" {
        let pixels = img.to_rgba8();
        WebPEncoder::new_lossless(&mut out)
            .write_image(
                pixels.as_raw(),
                img.width(),
                img.height(),
                image::ExtendedColorType::Rgba8,
            )
            .map_err(|_| ())?;
    } else {
        PngEncoder::new_with_quality(&mut out, CompressionType::Best, FilterType::Adaptive)
            .write_image(
                img.as_bytes(),
                img.width(),
                img.height(),
                img.color().into(),
            )
            .map_err(|_| ())?;
    }
    if out.len() >= raw.len() {
        return Ok(Encoded::kept(mime, "notSmaller"));
    }
    Ok(Encoded {
        mime: target,
        bytes: Some(Arc::from(out)),
        reason: if resized {
            "resized"
        } else if target == "image/jpeg" {
            "lossy"
        } else {
            "lossless"
        },
    })
}
