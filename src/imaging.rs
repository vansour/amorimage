use std::io::Cursor;

use image::codecs::{jpeg::JpegEncoder, webp::WebPDecoder};
use image::{DynamicImage, ImageFormat, ImageReader};

pub struct ProcessedImage {
    /// 落盘存储的原始文件内容（GIF 与动图为上传原件，静图已重编码去元数据）
    pub bytes: Vec<u8>,
    pub ext: &'static str,
    pub mime: &'static str,
    pub width: u32,
    pub height: u32,
    pub thumb: Vec<u8>,
}

const THUMB_EDGE: u32 = 420;
const JPEG_QUALITY: u8 = 90;
const WEBP_QUALITY: f32 = 85.0;
const THUMB_QUALITY: f32 = 80.0;

#[derive(Debug, thiserror::Error)]
pub enum ImagingError {
    #[error("仅支持 PNG / JPEG / GIF / WebP 格式")]
    Unsupported,
    #[error("无法解码为有效图片: {0}")]
    Decode(String),
    #[error("图片编码失败: {0}")]
    Encode(String),
}

pub fn process(bytes: &[u8]) -> Result<ProcessedImage, ImagingError> {
    let reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| ImagingError::Decode(e.to_string()))?;
    let Some(format) = reader.format() else {
        return Err(ImagingError::Unsupported);
    };
    let (ext, mime) = match format {
        ImageFormat::Png => ("png", "image/png"),
        ImageFormat::Jpeg => ("jpg", "image/jpeg"),
        ImageFormat::Gif => ("gif", "image/gif"),
        ImageFormat::WebP => ("webp", "image/webp"),
        _ => return Err(ImagingError::Unsupported),
    };

    let img = image::load_from_memory_with_format(bytes, format)
        .map_err(|e| ImagingError::Decode(e.to_string()))?;
    let (width, height) = (img.width(), img.height());
    if width == 0 || height == 0 {
        return Err(ImagingError::Decode("图片尺寸为 0".into()));
    }

    // GIF 一律保留原件：重编码会丢动画，逐帧解码还可能被恶意帧描述符
    // 声明的超大尺寸放大内存；动画 WebP 同样保留，静图走重编码去元数据
    let keep_original = match format {
        ImageFormat::Gif => true,
        ImageFormat::WebP => WebPDecoder::new(Cursor::new(bytes))
            .map(|d| d.has_animation())
            .unwrap_or(true),
        _ => false,
    };
    let stored = if keep_original {
        bytes.to_vec()
    } else {
        reencode(&img, format)?
    };

    let thumb_img = img.thumbnail(THUMB_EDGE, THUMB_EDGE);
    let thumb = webp_encode(&thumb_img, THUMB_QUALITY);
    Ok(ProcessedImage { bytes: stored, ext, mime, width, height, thumb })
}

fn reencode(img: &DynamicImage, format: ImageFormat) -> Result<Vec<u8>, ImagingError> {
    let to_string = |e: image::ImageError| ImagingError::Encode(e.to_string());
    match format {
        ImageFormat::Png => {
            let mut out = Vec::new();
            img.write_to(&mut Cursor::new(&mut out), ImageFormat::Png)
                .map_err(to_string)?;
            Ok(out)
        }
        ImageFormat::Jpeg => {
            let mut out = Vec::new();
            img.write_with_encoder(JpegEncoder::new_with_quality(
                &mut Cursor::new(&mut out),
                JPEG_QUALITY,
            ))
            .map_err(to_string)?;
            Ok(out)
        }
        ImageFormat::WebP => Ok(webp_encode(img, WEBP_QUALITY)),
        _ => Err(ImagingError::Unsupported),
    }
}

fn webp_encode(img: &DynamicImage, quality: f32) -> Vec<u8> {
    // webp 0.3.1 的 Encoder 只接受 Rgb8/Rgba8；灰度与 16 位等其余色型先转成 RGBA
    let mem = match img {
        DynamicImage::ImageRgb8(rgb) => {
            webp::Encoder::from_rgb(rgb.as_raw(), rgb.width(), rgb.height()).encode(quality)
        }
        DynamicImage::ImageRgba8(rgba) => {
            webp::Encoder::from_rgba(rgba.as_raw(), rgba.width(), rgba.height()).encode(quality)
        }
        other => {
            let rgba = other.to_rgba8();
            webp::Encoder::from_rgba(rgba.as_raw(), rgba.width(), rgba.height()).encode(quality)
        }
    };
    mem.to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode(format: ImageFormat, img: &DynamicImage) -> Vec<u8> {
        let mut buf = Cursor::new(Vec::new());
        img.write_to(&mut buf, format).unwrap();
        buf.into_inner()
    }

    #[test]
    fn grayscale_png_stores_and_thumbs() {
        let img =
            DynamicImage::ImageLuma8(image::GrayImage::from_pixel(6, 6, image::Luma([9u8])));
        let png = encode(ImageFormat::Png, &img);
        let out = process(&png).unwrap();
        assert_eq!(out.ext, "png");
        assert!(!out.thumb.is_empty());
    }

    #[test]
    fn rgb16_png_stores_and_thumbs() {
        let img = DynamicImage::ImageRgb16(image::ImageBuffer::from_pixel(
            6,
            6,
            image::Rgb([100u16, 200, 300]),
        ));
        let png = encode(ImageFormat::Png, &img);
        let out = process(&png).unwrap();
        assert_eq!(out.ext, "png");
        assert!(!out.thumb.is_empty());
    }

    #[test]
    fn static_gif_is_kept_as_original() {
        let img = DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            6,
            6,
            image::Rgba([10u8, 20, 30, 255]),
        ));
        let gif = encode(ImageFormat::Gif, &img);
        let out = process(&gif).unwrap();
        assert_eq!(out.ext, "gif");
        assert_eq!(out.bytes, gif);
    }

    #[test]
    fn gif_with_oversized_second_frame_is_not_decoded() {
        // 帧 1 合法；帧 2 描述符声明 65535x65535——逐帧解码会在读到像素数据
        // 之前就请求约 17GB 的帧缓冲，这里必须整体保留原件而不是逐帧解码
        let img = DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            4,
            4,
            image::Rgba([1u8, 2, 3, 255]),
        ));
        let mut gif = encode(ImageFormat::Gif, &img);
        if gif.last() == Some(&0x3B) {
            gif.pop(); // 去掉 trailer，把伪造描述符插到帧数据之后
        }
        gif.extend_from_slice(&[
            0x2C, 0x00, 0x00, 0x00, 0x00, // 图像分隔符 + left/top = 0
            0xFF, 0xFF, 0xFF, 0xFF, // width/height = 65535
            0x00, // packed: 无局部色彩表
            0x02, // LZW 最小码长
            0x01, 0x00, // 数据子块（1 字节）+ 子块结束
        ]);
        gif.push(0x3B);
        let out = process(&gif).unwrap();
        assert_eq!(out.ext, "gif");
        assert_eq!(out.bytes, gif);
    }
}
